//! Unified UVC radiometric driver family.
//!
//! One driver, many data-only profiles. The profile owns the frame layout,
//! radiometry and stream request; the transport owns enumeration and raw
//! frames; the server never learns either.

pub mod detect;
pub mod fingerprint;
pub mod layout;
pub mod profiles;
pub mod transport;
pub mod types;

pub use profiles::{profile_by_id, profile_ids, PROFILES};
pub use types::{FrameLayout, RadiometricProfile, RadiometrySpec, StreamConfig, UvcDevice};

use std::time::Instant;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use crate::camera::{
    CameraDriver, CameraFrame, CameraSession, CameraSpec, DeviceInfo, FramePayload, StreamKind,
    ThermalImage, ThermalStream,
};
use crate::config::{CameraConfig, DeviceSelector};

use self::transport::{UvcStream, UvcTransport};

pub const DRIVER_ID: &str = "uvc_radiometric";

pub struct UvcRadiometricDriver {
    cfg: CameraConfig,
    profile: &'static RadiometricProfile,
}

impl UvcRadiometricDriver {
    pub fn from_config(cfg: &CameraConfig) -> Result<Self> {
        let id = cfg.profile.as_deref().unwrap_or("uvc_generic");
        let profile = profile_by_id(id).with_context(|| {
            format!(
                "unknown UVC profile {id:?}; known profiles: {}",
                profile_ids().join(", ")
            )
        })?;
        Ok(Self {
            cfg: cfg.clone(),
            profile,
        })
    }

    fn spec_with(&self, device: Option<DeviceInfo>) -> CameraSpec {
        let (width, height) = self.profile.layout.thermal_size().unwrap_or((0, 0));
        CameraSpec {
            driver: DRIVER_ID,
            model: self.profile.info.display_name,
            video: None,
            thermal: Some(ThermalStream {
                width,
                height,
                bit_depth: 16,
                calibration: self.profile.radiometry.calibration(),
            }),
            profile: Some(self.profile.info),
            device,
            capabilities: self.profile.capabilities(),
        }
    }

    async fn select_device(&self, transport: &dyn UvcTransport) -> Result<UvcDevice> {
        let devices = transport.enumerate().await?;
        if devices.is_empty() {
            bail!(
                "no UVC device found; check the cable, the udev rule and video-group permissions"
            );
        }
        if let Some(path) = self.cfg.device.path.as_deref() {
            if let Some(device) = devices.iter().find(|d| d.path.as_deref() == Some(path)) {
                return Ok(device.clone());
            }
        }
        if let (Some(vid), Some(pid)) = (self.cfg.device.vid, self.cfg.device.pid) {
            if let Some(device) = devices.iter().find(|d| d.vid == vid && d.pid == pid) {
                return Ok(device.clone());
            }
        }
        // Prefer a device that matches this profile's fingerprint.
        for fp in fingerprint::FINGERPRINTS {
            if fp.profile_id != self.profile.info.id {
                continue;
            }
            if let Some(device) = devices.iter().find(|d| d.vid == fp.vid && d.pid == fp.pid) {
                return Ok(device.clone());
            }
        }
        // Explicit profile with an unrecognised device: the user asked for it.
        Ok(devices[0].clone())
    }
}

#[async_trait]
impl CameraDriver for UvcRadiometricDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn spec(&self) -> CameraSpec {
        self.spec_with(None)
    }

    async fn open(&self) -> Result<Box<dyn CameraSession>> {
        let transport = transport::default_transport();
        let device = self.select_device(transport.as_ref()).await?;
        let stream = transport.open(&device, self.profile.stream).await?;
        let info = stream.info();

        let layout = if matches!(self.profile.layout, FrameLayout::UnknownRawU16 { .. }) {
            FrameLayout::UnknownRawU16 {
                width: info.width,
                height: info.height / 2,
            }
        } else {
            self.profile.layout
        };
        let (width, height) = layout
            .thermal_size()
            .unwrap_or((info.width, info.height / 2));

        let mut spec = self.spec_with(Some(device.info()));
        spec.thermal = Some(ThermalStream {
            width,
            height,
            bit_depth: 16,
            calibration: self.profile.radiometry.calibration(),
        });
        spec.capabilities.radiometric =
            self.profile.radiometric && self.profile.radiometry.is_radiometric();

        Ok(Box::new(UvcSession {
            spec,
            stream,
            layout,
            profile: self.profile,
            seq: 0,
        }))
    }

    async fn discover(&self) -> Result<serde_json::Value> {
        let transport = transport::default_transport();
        let devices = transport.enumerate().await.unwrap_or_default();
        let detection = detect::detect(&self.cfg, transport.as_ref()).await;
        Ok(serde_json::json!({
            "hosts": [],
            "devices": devices,
            "candidates": detection.candidates,
            "selected": detection.selected,
            "fallback": detection.fallback,
            "ambiguous": detection.ambiguous,
            "notes": detection.notes,
        }))
    }

    async fn detect(&self) -> Result<serde_json::Value> {
        Ok(detect::detect_json(&self.cfg).await)
    }
}

struct UvcSession {
    spec: CameraSpec,
    stream: Box<dyn UvcStream>,
    layout: FrameLayout,
    profile: &'static RadiometricProfile,
    seq: u64,
}

#[async_trait]
impl CameraSession for UvcSession {
    fn spec(&self) -> &CameraSpec {
        &self.spec
    }

    async fn next_frame(&mut self) -> Result<CameraFrame> {
        let bytes = self.stream.next_frame().await?;
        let extracted = self.layout.extract(&bytes)?;
        self.seq += 1;
        Ok(CameraFrame {
            stream: StreamKind::Thermal,
            seq: self.seq,
            at: Instant::now(),
            payload: FramePayload::Thermal(ThermalImage {
                width: extracted.width,
                height: extracted.height,
                raw: extracted.raw,
                calibration: self.profile.radiometry.calibration(),
                lut: None,
            }),
        })
    }

    async fn close(&mut self) {
        self.stream.close().await;
    }
}

/// Resolve an auto-detected profile before a driver is constructed. This keeps
/// `CameraSpec` honest: the spec is built after detection, not guessed.
pub async fn resolve_config(cfg: &mut CameraConfig) -> Result<()> {
    if let Some(profile) = cfg.profile.as_deref() {
        if profile != "auto" {
            return Ok(());
        }
    }
    if !cfg.auto_detect {
        cfg.profile = Some("uvc_generic".into());
        return Ok(());
    }

    let transport = transport::default_transport();
    let detection = detect::detect(cfg, transport.as_ref()).await;
    if let Some(selected) = detection.selected {
        cfg.profile = Some(selected.profile_id.to_string());
        cfg.device = DeviceSelector {
            vid: Some(selected.device.vid),
            pid: Some(selected.device.pid),
            serial: selected.device.serial.clone(),
            path: selected.device.path.clone(),
        };
    } else {
        cfg.profile = Some("uvc_generic".into());
    }
    Ok(())
}
