//! A simulated camera for development and demos.
//!
//! It produces the same frame type as a real driver — 160×120 radiometric
//! samples — so the whole server/UI path can be exercised with no hardware.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::time::sleep;

use super::{
    Calibration, CameraDriver, CameraFrame, CameraSession, CameraSpec, FramePayload, StreamKind,
    ThermalImage, ThermalStream,
};

pub const WIDTH: u32 = 160;
pub const HEIGHT: u32 = 120;

pub struct MockDriver {
    spec: CameraSpec,
}

impl Default for MockDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDriver {
    pub fn new() -> Self {
        Self {
            spec: CameraSpec {
                driver: "mock",
                model: "Simulated 160x120 thermal source",
                video: None,
                capabilities: super::Capabilities {
                    thermal: true,
                    ..Default::default()
                },
                thermal: Some(ThermalStream {
                    width: WIDTH,
                    height: HEIGHT,
                    bit_depth: 16,
                    calibration: Calibration::default(),
                }),
            },
        }
    }
}

#[async_trait]
impl CameraDriver for MockDriver {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn spec(&self) -> CameraSpec {
        self.spec.clone()
    }

    async fn open(&self) -> anyhow::Result<Box<dyn CameraSession>> {
        Ok(Box::new(MockSession {
            spec: self.spec.clone(),
            n: 0,
        }))
    }

    async fn write_settings(
        &self,
        changes: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "ok": true,
            "applied": true,
            "requested": changes.get("ip").cloned().unwrap_or(serde_json::Value::Null),
            "note": "simulated camera: settings accepted",
        }))
    }
}

struct MockSession {
    spec: CameraSpec,
    n: u64,
}

#[async_trait]
impl CameraSession for MockSession {
    fn spec(&self) -> &CameraSpec {
        &self.spec
    }

    async fn next_frame(&mut self) -> anyhow::Result<CameraFrame> {
        sleep(Duration::from_millis(66)).await; // ~15 fps
        self.n += 1;
        Ok(CameraFrame {
            stream: StreamKind::Thermal,
            seq: self.n,
            at: std::time::Instant::now(),
            payload: FramePayload::Thermal(synth(self.n)),
        })
    }

    async fn close(&mut self) {}
}

fn synth(n: u64) -> ThermalImage {
    let t = n as f32 * 0.07;
    let hot_x = 80.0 + 46.0 * t.cos();
    let hot_y = 60.0 + 30.0 * t.sin();
    let cool_x = 80.0 - 50.0 * (t * 0.6).cos();
    let cool_y = 60.0 - 28.0 * (t * 0.6).sin();

    let mut raw = vec![0u8; (WIDTH * HEIGHT) as usize * 2];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let base = 1750.0 + 3.0 * x as f32 + 2.0 * y as f32; // 17.5 .. 22.5 C
            let dx = x as f32 - hot_x;
            let dy = y as f32 - hot_y;
            let hot = 1500.0 * (-(dx * dx + dy * dy) / 260.0).exp(); // up to +15 C
            let dcx = x as f32 - cool_x;
            let dcy = y as f32 - cool_y;
            let cool = 420.0 * (-(dcx * dcx + dcy * dcy) / 300.0).exp(); // down to -4 C
            let v = (base + hot - cool).max(0.0) as u16;
            let i = (y * WIDTH + x) as usize * 2;
            raw[i..i + 2].copy_from_slice(&v.to_le_bytes());
        }
    }
    ThermalImage {
        width: WIDTH,
        height: HEIGHT,
        raw: Bytes::from(raw),
        calibration: Calibration::default(),
    }
}
