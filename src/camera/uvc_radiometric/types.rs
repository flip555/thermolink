//! Shared types for the unified UVC radiometric driver family.
//!
//! A profile is data, not a driver implementation. Transport, frame layout,
//! radiometry and controls are composed from these types.

use serde::{Deserialize, Serialize};

use crate::camera::{Calibration, Capabilities, Confidence, DeviceInfo, ProfileInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    Little,
    Big,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThermalHalf {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RawMode {
    None,
    /// Vendor abuse of the UVC zoom control; value is the packed command id.
    V4l2Zoom(u16),
}

/// How thermal pixels are laid out inside one UVC frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameLayout {
    /// TC001 / P2 Pro: 256x384 YUYV; one half is the visible image, the
    /// other half is u16 raw. The thermal half is selected explicitly.
    SplitYuyvU16 {
        width: u32,
        height: u32,
        frame_height: u32,
        thermal_half: ThermalHalf,
        endian: Endian,
    },
    /// TC002C: raw16 packed into YUYV byte pairs, followed by metadata rows
    /// and a grayscale image.
    PackedYuyvU16 {
        width: u32,
        height: u32,
        frame_height: u32,
        thermal_rows: u32,
        metadata_rows: u32,
        image_start_row: u32,
        device_temp: Option<DeviceTemp>,
    },
    /// HT301 / T2S: u16 thermal block followed by metadata rows.
    RawU16WithMetadata {
        width: u32,
        height: u32,
        metadata_rows: u32,
        raw_mode: RawMode,
    },
    /// Best-effort fallback: treat the frame as u16 samples. No calibration
    /// claim is made; `radiometric` must be false.
    UnknownRawU16 { width: u32, height: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DeviceTemp {
    /// Row within the thermal block that contains the device temperature.
    pub row: u32,
    /// Byte offset within that row.
    pub offset: u32,
    pub divisor: f32,
    pub offset_c: f32,
}

/// How raw samples become Celsius.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RadiometrySpec {
    Linear {
        scale: f32,
        offset: f32,
    },
    /// HT301 / T2S family: coefficients live in the per-frame metadata.
    TableFromFrame {
        metadata_row: u32,
        fpa_off: f32,
        fpa_div: f32,
        cal00_offset: f32,
        cal00_fpamul: f32,
        range_c: u16,
    },
    /// TC002C: author-derived linear mapping plus device-temperature
    /// compensation. Explicitly empirical, not vendor calibration.
    EmpiricalDeviceTemp {
        scale: f32,
        offset: f32,
        device_temp: DeviceTemp,
    },
    Unknown,
}

impl RadiometrySpec {
    /// The linear fallback stored on `ThermalImage`. Table/empirical profiles
    /// add their own handling in Phase 3; M1 only needs Linear and Unknown.
    pub fn calibration(self) -> Calibration {
        match self {
            RadiometrySpec::Linear { scale, offset } => Calibration { scale, offset },
            _ => Calibration {
                scale: 1.0,
                offset: 0.0,
            },
        }
    }

    pub fn is_radiometric(self) -> bool {
        !matches!(self, RadiometrySpec::Unknown)
    }
}

/// Requested UVC stream format. `None` dimensions mean "negotiate".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamConfig {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fourcc: [u8; 4],
    pub fps: Option<u32>,
}

/// Negotiated UVC stream format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fourcc: [u8; 4],
    pub fps: Option<u32>,
}

/// One UVC device as seen by the transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UvcDevice {
    pub vid: u16,
    pub pid: u16,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub path: Option<String>,
    pub bus: Option<String>,
}

impl UvcDevice {
    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            vid: self.vid,
            pid: self.pid,
            product: self.product.clone(),
            serial: self.serial.clone(),
            path: self.path.clone(),
        }
    }

    pub fn pretty_id(&self) -> String {
        format!("{:04x}:{:04x}", self.vid, self.pid)
    }
}

/// A complete camera profile: identity + layout + radiometry + stream request.
#[derive(Debug, Clone, Copy)]
pub struct RadiometricProfile {
    pub info: ProfileInfo,
    pub layout: FrameLayout,
    pub radiometry: RadiometrySpec,
    pub stream: StreamConfig,
    pub visible: bool,
    pub radiometric: bool,
}

impl RadiometricProfile {
    pub fn capabilities(self) -> Capabilities {
        Capabilities {
            thermal: true,
            visible: self.visible,
            radiometric: self.radiometric && self.radiometry.is_radiometric(),
            discover: true,
            network_settings: false,
            reboot: false,
            controls: Vec::new(),
        }
    }

    pub fn tested(self) -> bool {
        self.info.tested
    }

    pub fn confidence(self) -> Confidence {
        self.info.confidence
    }
}
