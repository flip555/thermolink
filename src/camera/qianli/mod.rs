//! Qianli / Sunshine SuperCam ("Neptune") driver.
//!
//! The handshake replays the captured vendor sequence one packet at a time.
//! That sequence is not optional decoration: it registers the client, probes
//! both media values, negotiates formats, announces each stream flag and runs
//! the settings probe. An abbreviated version is accepted politely and then
//! produces no frames at all.
//!
//! The only runtime substitution is the per-session flag from each probe reply.

pub mod handshake;
pub mod protocol;
pub mod reboot;
pub mod settings;
mod settings_write;
pub mod thermal;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};

use super::{
    Calibration, CameraDriver, CameraFrame, CameraSession, CameraSpec, FramePayload, StreamKind,
    ThermalImage, ThermalStream, VideoStream,
};
use crate::config::CameraConfig;
use protocol as p;
use thermal::{ThermalAssembler, HEIGHT, WIDTH};

pub const DRIVER_ID: &str = "qianli";
const KEEPALIVE_EVERY: Duration = Duration::from_secs(10);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(20);

pub struct QianliDriver {
    host: Ipv4Addr,
    control_port: u16,
    media_port: u16,
    calibration: Calibration,
    /// Last configuration block the camera reported, with its arrival time.
    /// The handshake itself carries one, so normal reads cost nothing.
    settings: Arc<Mutex<Option<(Instant, serde_json::Value)>>>,
    /// The live control socket that answered the settings probe. Probing it
    /// avoids opening a second session alongside the streaming one.
    settings_link: Arc<Mutex<Option<TcpStream>>>,
}

impl QianliDriver {
    pub fn from_config(cfg: &CameraConfig) -> Result<Self> {
        let host: Ipv4Addr = cfg
            .host
            .parse()
            .with_context(|| format!("qianli driver needs an IPv4 address, got {:?}", cfg.host))?;
        let calibration = cfg
            .options
            .get("calibration")
            .map(|v| Calibration {
                scale: v.get("scale").and_then(|x| x.as_f64()).unwrap_or(0.01) as f32,
                offset: v.get("offset").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
            })
            .unwrap_or_default();
        Ok(Self {
            host,
            control_port: cfg.control_port,
            media_port: cfg.media_port,
            calibration,
            settings: Arc::new(Mutex::new(None)),
            settings_link: Arc::new(Mutex::new(None)),
        })
    }

    fn spec_with(&self, calibration: Calibration) -> CameraSpec {
        CameraSpec {
            driver: DRIVER_ID,
            model: "Qianli SuperCam LC-AD11 / Sunshine LC-AD11 / TB-03 v2",
            video: Some(VideoStream {
                width: 1920,
                height: 1080,
                codec: "h264-annexb",
                fps_hint: 30.0,
            }),
            capabilities: super::Capabilities {
                thermal: true, visible: true, discover: true,
                network_settings: true, reboot: true,
                controls: vec!["network".to_string()],
            },
            thermal: Some(ThermalStream {
                width: WIDTH,
                height: HEIGHT,
                bit_depth: 16,
                calibration,
            }),
        }
    }
}

