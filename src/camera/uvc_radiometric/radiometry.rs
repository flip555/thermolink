//! Per-frame radiometry.
//!
//! Linear profiles use `Calibration`; table profiles build a 65,536-entry
//! Celsius LUT from the frame's metadata. TC002C applies an empirical
//! device-temperature offset. All formulas are ported from public
//! reverse-engineering projects and remain untested against hardware.

use std::sync::Arc;

use crate::camera::Calibration;

use super::layout::ExtractedFrame;
use super::types::{DeviceTemp, RadiometrySpec};

impl RadiometrySpec {
    /// Resolve the calibration and optional LUT for one extracted frame.
    pub fn for_frame(self, frame: &ExtractedFrame) -> (Calibration, Option<Arc<[f32]>>) {
        match self {
            RadiometrySpec::Linear { scale, offset } => (Calibration { scale, offset }, None),
            RadiometrySpec::EmpiricalDeviceTemp {
                scale,
                offset,
                device_temp,
            } => {
                let dev = device_temp_c(frame, device_temp).unwrap_or(0.0);
                (
                    Calibration {
                        scale,
                        offset: offset + dev * 1.1,
                    },
                    None,
                )
            }
            RadiometrySpec::TableFromFrame {
                metadata_row,
                fpa_off,
                fpa_div,
                cal00_offset,
                cal00_fpamul,
                range_c,
            } => {
                let table = build_table(
                    frame,
                    metadata_row,
                    fpa_off,
                    fpa_div,
                    cal00_offset,
                    cal00_fpamul,
                    range_c,
                );
                (
                    Calibration {
                        scale: 1.0,
                        offset: 0.0,
                    },
                    table,
                )
            }
            RadiometrySpec::Unknown => (
                Calibration {
                    scale: 1.0,
                    offset: 0.0,
                },
                None,
            ),
        }
    }
}

fn u16_at(bytes: &[u8], index: usize) -> Option<u16> {
    let off = index.checked_mul(2)?;
    let pair = bytes.get(off..off + 2)?;
    Some(u16::from_le_bytes([pair[0], pair[1]]))
}

fn f32_at(bytes: &[u8], index: usize) -> Option<f32> {
    let off = index.checked_mul(2)?;
    let quad = bytes.get(off..off + 4)?;
    Some(f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]))
}

fn device_temp_c(frame: &ExtractedFrame, spec: DeviceTemp) -> Option<f32> {
    let meta = frame.metadata.as_deref()?;
    let off = spec.row as usize * frame.width as usize * 2 + spec.offset as usize;
    let pair = meta.get(off..off + 2)?;
    let raw = if spec.big_endian {
        u16::from_be_bytes([pair[0], pair[1]])
    } else {
        u16::from_le_bytes([pair[0], pair[1]])
    };
    Some(raw as f32 / spec.divisor + spec.offset_c)
}

fn wvc(humidity: f32, air: f32) -> f32 {
    humidity * (1.5587 + 0.06939 * air - 2.7816e-4 * air.powi(2) + 6.8455e-7 * air.powi(3)).exp()
}

/// Atmospheric transmittance from humidity, ambient temperature and distance.
fn atmt(humidity: f32, air: f32, distance: f32) -> f32 {
    let k_atm = 1.9;
    let nsqd = -distance.max(0.0).sqrt();
    let sqw = wvc(humidity, air).max(0.0).sqrt();
    let a1 = 0.006569;
    let a2 = 0.01262;
    let b1 = -0.002276;
    let b2 = -0.00667;
    k_atm * (nsqd * (a1 + b1 * sqw)).exp() + (1.0 - k_atm) * (nsqd * (a2 + b2 * sqw)).exp()
}

#[allow(clippy::too_many_arguments)]
fn build_table(
    frame: &ExtractedFrame,
    _metadata_row: u32,
    fpa_off: f32,
    fpa_div: f32,
    cal00_offset: f32,
    cal00_fpamul: f32,
    range_c: u16,
) -> Option<Arc<[f32]>> {
    let meta = frame.metadata.as_deref()?;
    let width = frame.width as usize;
    // ht301_hacklib: 640/384 use three metadata rows before the calibration
    // block; 256/240 use one. The captured metadata is four rows.
    let amount_pixels = if width >= 384 { width * 3 } else { width };

    let shut = u16_at(meta, amount_pixels + 1)? as f32;
    let cal_00 = u16_at(meta, amount_pixels)? as f32;
    let cal_01 = f32_at(meta, amount_pixels + 3)?;
    let cal_02 = f32_at(meta, amount_pixels + 5)?;
    let cal_03 = f32_at(meta, amount_pixels + 7)?;
    let cal_04 = f32_at(meta, amount_pixels + 9)?;
    let cal_05 = f32_at(meta, amount_pixels + 11)?;

    let user_area = amount_pixels + 127;
    let correction = f32_at(meta, user_area)?;
    let reflected = f32_at(meta, user_area + 2)?;
    let air = f32_at(meta, user_area + 4)?;
    let humidity = f32_at(meta, user_area + 6)?;
    let emissivity = f32_at(meta, user_area + 8)?;
    let distance = u16_at(meta, user_area + 10)? as f32;

    let fpa_tmp = u16_at(meta, 1)? as f32;

    if cal_01.abs() < 1e-12 {
        return None;
    }
    let ts = shut / 10.0 - 273.15;
    let tfpa = 20.0 - (fpa_tmp - fpa_off) / fpa_div;

    let cal_a = cal_02 / (2.0 * cal_01);
    let cal_b = cal_02 * cal_02 / (4.0 * cal_01 * cal_01);
    let cal_c = cal_01 * ts * ts + ts * cal_02;
    let cal_d = cal_03 * tfpa * tfpa + cal_04 * tfpa + cal_05;

    let cal_00_corr = if range_c == 120 {
        cal00_offset - tfpa * cal00_fpamul
    } else {
        0.0
    };
    let table_offset = cal_00 - cal_00_corr.max(0.0);
    let distance_adjusted = if distance >= 20.0 { 20.0 } else { distance };
    let atm = atmt(humidity, air, distance_adjusted);
    let denominator = emissivity * atm;
    if denominator.abs() < 1e-9 {
        return None;
    }
    let numerator_sub = (1.0 - emissivity) * atm * (reflected + 273.15).powi(4)
        + (1.0 - atm) * (air + 273.15).powi(4);

    let mut table = vec![f32::NAN; 65_536];
    for (raw, slot) in table.iter_mut().take(16_384).enumerate() {
        let inner = ((raw as f32 - table_offset) * cal_d + cal_c) / cal_01 + cal_b;
        let n = inner.abs().sqrt();
        let wtot = (n - cal_a + 273.15).powi(4);
        let ttot = ((wtot - numerator_sub) / denominator).max(0.0).powf(0.25) - 273.15;
        let value = ttot + (distance_adjusted * 0.85 - 1.125) * (ttot - air) / 100.0 + correction;
        *slot = value;
    }
    // The sensor is 14-bit; keep a finite tail for unexpected raw values.
    let last = table[16_383];
    for slot in table.iter_mut().skip(16_384) {
        *slot = last;
    }
    Some(Arc::from(table))
}
