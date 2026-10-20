//! Built-in radiometric profiles.
//!
//! Profiles are data. Adding a camera means adding a table entry and, if its
//! layout is new, one `FrameLayout` variant — not copying a driver.
//!
//! Every formula here is ported from a public reverse-engineering project.
//! None of these profiles has been validated against hardware by this project.

use crate::camera::{Confidence, ProfileInfo};

use super::types::{
    DeviceTemp, Endian, FrameLayout, RadiometricProfile, RadiometrySpec, RawMode, StreamConfig,
    ThermalHalf,
};

const TC002C_DEVICE_TEMP: DeviceTemp = DeviceTemp {
    row: 0,
    offset: 2,
    divisor: 44.0,
    offset_c: -3.0,
    big_endian: false,
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
        raw_mode: None,
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
        raw_mode: None,
    },
    visible: false,
    radiometric: true,
};

/// TOPDON TC002C DUO. The public decoder is empirical and does not publish a
/// VID/PID, so this profile is explicit-selection only.
pub static TOPDON_TC002C: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "topdon_tc002c",
        display_name: "TOPDON TC002C DUO",
        vendor: "TOPDON",
        model: "TC002C DUO",
        tested: false,
        confidence: Confidence::Inferred,
        sources: &["Weaver32/TC002C-DUO-Viewer"],
        notes: "256x192, 256x392 YUYV. The device temperature is in metadata \
                row 192; the mapping is the author's empirical fit, not vendor \
                calibration. VID/PID is unknown; select this profile explicitly.",
    },
    layout: FrameLayout::PackedYuyvU16 {
        width: 256,
        height: 192,
        frame_height: 392,
        thermal_rows: 192,
        metadata_rows: 4,
        image_start_row: 196,
        device_temp: Some(TC002C_DEVICE_TEMP),
    },
    radiometry: RadiometrySpec::EmpiricalDeviceTemp {
        scale: 0.03125, // 1/32
        offset: -171.0,
        device_temp: TC002C_DEVICE_TEMP,
    },
    stream: StreamConfig {
        width: Some(256),
        height: Some(392),
        fourcc: *b"YUYV",
        fps: Some(25),
        raw_mode: None,
    },
    visible: false,
    radiometric: true,
};

/// Hti HT-301, 384x288. The raw mode is triggered through the UVC zoom unit.
pub static HT301: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "ht301",
        display_name: "Hti HT-301",
        vendor: "Hti",
        model: "HT-301",
        tested: false,
        confidence: Confidence::Documented,
        sources: &[
            "stawel/ht301_hacklib",
            "MCMH2000/OpenHD_HT301_Driver",
            "diminDDL/InfiCamPlus device table",
        ],
        notes: "384x288, 1514:0001. Four metadata rows after the thermal block; \
                per-frame table calibration and a zoom-unit raw-mode command.",
    },
    layout: FrameLayout::RawU16WithMetadata {
        width: 384,
        height: 288,
        metadata_rows: 4,
        raw_mode: RawMode::V4l2Zoom(0x8004),
    },
    radiometry: RadiometrySpec::TableFromFrame {
        metadata_row: 0,
        fpa_off: 7800.0,
        fpa_div: 36.0,
        cal00_offset: 390.0,
        cal00_fpamul: 7.05,
        range_c: 120,
    },
    stream: StreamConfig {
        width: Some(384),
        height: Some(292),
        fourcc: *b"YUYV",
        fps: Some(25),
        raw_mode: Some(0x8004),
    },
    visible: false,
    radiometric: true,
};

/// Xtherm T2S+. T2S+ v1 reports 1514:xxxx; v2 reports 04b4:0100. Plain T2S
/// has no documented PID, so the profile is also selectable explicitly.
pub static XTHERM_T2S: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "xtherm_t2s",
        display_name: "Xtherm T2S+",
        vendor: "InfiRay Xtherm",
        model: "T2S+",
        tested: false,
        confidence: Confidence::Inferred,
        sources: &[
            "stawel/ht301_hacklib (T2S+ support)",
            "diminDDL/InfiCamPlus (T2S+ v1/v2)",
        ],
        notes: "256x192, same HT301-family raw U16 + 4 metadata rows. T2S+ v2 \
                is 04b4:0100; v1 PID and plain T2S PID are unknown.",
    },
    layout: FrameLayout::RawU16WithMetadata {
        width: 256,
        height: 192,
        metadata_rows: 4,
        raw_mode: RawMode::V4l2Zoom(0x8004),
    },
    radiometry: RadiometrySpec::TableFromFrame {
        metadata_row: 0,
        fpa_off: 8617.0,
        fpa_div: 37.682,
        cal00_offset: 170.0,
        cal00_fpamul: 0.0,
        range_c: 120,
    },
    stream: StreamConfig {
        width: Some(256),
        height: Some(196),
        fourcc: *b"YUYV",
        fps: Some(25),
        raw_mode: Some(0x8004),
    },
    visible: false,
    radiometric: true,
};

/// HIKMICRO Mini2. The public viewer only extracts the Y channel and applies
/// a per-unit polynomial; no absolute radiometry is proven here.
pub static HIK_MINI2: RadiometricProfile = RadiometricProfile {
    info: ProfileInfo {
        id: "hik_mini2",
        display_name: "HIKMICRO Mini2",
        vendor: "HIKMICRO",
        model: "Mini2",
        tested: false,
        confidence: Confidence::Inferred,
        sources: &["creator72/hikmicro-thermal"],
        notes: "256x192 YUYV; thermal data is the Y channel (8-bit). Public code \
                uses a per-unit polynomial not shipped here, so this profile is \
                a relative image only and sets radiometric=false.",
    },
    layout: FrameLayout::YuyvY8 {
        width: 256,
        height: 192,
        frame_height: 192,
    },
    radiometry: RadiometrySpec::Unknown,
    stream: StreamConfig {
        width: Some(256),
        height: Some(192),
        fourcc: *b"YUYV",
        fps: Some(25),
        raw_mode: None,
    },
    visible: false,
    radiometric: false,
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
        raw_mode: None,
    },
    visible: false,
    radiometric: false,
};

pub static PROFILES: &[RadiometricProfile] = &[
    TOPDON_TC001,
    INFIRAY_P2PRO,
    TOPDON_TC002C,
    HT301,
    XTHERM_T2S,
    HIK_MINI2,
    UVC_GENERIC,
];

pub fn profile_by_id(id: &str) -> Option<&'static RadiometricProfile> {
    PROFILES.iter().find(|p| p.info.id == id)
}

pub fn profile_ids() -> Vec<&'static str> {
    PROFILES.iter().map(|p| p.info.id).collect()
}