#[async_trait]
impl CameraDriver for QianliDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn spec(&self) -> CameraSpec {
        self.spec_with(self.calibration)
    }

    async fn reboot(&self) -> Result<serde_json::Value> {
        reboot::reboot(self.host).await
    }

    async fn read_settings(&self) -> Result<Option<serde_json::Value>> {
        // A live control socket already in config mode answers the probe
        // without opening a second session against a streaming camera.
        let link = self.settings_link.lock().await.take();
        if let Some(mut s) = link {
            match collect_settings(&mut s, true, Duration::from_secs(1)).await {
                Ok(Some(v)) => {
                    *self.settings.lock().await = Some((Instant::now(), v.clone()));
                    *self.settings_link.lock().await = Some(s);
                    return Ok(Some(v));
                }
                Ok(None) => {
                    *self.settings_link.lock().await = Some(s);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "live settings probe failed; trying a fresh connection");
                }
            }
        }

        // No live socket (or it died): a fresh connection is the old
        // behaviour and stays as the fallback.
        if let Ok(mut s) = connect(self.host, self.control_port).await {
            match collect_settings(&mut s, true, Duration::from_secs(2)).await {
                Ok(Some(v)) => {
                    *self.settings.lock().await = Some((Instant::now(), v.clone()));
                    *self.settings_link.lock().await = Some(s);
                    return Ok(Some(v));
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(error = %e, "settings probe failed"),
            }
        } else {
            tracing::debug!("settings connection failed");
        }

        // Last known good values keep the UI useful while the camera is busy.
        Ok(self
            .settings
            .lock()
            .await
            .as_ref()
            .map(|(_, v)| v.clone()))
    }

    async fn write_settings(&self, changes: serde_json::Value) -> Result<serde_json::Value> {
        let map = changes
            .as_object()
            .cloned()
            .context("settings changes must be an object")?;
        settings_write::write(self.host, self.control_port, &map).await
    }

    async fn open(&self) -> Result<Box<dyn CameraSession>> {
        let mut socks: HashMap<u16, TcpStream> = HashMap::new();
        let mut flags: [Option<[u8; 4]>; 2] = [None, None];
        let started = Instant::now();

        for step in handshake::STEPS {
            let due = started + Duration::from_millis(step.at_ms as u64);
            let now = Instant::now();
            if due > now {
                sleep(due - now).await;
            }

            if !socks.contains_key(&step.key) {
                let port = if step.port == self.control_port || step.port == self.media_port {
                    step.port
                } else {
                    // The capture always uses 3000/3001; keep the configured
                    // ports for cameras on non-default ones.
                    if step.port == 3001 {
                        self.control_port
                    } else {
                        self.media_port
                    }
                };
                let s = connect(self.host, port).await?;
                socks.insert(step.key, s);
            }

            let mut packet = decode_hex(step.hex)?;
            apply_flag(&mut packet, &flags);
            let stream = socks
                .get_mut(&step.key)
                .expect("socket inserted above");

            if let Err(e) = stream.write_all(&packet).await {
                bail!("handshake write failed at {}ms: {e}", step.at_ms);
            }

            // The probe reply only arrives after the 40-byte serial packet on
            // the same socket, so flags are collected once both serial packets
            // have gone out — never between probe and serial, which deadlocks.
            if packet.len() == 40 && step.key == handshake::PROBE_THERMAL {
                let f0 = read_flag(
                    socks
                        .get_mut(&handshake::PROBE_VISIBLE)
                        .expect("probe socket present"),
                )
                .await
                .context("visible probe")?;
                let f1 = read_flag(
                    socks
                        .get_mut(&handshake::PROBE_THERMAL)
                        .expect("probe socket present"),
                )
                .await
                .context("thermal probe")?;
                tracing::debug!(
                    visible = %hex(&f0),
                    thermal = %hex(&f1),
                    "session flags acquired"
                );
                flags = [Some(f0), Some(f1)];
            }
        }

        let flag0 = flags[0].context("camera never supplied a visible session flag")?;
        let flag1 = flags[1].context("camera never supplied a thermal session flag")?;

        let visible = socks
            .remove(&handshake::VIDEO_VISIBLE)
            .context("visible video socket missing")?;
        let thermal = socks
            .remove(&handshake::VIDEO_THERMAL)
            .context("thermal video socket missing")?;
        let probe0 = socks
            .remove(&handshake::PROBE_VISIBLE)
            .context("visible probe socket missing")?;
        let probe1 = socks
            .remove(&handshake::PROBE_THERMAL)
            .context("thermal probe socket missing")?;
        // The 0x6c probe is part of the captured handshake, so the camera's
        // configuration block is usually already waiting on this socket.
        let mut config = socks
            .remove(&handshake::CONFIG)
            .context("config control socket missing")?;
        match collect_settings(&mut config, false, Duration::from_millis(400)).await {
            Ok(Some(v)) => {
                *self.settings.lock().await = Some((Instant::now(), v));
            }
            Ok(None) => tracing::debug!("handshake carried no settings reply"),
            Err(e) => tracing::debug!(error = %e, "reading handshake settings reply failed"),
        }
        *self.settings_link.lock().await = Some(config);

        let mut keepalive = interval(KEEPALIVE_EVERY);
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
        keepalive.tick().await; // consume the immediate first tick

        tracing::info!(
            host = %self.host,
            visible_flag = %hex(&flag0),
            thermal_flag = %hex(&flag1),
            controls = socks.len(),
            "qianli handshake complete"
        );

        Ok(Box::new(QianliSession {
            spec: self.spec_with(self.calibration),
            visible,
            thermal,
            probe0,
            probe1,
            _control: socks.into_values().collect(),
            assembler: ThermalAssembler::new(),
            seq: 0,
            keepalive,
            last_visible: Instant::now(),
            last_thermal: Instant::now(),
        }))
    }
}

