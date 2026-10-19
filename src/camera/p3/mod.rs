//! P3/P1 USB thermal cameras.
//!
//! The parser, calibration and USB session are ported from
//! `jvdillon/p3-ir-camera` (P3_PROTOCOL.md). The transport uses `nusb`; it has
//! byte-level tests, but still needs a run against real P3/P1 hardware.

use async_trait::async_trait;
use bytes::Bytes;
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Recipient};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

use super::{
    Calibration, CameraDriver, CameraFrame, CameraSession, CameraSpec, Capabilities, FramePayload,
    StreamKind, ThermalImage, ThermalStream,
};
use crate::config::CameraConfig;

pub const DRIVER_ID: &str = "p3";
const VID: u16 = 0x3474;
const P3_PID: u16 = 0x45a2;
const P1_PID: u16 = 0x45c2;
const MARKER_LEN: usize = 12;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const READ_NAME: [u8; 18] = [
    0x01, 0x01, 0x81, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e, 0x00, 0x00, 0x00,
    0x4f, 0x90,
];
const START_STREAM: [u8; 18] = [
    0x01, 0x2f, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x49, 0x30,
];
const GAIN_LOW: [u8; 18] = [
    0x01, 0x2f, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x3c, 0x3a,
];
const GAIN_HIGH: [u8; 18] = [
    0x01, 0x2f, 0x41, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x49, 0x39,
];
const SHUTTER: [u8; 18] = [
    0x01, 0x36, 0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xcd, 0x0b,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    P1,
    P3,
}

impl Model {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "p1" => Some(Model::P1),
            "p3" => Some(Model::P3),
            _ => None,
        }
    }

    pub fn width(self) -> u32 {
        match self {
            Model::P1 => 160,
            Model::P3 => 256,
        }
    }

    pub fn height(self) -> u32 {
        match self {
            Model::P1 => 120,
            Model::P3 => 192,
        }
    }

    fn rows(self) -> u32 {
        2 * self.height() + 2
    }

    pub fn frame_bytes(self) -> usize {
        (self.width() * self.rows() * 2) as usize
    }

    pub fn calibration(self) -> Calibration {
        // Raw values are 1/64 Kelvin.
        Calibration {
            scale: 1.0 / 64.0,
            offset: -273.15,
        }
    }
}

pub struct P3Driver {
    model: Model,
}

impl P3Driver {
    pub fn from_config(cfg: &CameraConfig) -> anyhow::Result<Self> {
        let name = cfg
            .options
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("p3");
        let model = Model::from_name(name)
            .ok_or_else(|| anyhow::anyhow!("unknown P3 model {name:?}; use p1 or p3"))?;
        Ok(Self { model })
    }
}

#[async_trait]
impl CameraDriver for P3Driver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn spec(&self) -> CameraSpec {
        CameraSpec {
            driver: DRIVER_ID,
            model: "P3/P1 USB thermal",
            video: None,
            thermal: Some(ThermalStream {
                width: self.model.width(),
                height: self.model.height(),
                bit_depth: 16,
                calibration: self.model.calibration(),
            }),
            profile: None,
            device: None,
            capabilities: Capabilities {
                thermal: true,
                visible: false,
                radiometric: true,
                discover: false,
                network_settings: false,
                reboot: false,
                controls: vec!["shutter".into(), "gain".into()],
            },
        }
    }

    async fn open(&self) -> anyhow::Result<Box<dyn CameraSession>> {
        let pid = match self.model {
            Model::P1 => P1_PID,
            Model::P3 => P3_PID,
        };
        let info = nusb::list_devices()
            .await
            .map_err(|e| anyhow::anyhow!("list USB devices: {e}"))?
            .find(|d| d.vendor_id() == VID && d.product_id() == pid)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "P3/P1 not found (VID 0x{VID:04x}, PID 0x{pid:04x}); \
                     check the cable, udev rule and permissions"
                )
            })?;
        let device = info
            .open()
            .await
            .map_err(|e| anyhow::anyhow!("open P3/P1 USB device: {e}"))?;

        let iface0 = device
            .detach_and_claim_interface(0)
            .await
            .map_err(|e| anyhow::anyhow!("claim P3 control interface 0: {e}"))?;
        let iface1 = device
            .detach_and_claim_interface(1)
            .await
            .map_err(|e| anyhow::anyhow!("claim P3 streaming interface 1: {e}"))?;

        send_command(&iface0, &READ_NAME).await?;
        read_status(&iface0).await?;
        let name = read_response(&iface0, 30).await?;
        read_status(&iface0).await?;
        let name = String::from_utf8_lossy(&name)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        tracing::info!(%name, model = ?self.model, "P3/P1 device opened");

        start_stream(&iface0).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;

        iface1
            .set_alt_setting(1)
            .await
            .map_err(|e| anyhow::anyhow!("enable P3 streaming alt setting: {e}"))?;
        device
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: 0xee,
                    value: 0,
                    index: 1,
                    data: &[],
                },
                CONTROL_TIMEOUT,
            )
            .await
            .map_err(|e| anyhow::anyhow!("enable P3 stream: {e}"))?;
        tokio::time::sleep(Duration::from_secs(2)).await;

        let endpoint = iface1
            .endpoint::<Bulk, In>(0x81)
            .map_err(|e| anyhow::anyhow!("P3 bulk endpoint 0x81: {e}"))?;
        let reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> =
            Box::new(endpoint.reader(64).with_num_transfers(8));

        // The device wants a second start after the interface is enabled.
        start_stream(&iface0).await?;

        Ok(Box::new(P3Session {
            spec: self.spec(),
            _device: device,
            iface0,
            iface1,
            reader,
            assembler: FrameAssembler::new(self.model),
            pending: VecDeque::new(),
            seq: 0,
            closed: false,
        }))
    }
}

