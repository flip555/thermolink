//! UVC transport abstraction.
//!
//! M1 ships one Linux backend (ffmpeg's `v4l2` input) and a deterministic mock.
//! The trait exists so a direct V4L2 ioctl backend or a Windows/macOS backend
//! can be added without touching profiles or the server.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use super::types::{StreamConfig, StreamInfo, UvcDevice};

pub mod mock;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxV4l2Transport;

#[cfg(not(target_os = "linux"))]
pub struct LinuxV4l2Transport;

#[cfg(not(target_os = "linux"))]
#[async_trait]
impl UvcTransport for LinuxV4l2Transport {
    async fn enumerate(&self) -> Result<Vec<UvcDevice>> {
        anyhow::bail!("UVC transport is currently Linux-only")
    }

    async fn open(&self, _device: &UvcDevice, _config: StreamConfig) -> Result<Box<dyn UvcStream>> {
        anyhow::bail!("UVC transport is currently Linux-only")
    }
}

#[async_trait]
pub trait UvcTransport: Send + Sync {
    /// Enumerate attached UVC devices. Implementations must not open or claim
    /// a device here; a failed probe must not prevent streaming.
    async fn enumerate(&self) -> Result<Vec<UvcDevice>>;

    /// Open one device with the requested format. `None` dimensions mean the
    /// backend negotiates a supported format.
    async fn open(&self, device: &UvcDevice, config: StreamConfig) -> Result<Box<dyn UvcStream>>;
}

#[async_trait]
pub trait UvcStream: Send {
    fn info(&self) -> StreamInfo;

    /// Next complete raw frame. Implementations should time out rather than
    /// hang forever when the camera goes silent.
    async fn next_frame(&mut self) -> Result<Bytes>;

    async fn close(&mut self);
}

pub fn default_transport() -> Arc<dyn UvcTransport> {
    Arc::new(LinuxV4l2Transport)
}