struct QianliSession {
    spec: CameraSpec,
    visible: TcpStream,
    thermal: TcpStream,
    /// Kept open: keepalives go here, not on the streaming sockets.
    probe0: TcpStream,
    probe1: TcpStream,
    /// Control sockets stay alive for the life of the session.
    _control: Vec<TcpStream>,
    assembler: ThermalAssembler,
    seq: u64,
    keepalive: tokio::time::Interval,
    /// Wall clock of the last frame from each stream. They are tracked
    /// separately so a live visible feed cannot mask a stalled thermal one.
    last_visible: Instant,
    last_thermal: Instant,
}

#[async_trait]
impl CameraSession for QianliSession {
    fn spec(&self) -> &CameraSpec {
        &self.spec
    }

    async fn next_frame(&mut self) -> Result<CameraFrame> {
        let calibration = self
            .spec
            .thermal
            .as_ref()
            .map(|t| t.calibration)
            .unwrap_or_default();
        let mut vbuf = vec![0u8; 64 * 1024];
        let mut tbuf = vec![0u8; 64 * 1024];

        loop {
            // Per-stream deadlines, not one timeout reset by any traffic:
            // visible frames arrive continuously and used to keep the
            // thermal stream's silence from ever being noticed.
            let now = Instant::now();
            let thermal_deadline = self.last_thermal + SILENCE_TIMEOUT;
            let visible_deadline = self.last_visible + SILENCE_TIMEOUT;
            let wait = thermal_deadline
                .min(visible_deadline)
                .saturating_duration_since(now);

            let Self {
                visible,
                thermal,
                probe0,
                probe1,
                assembler,
                seq,
                keepalive,
                ..
            } = self;
            let mut keepalive_due = false;
            let mut stalled = false;

            let frame = tokio::select! {
                r = visible.read(&mut vbuf) => {
                    let n = r.context("visible stream read failed")?;
                    if n == 0 {
                        bail!("camera closed the visible stream");
                    }
                    *seq += 1;
                    Some(CameraFrame {
                        stream: StreamKind::Visible,
                        seq: *seq,
                        at: std::time::Instant::now(),
                        payload: FramePayload::Video(Bytes::copy_from_slice(&vbuf[..n])),
                    })
                }
                r = thermal.read(&mut tbuf) => {
                    let n = r.context("thermal stream read failed")?;
                    if n == 0 {
                        bail!("camera closed the thermal stream");
                    }
                    if let Some(frame) = assembler.push(&tbuf[..n]).into_iter().last() {
                        *seq += 1;
                        Some(CameraFrame {
                            stream: StreamKind::Thermal,
                            seq: *seq,
                            at: std::time::Instant::now(),
                            payload: FramePayload::Thermal(ThermalImage {
                                width: WIDTH,
                                height: HEIGHT,
                                raw: frame.raw,
                                calibration,
                            }),
                        })
                    } else {
                        None
                    }
                }
                _ = keepalive.tick() => {
                    keepalive_due = true;
                    None
                }
                _ = sleep(wait) => {
                    stalled = true;
                    None
                }
            };

            if stalled {
                if Instant::now() >= thermal_deadline {
                    bail!(
                        "camera sent no thermal frames for {}s",
                        SILENCE_TIMEOUT.as_secs()
                    );
                }
                bail!(
                    "camera sent no visible frames for {}s",
                    SILENCE_TIMEOUT.as_secs()
                );
            }
            if keepalive_due {
                let _ = probe0.write_all(&p::keepalive(0)).await;
                let _ = probe1.write_all(&p::keepalive(1)).await;
            }
            if let Some(frame) = frame {
                match frame.stream {
                    StreamKind::Visible => self.last_visible = Instant::now(),
                    StreamKind::Thermal => self.last_thermal = Instant::now(),
                }
                return Ok(frame);
            }
        }
    }

    async fn close(&mut self) {
        let _ = self.visible.shutdown().await;
        let _ = self.thermal.shutdown().await;
        let _ = self.probe0.shutdown().await;
        let _ = self.probe1.shutdown().await;
        for s in &mut self._control {
            let _ = s.shutdown().await;
        }
    }
}

/// Read one configuration block, optionally asking for it first.
///
/// A reply is an 88-byte echo followed by the 376-byte block; the parser also
/// accepts a bare block. Unrelated traffic on the same socket is ignored until
/// the deadline, so keepalives cannot produce a false parse.
async fn collect_settings(
    s: &mut TcpStream,
    probe: bool,
    budget: Duration,
) -> Result<Option<serde_json::Value>> {
    if probe {
        s.write_all(&p::register(0x6c, 0)).await?;
    }
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match timeout(left, s.read(&mut chunk)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(v) = settings::parse(&buf) {
                    return Ok(Some(v));
                }
                if buf.len() > 8192 {
                    let keep = buf.split_off(buf.len() - 1024);
                    buf = keep;
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => break,
        }
    }
    Ok(None)
}

