//! Qianli / Sunshine SuperCam ("Neptune") driver.
//!
//! One session uses four media sockets (two probe/hold, two streaming) and a
//! few short-lived control sockets. The handshake is strictly sequential: the
//! camera accepts connections out of order and then drops them silently, so
//! every step below waits for its reply before the next one starts.
//!
//! The per-session flag is the whole trick. It arrives in the probe reply and
//! must be echoed in START and CTRL; replaying a stale flag produces a polite
//! handshake and a stream that closes after a second or two with zero frames.

pub mod protocol;
pub mod thermal;

use std::net::Ipv4Addr;
use std::time::Duration;

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
const HANDSHAKE_STEP_PAUSE: Duration = Duration::from_millis(25);

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

    async fn open(&self) -> Result<Box<dyn CameraSession>> {
        // 1. Client registration: two commands, two flags each, one connection
        //    apiece. The captured vendor client never reuses these sockets.
        for (cmd, flags) in [(0x65u64, 0u64), (0x65, 1), (0x66, 0), (0x66, 1)] {
            let mut s = connect(self.host, self.control_port).await?;
            s.write_all(&p::register(cmd, flags)).await?;
            drain(&mut s, Duration::from_millis(120)).await;
            s.shutdown().await.ok();
            sleep(HANDSHAKE_STEP_PAUSE).await;
        }

        // 2. Probe each value on its own media connection. The reply carries
        //    the live session flag; nothing else in the protocol does.
        let mut probe0 = connect(self.host, self.media_port).await?;
        probe0.write_all(&p::probe(self.host, self.media_port, 0)).await?;
        probe0.write_all(&p::serial_packet()).await?;
        let flag0 = read_flag(&mut probe0).await.context("visible probe")?;

        let mut probe1 = connect(self.host, self.media_port).await?;
        probe1.write_all(&p::probe(self.host, self.media_port, 1)).await?;
        probe1.write_all(&p::serial_packet()).await?;
        let flag1 = read_flag(&mut probe1).await.context("thermal probe")?;
        sleep(HANDSHAKE_STEP_PAUSE).await;

        // 3. Streaming sockets are separate from the probe sockets: INIT binds
        //    a value, START (carrying the live flag) actually opens the tap.
        let mut visible = connect(self.host, self.media_port).await?;
        visible.write_all(&p::init(0, 8)).await?;
        visible.write_all(&p::start(flag0, 1)).await?;

        let mut thermal = connect(self.host, self.media_port).await?;
        thermal.write_all(&p::init(1, 8)).await?;
        thermal.write_all(&p::start(flag1, 1)).await?;
        sleep(HANDSHAKE_STEP_PAUSE).await;

        // 4. Announce each flag on its own control connection, then run the
        //    type-0x14 handshake on the probe socket that owns it.
        let mut ctl_thermal = connect(self.host, self.control_port).await?;
        ctl_thermal.write_all(&p::register(0x78, 1)).await?;
        drain(&mut ctl_thermal, Duration::from_millis(100)).await;
        ctl_thermal
            .write_all(&p::ctrl(4, flag1, self.media_port))
            .await?;
        probe1.write_all(&p::type14(1)).await?;
        sleep(HANDSHAKE_STEP_PAUSE).await;

        let mut ctl_visible = connect(self.host, self.control_port).await?;
        ctl_visible.write_all(&p::register(0x78, 0)).await?;
        drain(&mut ctl_visible, Duration::from_millis(100)).await;
        ctl_visible
            .write_all(&p::ctrl(2, flag0, self.media_port))
            .await?;
        probe0.write_all(&p::type14(0)).await?;

        let mut keepalive = interval(KEEPALIVE_EVERY);
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
        keepalive.tick().await; // consume the immediate first tick

        tracing::info!(
            host = %self.host,
            visible_flag = %hex(&flag0),
            thermal_flag = %hex(&flag1),
            "qianli handshake complete"
        );

        Ok(Box::new(QianliSession {
            spec: self.spec_with(self.calibration),
            visible,
            thermal,
            probe0,
            probe1,
            _control: vec![ctl_visible, ctl_thermal],
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

/// Read briefly and discard, so replies cannot be mis-attributed to the next
/// step. Deliberately bounded: the camera may keep the socket open.
async fn drain(s: &mut TcpStream, wait: Duration) {
    let mut buf = [0u8; 512];
    let _ = timeout(wait, async {
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
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
