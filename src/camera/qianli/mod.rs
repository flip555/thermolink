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
pub mod settings;
pub mod thermal;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
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

    async fn read_settings(&self) -> Result<Option<serde_json::Value>> {
        let mut s = connect(self.host, self.control_port).await?;
        s.write_all(&p::register(0x6c, 0)).await?;
        let mut buf: Vec<u8> = Vec::with_capacity(512);
        let mut chunk = [0u8; 512];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match timeout(left, s.read(&mut chunk)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() >= 464 {
                        break;
                    }
                }
                Ok(Err(_)) => break,
            }
        }
        Ok(settings::parse(&buf))
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

            tokio::select! {
                r = visible.read(&mut vbuf) => {
                    let n = r.context("visible stream read failed")?;
                    if n == 0 {
                        bail!("camera closed the visible stream");
                    }
                    *seq += 1;
                    return Ok(CameraFrame {
                        stream: StreamKind::Visible,
                        seq: *seq,
                        at: std::time::Instant::now(),
                        payload: FramePayload::Video(Bytes::copy_from_slice(&vbuf[..n])),
                    });
                }
                r = thermal.read(&mut tbuf) => {
                    let n = r.context("thermal stream read failed")?;
                    if n == 0 {
                        bail!("camera closed the thermal stream");
                    }
                    if let Some(frame) = assembler.push(&tbuf[..n]).into_iter().last() {
                        *seq += 1;
                        return Ok(CameraFrame {
                            stream: StreamKind::Thermal,
                            seq: *seq,
                            at: std::time::Instant::now(),
                            payload: FramePayload::Thermal(ThermalImage {
                                width: WIDTH,
                                height: HEIGHT,
                                raw: frame.raw,
                                calibration,
                            }),
                        });
                    }
                }
                _ = keepalive.tick() => {
                    keepalive_due = true;
                }
                _ = sleep(SILENCE_TIMEOUT) => {
                    stalled = true;
                }
            }

            if stalled {
                bail!("camera sent nothing for {}s", SILENCE_TIMEOUT.as_secs());
            }
            if keepalive_due {
                let _ = probe0.write_all(&p::keepalive(0)).await;
                let _ = probe1.write_all(&p::keepalive(1)).await;
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
