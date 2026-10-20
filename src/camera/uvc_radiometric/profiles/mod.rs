//! Built-in radiometric profiles.
//!
//! Profiles are data. Adding a camera means adding a table entry and, if its
//! layout is new, one `FrameLayout` variant — not copying a driver.

use crate::camera::{Confidence, ProfileInfo};

use super::types::{
    Endian, FrameLayout, RadiometricProfile, RadiometrySpec, StreamConfig, ThermalHalf,
};

pub static TOPDON_TC001: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "topdon_tc001",
        display_name: "TOPDON TC001",
        vendor: "TOPDON",
        model: "TC001",
        tested: false,
        confidence: Confidence::Documented,
        sources: &[
            "leswright1977/PyThermalCamera tc001v4.2.py",
            "92es/Thermal-Camera-Redux",
            "LeoDJ/P2Pro-Viewer",
        ],
        notes: "256x192, 0bda:5830. Shares VID/PID and layout with the P2 Pro; \
                auto-detect must show the ambiguity. The visible half is present \
                but not exposed in M1.",
    },
    layout: FrameLayout::SplitYuyvU16 {
        width: 256,
        height: 192,
        frame_height: 384,
        thermal_half: ThermalHalf::Bottom,
        endian: Endian::Little,
    },
    radiometry: RadiometrySpec::Linear {
        scale: 0.015625, // 1/64 K
        offset: -273.15,
    },
    stream: StreamConfig {
        width: Some(256),
        height: Some(384),
        fourcc: *b"YUYV",
        fps: Some(25),
    },
    visible: false,
    radiometric: true,
};

pub static INFIRAY_P2PRO: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "infiray_p2pro",
        display_name: "InfiRay P2 Pro",
        vendor: "InfiRay",
        model: "P2 Pro",
        tested: false,
        confidence: Confidence::Documented,
        sources: &[
            "LeoDJ/P2Pro-Viewer P2Pro/video.py",
            "LeoDJ/P2Pro-Viewer P2Pro/P2Pro_cmd.py",
        ],
        notes: "256x192, 0bda:5830, 25 fps. Same split-frame layout and 1/64 K \
                calibration as the TC001. Vendor controls are documented but not \
                exposed in M1.",
    },
    layout: FrameLayout::SplitYuyvU16 {
        width: 256,
        height: 192,
        frame_height: 384,
        thermal_half: ThermalHalf::Bottom,
        endian: Endian::Little,
    },
    radiometry: RadiometrySpec::Linear {
        scale: 0.015625,
        offset: -273.15,
    },
    stream: StreamConfig {
        width: Some(256),
        height: Some(384),
        fourcc: *b"YUYV",
        fps: Some(25),
    },
    visible: false,
    radiometric: true,
};

/// Fallback for an unknown UVC device. It streams a best-effort raw view and
/// never claims calibrated temperatures.
pub static UVC_GENERIC: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "uvc_generic",
        display_name: "Generic UVC radiometric (uncalibrated)",
        vendor: "Generic",
        model: "UVC",
        tested: false,
        confidence: Confidence::Unknown,
        sources: &[],
        notes: "Fallback only. The frame layout and calibration are unknown; \
                temperature measurement is disabled and the UI must say so.",
    },
    layout: FrameLayout::UnknownRawU16 {
        width: 0,
        height: 0,
    },
    radiometry: RadiometrySpec::Unknown,
    stream: StreamConfig {
        width: None,
        height: None,
        fourcc: *b"YUYV",
        fps: None,
    },
    visible: false,
    radiometric: false,
};

pub static PROFILES: &[RadiometricProfile] = &[TOPDON_TC001, INFIRAY_P2PRO, UVC_GENERIC];

pub fn profile_by_id(id: &str) -> Option<&'static RadiometricProfile> {
    PROFILES.iter().find(|p| p.info.id == id)
}

pub fn profile_ids() -> Vec<&'static str> {
    PROFILES.iter().map(|p| p.info.id).collect()
}
