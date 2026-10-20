//! Maps a driver id from configuration to a concrete implementation.

use anyhow::{bail, Result};

use super::mock::MockDriver;
use super::p3::P3Driver;
use super::qianli::QianliDriver;
use super::uvc_radiometric::{self, UvcRadiometricDriver};
use super::CameraDriver;
use crate::config::CameraConfig;

pub const KNOWN: &[&str] = &["qianli", "p3", "uvc_radiometric", "mock"];

pub fn build(cfg: &CameraConfig) -> Result<Box<dyn CameraDriver>> {
    match cfg.driver.as_str() {
        "qianli" | "qianli-supercam" | "supercam" | "lc-ad11" => {
            Ok(Box::new(QianliDriver::from_config(cfg)?))
        }
        "p3" | "p3-ir" | "p1" => Ok(Box::new(P3Driver::from_config(cfg)?)),
        "uvc_radiometric" | "uvc" | "radiometric" => {
            Ok(Box::new(UvcRadiometricDriver::from_config(cfg)?))
        }
        "mock" | "sim" | "simulated" => Ok(Box::new(MockDriver::new())),
        other => bail!(
            "unknown camera driver {other:?}; known drivers: {}",
            KNOWN.join(", ")
        ),
    }
}

/// Every compiled radiometric profile, for `/api/drivers` and the UI.
pub fn profiles() -> &'static [uvc_radiometric::RadiometricProfile] {
    uvc_radiometric::PROFILES
}

pub fn is_uvc(driver: &str) -> bool {
    matches!(driver, "uvc_radiometric" | "uvc" | "radiometric")
}

/// Resolve an auto-detected profile before building the driver. The registry
/// is the only module allowed to know about concrete driver families.
pub async fn resolve(cfg: &mut CameraConfig) -> Result<()> {
    if is_uvc(&cfg.driver) {
        uvc_radiometric::resolve_config(cfg).await
    } else {
        Ok(())
    }
}
