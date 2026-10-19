//! Maps a driver id from configuration to a concrete implementation.

use anyhow::{bail, Result};

use super::mock::MockDriver;
use super::qianli::QianliDriver;
use super::p3::P3Driver;
use super::CameraDriver;
use crate::config::CameraConfig;

pub const KNOWN: &[&str] = &["qianli", "p3", "mock"];

pub fn build(cfg: &CameraConfig) -> Result<Box<dyn CameraDriver>> {
    match cfg.driver.as_str() {
        "qianli" | "qianli-supercam" | "supercam" | "lc-ad11" => {
            Ok(Box::new(QianliDriver::from_config(cfg)?))
        }
        "p3" | "p3-ir" | "p1" => Ok(Box::new(P3Driver::from_config(cfg)?)),
        "mock" | "sim" | "simulated" => Ok(Box::new(MockDriver::new())),
        other => bail!(
            "unknown camera driver {other:?}; known drivers: {}",
            KNOWN.join(", ")
        ),
    }
}