pub struct P3Session {
    spec: CameraSpec,
    _device: nusb::Device,
    iface0: nusb::Interface,
    iface1: nusb::Interface,
    reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    assembler: FrameAssembler,
    pending: VecDeque<ThermalImage>,
    seq: u64,
    closed: bool,
}

impl P3Session {
    /// Trigger the camera's shutter/NUC. The current server control surface
    /// does not expose this yet, but it is kept here for the next API step.
    #[allow(dead_code)]
    pub async fn shutter(&self) -> anyhow::Result<()> {
        send_command(&self.iface0, &SHUTTER).await?;
        read_status(&self.iface0).await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn set_high_gain(&self, high: bool) -> anyhow::Result<()> {
        send_command(&self.iface0, if high { &GAIN_HIGH } else { &GAIN_LOW }).await?;
        read_status(&self.iface0).await?;
        Ok(())
    }
}

#[async_trait]
impl CameraSession for P3Session {
    fn spec(&self) -> &CameraSpec {
        &self.spec
    }

    async fn next_frame(&mut self) -> anyhow::Result<CameraFrame> {
        if let Some(image) = self.pending.pop_front() {
            self.seq += 1;
            return Ok(CameraFrame {
                stream: StreamKind::Thermal,
                seq: self.seq,
                at: std::time::Instant::now(),
                payload: FramePayload::Thermal(image),
            });
        }
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = timeout(FRAME_TIMEOUT, self.reader.read(&mut chunk))
                .await
                .map_err(|_| {
                    anyhow::anyhow!("P3/P1 sent no USB frames for {}s", FRAME_TIMEOUT.as_secs())
                })?
                .map_err(|e| anyhow::anyhow!("P3 bulk read failed: {e}"))?;
            if n == 0 {
                anyhow::bail!("P3/P1 closed the USB stream");
            }
            let mut images = self.assembler.push(&chunk[..n]);
            if !images.is_empty() {
                let image = images.remove(0);
                self.pending.extend(images);
                self.seq += 1;
                return Ok(CameraFrame {
                    stream: StreamKind::Thermal,
                    seq: self.seq,
                    at: std::time::Instant::now(),
                    payload: FramePayload::Thermal(image),
                });
            }
        }
    }

    async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.iface1.set_alt_setting(0).await;
    }
}

async fn send_command(iface: &nusb::Interface, command: &[u8; 18]) -> anyhow::Result<()> {
    iface
        .control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: 0x20,
                value: 0,
                index: 0,
                data: command,
            },
            CONTROL_TIMEOUT,
        )
        .await
        .map_err(|e| anyhow::anyhow!("P3 command write failed: {e}"))
}

async fn read_status(iface: &nusb::Interface) -> anyhow::Result<u8> {
    let data = iface
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: 0x22,
                value: 0,
                index: 0,
                length: 1,
            },
            CONTROL_TIMEOUT,
        )
        .await
        .map_err(|e| anyhow::anyhow!("P3 status read failed: {e}"))?;
    Ok(data.first().copied().unwrap_or(0))
}

async fn read_response(iface: &nusb::Interface, length: u16) -> anyhow::Result<Vec<u8>> {
    iface
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: 0x21,
                value: 0,
                index: 0,
                length,
            },
            CONTROL_TIMEOUT,
        )
        .await
        .map_err(|e| anyhow::anyhow!("P3 response read failed: {e}"))
}

async fn start_stream(iface: &nusb::Interface) -> anyhow::Result<()> {
    send_command(iface, &START_STREAM).await?;
    read_status(iface).await?;
    let _ = read_response(iface, 1).await?;
    read_status(iface).await?;
    Ok(())
}

/// Reassembles the two bulk transfers per frame and verifies the start/end
/// markers. Extra or corrupt bytes are dropped while looking for the next
/// marker, so a single bad frame does not kill the stream.
pub struct FrameAssembler {
    model: Model,
    buf: Vec<u8>,
}

