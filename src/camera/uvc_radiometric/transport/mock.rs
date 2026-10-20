//! Deterministic mock UVC transport for CI.

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use super::{UvcStream, UvcTransport};
use crate::camera::uvc_radiometric::types::{StreamConfig, StreamInfo, UvcDevice};

#[derive(Debug, Clone, Default)]
pub struct MockTransport {
    pub devices: Vec<UvcDevice>,
    pub frames: Vec<Bytes>,
}

impl MockTransport {
    pub fn new(devices: Vec<UvcDevice>, frames: Vec<Bytes>) -> Self {
        Self { devices, frames }
    }

    pub fn single(device: UvcDevice, frame: Bytes) -> Self {
        Self {
            devices: vec![device],
            frames: vec![frame],
        }
    }
}

#[async_trait]
impl UvcTransport for MockTransport {
    async fn enumerate(&self) -> Result<Vec<UvcDevice>> {
        Ok(self.devices.clone())
    }

    async fn open(&self, _device: &UvcDevice, config: StreamConfig) -> Result<Box<dyn UvcStream>> {
        let width = config.width.unwrap_or(256);
        let height = config.height.unwrap_or(384);
        let frames = if self.frames.is_empty() {
            vec![Bytes::from(vec![0u8; width as usize * height as usize * 2])]
        } else {
            self.frames.clone()
        };
        Ok(Box::new(MockStream {
            info: StreamInfo {
                width,
                height,
                fourcc: config.fourcc,
                fps: config.fps,
            },
            frames,
            index: 0,
        }))
    }
}

pub struct MockStream {
    info: StreamInfo,
    frames: Vec<Bytes>,
    index: usize,
}

#[async_trait]
impl UvcStream for MockStream {
    fn info(&self) -> StreamInfo {
        self.info
    }

    async fn next_frame(&mut self) -> Result<Bytes> {
        let frame = self.frames[self.index % self.frames.len()].clone();
        self.index = self.index.wrapping_add(1);
        Ok(frame)
    }

    async fn close(&mut self) {}
}
