//! Configuration shared by every driver.
//!
//! Driver-specific extras live in `camera.options`, so adding a camera family
//! never changes this file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub camera: CameraConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraConfig {
    /// Driver id, e.g. `qianli`.
    pub driver: String,
    /// Camera address. IP literal for now; hostnames may resolve to several
    /// addresses, which a session cannot use unambiguously.
    pub host: String,
    /// Control / registration port.
    pub control_port: u16,
    /// Media port.
    pub media_port: u16,
    /// Seconds to wait before reconnecting after a session ends.
    pub reconnect_secs: u64,
    /// Driver-specific knobs (width/height overrides, calibration, ...).
    pub options: serde_json::Value,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            driver: "qianli".into(),
            host: "192.168.1.64".into(),
            control_port: 3001,
            media_port: 3000,
            reconnect_secs: 8,
            options: serde_json::json!({}),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// HTTP listen address for the future web UI.
    pub bind: String,
    /// JPEG quality used when the server has to transcode a thermal frame.
    pub jpeg_quality: u8,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8099".into(),
            jpeg_quality: 80,
        }
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}