async fn connect(host: Ipv4Addr, port: u16) -> Result<TcpStream> {
    let s = timeout(Duration::from_secs(3), TcpStream::connect((host, port)))
        .await
        .with_context(|| format!("connecting to {host}:{port} timed out"))??;
    s.set_nodelay(true)?;
    Ok(s)
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!("odd-length hex packet");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("bad hex packet"))
        .collect()
}

/// Replace the captured placeholder flag with the live one.
///
/// START is 8 bytes and carries `0x33` (visible) or `0x35` (thermal) as its
/// placeholder. CTRL is 16 bytes: value 2 = visible, value 4 = thermal, and the
/// flag sits at bytes 8..12.
fn apply_flag(packet: &mut [u8], flags: &[Option<[u8; 4]>; 2]) {
    if packet.len() == 8 {
        let live = match packet[0] {
            0x33 => flags[0],
            0x35 => flags[1],
            _ => None,
        };
        if let Some(flag) = live {
            packet[..4].copy_from_slice(&flag);
        }
    } else if packet.len() == 16 {
        let value = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        let live = match value {
            2 => flags[0],
            4 => flags[1],
            _ => None,
        };
        if let Some(flag) = live {
            packet[8..12].copy_from_slice(&flag);
        }
    }
}

/// Scan a probe reply for the `01 00` marker at 28..30, then take the flag from
/// 48..52. Not every reply carries it, so this reads until the marker appears.
async fn read_flag(s: &mut TcpStream) -> Result<[u8; 4]> {
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut chunk = [0u8; 256];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);

    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            bail!("timed out waiting for the session flag");
        }
        let n = timeout(left, s.read(&mut chunk))
            .await
            .context("probe reply timed out")??;
        if n == 0 {
            bail!("probe socket closed before the session flag arrived");
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() >= 52 && buf[28] == 0x01 && buf[29] == 0x00 {
            let mut flag = [0u8; 4];
            flag.copy_from_slice(&buf[48..52]);
            return Ok(flag);
        }
        if buf.len() > 4096 {
            let drop = buf.len() - 64;
            buf.drain(..drop);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CameraConfig;
    use tokio::net::TcpListener;

    /// An 88-byte echo followed by the camera's 376-byte config block.
    fn config_block(name: &str, ip: &str) -> Vec<u8> {
        let mut blob = vec![0u8; 88 + 376];
        blob[88..88 + name.len()].copy_from_slice(name.as_bytes());
        blob[112..112 + ip.len()].copy_from_slice(ip.as_bytes());
        blob
    }

    fn driver(port: u16) -> QianliDriver {
        let cfg = CameraConfig {
            driver: "qianli".into(),
            host: "127.0.0.1".into(),
            control_port: port,
            media_port: port,
            ..Default::default()
        };
        QianliDriver::from_config(&cfg).expect("driver")
    }

    #[tokio::test]
    async fn settings_probe_reuses_the_live_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // Two reads over one accepted connection: the second must reuse it.
            for _ in 0..2 {
                let mut probe = [0u8; 88];
                s.read_exact(&mut probe).await.unwrap();
                let cmd = u64::from_le_bytes(probe[68..76].try_into().unwrap());
                assert_eq!(cmd, 0x6c, "expected the settings probe");
                s.write_all(&config_block("SuperCam", "192.168.1.64"))
                    .await
                    .unwrap();
            }
        });

        let d = driver(port);
        let first = d.read_settings().await.unwrap().expect("first reply");
        assert_eq!(first["name"], "SuperCam");
        assert_eq!(first["reply_offset"], 88);
        let second = d.read_settings().await.unwrap().expect("cached/reused");
        assert_eq!(second["ip"], "192.168.1.64");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_reply_is_parsed_without_a_probe() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // No request: this is the reply the captured handshake already asked for.
            s.write_all(&config_block("SuperCam", "10.0.0.7"))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
        });

        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let v = collect_settings(&mut s, false, Duration::from_millis(500))
            .await
            .unwrap()
            .expect("handshake reply");
        assert_eq!(v["ip"], "10.0.0.7");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn settings_probe_ignores_unrelated_traffic() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut probe = [0u8; 88];
            s.read_exact(&mut probe).await.unwrap();
            s.write_all(&[0u8; 7]).await.unwrap();
            s.write_all(&config_block("SuperCam", "10.1.2.3")).await.unwrap();
        });

        let d = driver(port);
        let v = d.read_settings().await.unwrap().expect("reply after junk");
        assert_eq!(v["ip"], "10.1.2.3");
        server.await.unwrap();
    }
}