impl FrameAssembler {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            buf: Vec::with_capacity(model.frame_bytes() + 2 * MARKER_LEN),
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<ThermalImage> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.find_start_marker() else {
                if self.buf.len() > 1 {
                    let keep = self.buf.len() - 1;
                    self.buf.drain(..keep);
                }
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            let body = self.model.frame_bytes();
            let total = MARKER_LEN + body + MARKER_LEN;
            if self.buf.len() < total {
                break;
            }
            let end = MARKER_LEN + body;
            let start_marker = &self.buf[..MARKER_LEN];
            let end_marker = &self.buf[end..total];
            let sync_ok = (0x8e..=0x8f).contains(&end_marker[1]);
            let count_ok = start_marker[2..6] == end_marker[2..6];
            if sync_ok && count_ok {
                if let Ok(image) = parse_frame(&self.buf[..end], self.model) {
                    out.push(image);
                }
                self.buf.drain(..total);
            } else {
                self.buf.drain(..1);
            }
        }
        out
    }

    fn find_start_marker(&self) -> Option<usize> {
        (0..self.buf.len().saturating_sub(1))
            .find(|&i| self.buf[i] == MARKER_LEN as u8 && (0x8c..=0x8d).contains(&self.buf[i + 1]))
    }
}

/// Parse one camera frame: a 12-byte start marker followed by the pixel buffer.
pub fn parse_frame(frame: &[u8], model: Model) -> anyhow::Result<ThermalImage> {
    let body = model.frame_bytes();
    anyhow::ensure!(
        frame.len() >= 12 + body,
        "short P3 frame: {} bytes, need {}",
        frame.len(),
        12 + body
    );
    let marker = &frame[..12];
    anyhow::ensure!(marker[0] == 12, "bad marker length {}", marker[0]);
    anyhow::ensure!(
        (0x8c..=0x8f).contains(&marker[1]),
        "bad marker sync 0x{:02x}",
        marker[1]
    );
    let data = &frame[12..12 + body];
    let width = model.width() as usize;
    let height = model.height() as usize;
    let start_row = height + 2;
    let mut raw = Vec::with_capacity(width * height * 2);
    for row in start_row..start_row + height {
        let off = row * width * 2;
        raw.extend_from_slice(&data[off..off + width * 2]);
    }
    Ok(ThermalImage {
        width: width as u32,
        height: height as u32,
        raw: Bytes::from(raw),
        calibration: model.calibration(),
        lut: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(model: Model, value: u16) -> Vec<u8> {
        let mut frame = vec![0u8; 12 + model.frame_bytes()];
        frame[0] = 12;
        frame[1] = 0x8c;
        let width = model.width() as usize;
        let height = model.height() as usize;
        let start = height + 2;
        for row in start..start + height {
            for col in 0..width {
                let off = 12 + (row * width + col) * 2;
                frame[off..off + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        frame
    }

    #[test]
    fn parses_p3_thermal_rows() {
        let frame = synthetic(Model::P3, 19082);
        let img = parse_frame(&frame, Model::P3).unwrap();
        assert_eq!((img.width, img.height), (256, 192));
        assert_eq!(img.raw.len(), 256 * 192 * 2);
        let first = u16::from_le_bytes([img.raw[0], img.raw[1]]);
        assert_eq!(first, 19082);
        assert!((img.calibration.celsius(first) - 25.0).abs() < 0.01);
    }

    #[test]
    fn parses_p1_thermal_rows() {
        let frame = synthetic(Model::P1, 16000);
        let img = parse_frame(&frame, Model::P1).unwrap();
        assert_eq!((img.width, img.height), (160, 120));
        assert_eq!(img.raw.len(), 160 * 120 * 2);
    }

    #[test]
    fn rejects_bad_marker() {
        let mut frame = synthetic(Model::P3, 1);
        frame[1] = 0x00;
        assert!(parse_frame(&frame, Model::P3).is_err());
    }

    #[test]
    fn rejects_short_frame() {
        assert!(parse_frame(&[12, 0x8c], Model::P3).is_err());
    }
    #[test]
    fn assembler_handles_split_frame() {
        let model = Model::P3;
        let mut frame = synthetic(model, 19082);
        let mut end = [0u8; 12];
        end[0] = 12;
        end[1] = 0x8e;
        end[2..6].copy_from_slice(&frame[2..6]);
        frame.extend_from_slice(&end);

        let split = frame.len() / 2;
        let mut assembler = FrameAssembler::new(model);
        assert!(assembler.push(&frame[..split]).is_empty());
        let images = assembler.push(&frame[split..]);
        assert_eq!(images.len(), 1);
        assert_eq!((images[0].width, images[0].height), (256, 192));
    }

    #[test]
    fn assembler_rejects_mismatched_counter() {
        let model = Model::P3;
        let mut frame = synthetic(model, 1);
        let mut end = [0u8; 12];
        end[0] = 12;
        end[1] = 0x8e;
        end[2..6].copy_from_slice(&[9, 0, 0, 0]);
        frame.extend_from_slice(&end);

        let mut assembler = FrameAssembler::new(model);
        assert!(assembler.push(&frame).is_empty());
    }
}
