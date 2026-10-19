//! The driver contract.
//!
//! A driver owns everything vendor-specific: how to connect, how packets are
//! framed, how pixels are laid out, and what the sensor's raw values mean.
//! Everything above this module works only with the types defined here.

use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use serde::Serialize;

pub mod mock;
pub mod qianli;
pub mod registry;

pub type Result<T> = anyhow::Result<T>;

/// Which logical stream a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    /// Ordinary camera image (H.264 / MJPEG / raw), meant for human viewing.
    Visible,
    /// Radiometric thermal image.
    Thermal,
}

/// `celsius = raw * scale + offset`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Calibration {
    pub scale: f32,
    pub offset: f32,
}

impl Default for Calibration {
    fn default() -> Self {
        // Qianli factory default; override per camera after a two-point fit.
        Self {
            scale: 0.01,
            offset: 0.0,
        }
    }
}

impl Calibration {
    pub fn celsius(&self, raw: u16) -> f32 {
        raw as f32 * self.scale + self.offset
    }

    /// Two `(raw, true_celsius)` pairs -> scale and offset.
    pub fn fit(points: [(u16, f32); 2]) -> anyhow::Result<Self> {
        let (r1, t1) = (points[0].0 as f32, points[0].1);
        let (r2, t2) = (points[1].0 as f32, points[1].1);
        anyhow::ensure!(
            (r2 - r1).abs() >= 50.0,
            "calibration points are too close together (need a real span)"
        );
        let scale = (t2 - t1) / (r2 - r1);
        anyhow::ensure!(
            (0.0001..=1.0).contains(&scale.abs()),
            "implausible scale {scale}"
        );
        Ok(Self {
            scale,
            offset: t1 - scale * r1,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoStream {
    pub width: u32,
    pub height: u32,
    /// Container/codec hint for the browser, e.g. `h264-annexb`.
    pub codec: &'static str,
    pub fps_hint: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThermalStream {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub calibration: Calibration,
}

/// What a driver can produce. The web UI builds itself from this.
#[derive(Debug, Clone, Serialize)]
pub struct CameraSpec {
    pub driver: &'static str,
    pub model: &'static str,
    pub video: Option<VideoStream>,
    pub thermal: Option<ThermalStream>,
}

/// One radiometric image, already extracted from the vendor wrapper.
#[derive(Debug, Clone)]
pub struct ThermalImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` little-endian `u16` samples.
    pub raw: Bytes,
    pub calibration: Calibration,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Peak {
    pub x: u32,
    pub y: u32,
    pub celsius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ThermalStats {
    pub min_c: f32,
    pub max_c: f32,
    pub mean_c: f32,
    pub hot: (u32, u32),
    pub cold: (u32, u32),
    pub width: u32,
    pub height: u32,
}

impl ThermalImage {
    pub fn pixel_count(&self) -> usize {
        (self.width * self.height) as usize
    }

    pub fn raw_value(&self, x: u32, y: u32) -> Option<u16> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y * self.width + x) as usize * 2;
        Some(u16::from_le_bytes([self.raw[i], self.raw[i + 1]]))
    }

    pub fn celsius_at(&self, x: u32, y: u32) -> Option<f32> {
        self.raw_value(x, y).map(|v| self.calibration.celsius(v))
    }

    pub fn values(&self) -> impl Iterator<Item = u16> + '_ {
        self.raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
    }

    pub fn stats(&self) -> ThermalStats {
        let mut min = u16::MAX;
        let mut max = 0u16;
        let mut sum = 0u64;
        let (mut hot, mut cold) = (0usize, 0usize);
        for (i, v) in self.values().enumerate() {
            if v < min {
                min = v;
                cold = i;
            }
            if v > max {
                max = v;
                hot = i;
            }
            sum += v as u64;
        }
        let n = self.pixel_count().max(1) as u64;
        let w = self.width.max(1);
        ThermalStats {
            min_c: self.calibration.celsius(min),
            max_c: self.calibration.celsius(max),
            mean_c: (sum as f32 / n as f32) * self.calibration.scale + self.calibration.offset,
            hot: (hot as u32 % w, hot as u32 / w),
            cold: (cold as u32 % w, cold as u32 / w),
            width: self.width,
            height: self.height,
        }
    }

    /// Up to `n` hottest spots, kept apart so they label distinct regions.
    /// A border margin skips sensor-edge artefacts.
    pub fn peaks(&self, n: usize) -> Vec<Peak> {
        const MIN_SEP: i64 = 14;
        const MARGIN: u32 = 3;
        let mut candidates: Vec<(u16, u32, u32)> = self
            .values()
            .enumerate()
            .map(|(i, v)| (v, i as u32 % self.width, i as u32 / self.width))
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0));

        let mut out: Vec<Peak> = Vec::with_capacity(n.min(8));
        for (v, x, y) in candidates {
            if x < MARGIN || y < MARGIN || x >= self.width - MARGIN || y >= self.height - MARGIN {
                continue;
            }
            let distinct = out.iter().all(|p| {
                let dx = p.x as i64 - x as i64;
                let dy = p.y as i64 - y as i64;
                dx * dx + dy * dy >= MIN_SEP * MIN_SEP
            });
            if distinct {
                out.push(Peak {
                    x,
                    y,
                    celsius: self.calibration.celsius(v),
                });
            }
            if out.len() >= n {
                break;
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub enum FramePayload {
    /// An opaque chunk of the visible stream (Annex-B H.264 in the Qianli case).
    /// The server forwards it to the browser or to a transcoder unchanged.
    Video(Bytes),
    Thermal(ThermalImage),
}

#[derive(Debug, Clone)]
pub struct CameraFrame {
    pub stream: StreamKind,
    pub seq: u64,
    pub at: Instant,
    pub payload: FramePayload,
}

impl CameraFrame {
    pub fn thermal(&self) -> Option<&ThermalImage> {
        match &self.payload {
            FramePayload::Thermal(t) => Some(t),
            _ => None,
        }
    }
}

/// Small helper for renderer tests.
#[cfg(test)]
pub fn thermal_test_image(values: &[u16], width: u32, height: u32) -> ThermalImage {
    let mut raw = Vec::with_capacity(values.len() * 2);
    for v in values {
        raw.extend_from_slice(&v.to_le_bytes());
    }
    ThermalImage {
        width,
        height,
        raw: Bytes::from(raw),
        calibration: Calibration::default(),
    }
}

/// A camera family. Cheap to clone/share; `open` performs the handshake and
/// returns a live session.
#[async_trait]
pub trait CameraDriver: Send + Sync {
    fn id(&self) -> &'static str;

    /// Static capabilities; safe to call before connecting.
    fn spec(&self) -> CameraSpec;

    /// Connect and complete the vendor handshake.
    async fn open(&self) -> Result<Box<dyn CameraSession>>;

    /// Read the camera's own configuration, if the family supports it.
    async fn read_settings(&self) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

/// A live connection. `next_frame` must be cancellation-safe and should fail
/// (rather than hang forever) if the camera goes silent.
#[async_trait]
pub trait CameraSession: Send {
    fn spec(&self) -> &CameraSpec;

    /// Next available frame from any stream. Blocks until one arrives.
    async fn next_frame(&mut self) -> Result<CameraFrame>;

    /// Best-effort orderly teardown.
    async fn close(&mut self);
}
