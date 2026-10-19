//! P3/P1 USB thermal cameras.
//!
//! The USB transfer layer is not wired in this build. The protocol parser and
//! calibration are ported from `jvdillon/p3-ir-camera` (P3_PROTOCOL.md) and are
//! covered by byte-level tests; the driver is advertised as **untested** until
//! it can be exercised against a real camera.

use async_trait::async_trait;
use bytes::Bytes;

use super::{
    Calibration, CameraDriver, CameraSession, CameraSpec, Capabilities, ThermalImage,
    ThermalStream,
};
use crate::config::CameraConfig;

pub const DRIVER_ID: &str = "p3";

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
            model: "P3/P1 USB thermal (untested)",
            video: None,
            thermal: Some(ThermalStream {
                width: self.model.width(),
                height: self.model.height(),
                bit_depth: 16,
                calibration: self.model.calibration(),
            }),
            capabilities: Capabilities {
                thermal: true,
                visible: false,
                discover: false,
                network_settings: false,
                reboot: false,
                controls: vec!["shutter".into(), "gain".into()],
            },
        }
    }

    async fn open(&self) -> anyhow::Result<Box<dyn CameraSession>> {
        anyhow::bail!(
            "P3 USB transfer layer is not wired in this build (driver is untested); \
             the protocol parser and calibration are ready"
        )
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
}
