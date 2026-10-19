//! Palette rendering for radiometric frames.
//!
//! Two independent bands, as in the original:
//!
//! * `clip_lo`/`clip_hi` decide what is *shown*; outside is drawn white.
//! * `span_lo`/`span_hi` decide what the palette spans. Unset means the span
//!   follows the data that survives the clip, so colours always use the full
//!   range of what you are actually looking at.
//!
//! With everything unset: colours across the frame's own min..max.

use serde::{Deserialize, Serialize};

use crate::camera::ThermalImage;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    pub palette: String,
    pub clip_lo: Option<f32>,
    pub clip_hi: Option<f32>,
    pub span_lo: Option<f32>,
    pub span_hi: Option<f32>,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            palette: "rainbow".into(),
            clip_lo: None,
            clip_hi: None,
            span_lo: None,
            span_hi: None,
        }
    }
}

pub const PALETTES: &[&str] = &["rainbow", "ironbow", "whitehot", "blackhot"];

fn ramp(stops: &[(u16, [u8; 3])]) -> [[u8; 3]; 256] {
    let mut out = [[0u8; 3]; 256];
    for (i, slot) in out.iter_mut().enumerate() {
        let i = i as u16;
        let mut chosen = stops.last().map(|s| s.1).unwrap_or([255, 255, 255]);
        for pair in stops.windows(2) {
            let (p0, c0) = pair[0];
            let (p1, c1) = pair[1];
            if p0 <= i && i <= p1 {
                let t = if p1 == p0 {
                    0.0
                } else {
                    (i - p0) as f32 / (p1 - p0) as f32
                };
                chosen = [
                    (c0[0] as f32 + (c1[0] as f32 - c0[0] as f32) * t) as u8,
                    (c0[1] as f32 + (c1[1] as f32 - c0[1] as f32) * t) as u8,
                    (c0[2] as f32 + (c1[2] as f32 - c0[2] as f32) * t) as u8,
                ];
                break;
            }
        }
        *slot = chosen;
    }
    out
}

fn lut(name: &str) -> [[u8; 3]; 256] {
    match name {
        "ironbow" => ramp(&[
            (0, [0, 0, 0]),
            (64, [90, 0, 130]),
            (128, [200, 50, 20]),
            (192, [250, 180, 30]),
            (256, [255, 255, 255]),
        ]),
        "whitehot" => ramp(&[(0, [0, 0, 0]), (256, [255, 255, 255])]),
        "blackhot" => ramp(&[(0, [255, 255, 255]), (256, [0, 0, 0])]),
        _ => ramp(&[
            (0, [0, 0, 40]),
            (48, [0, 90, 200]),
            (110, [0, 200, 160]),
            (170, [240, 220, 40]),
            (215, [250, 120, 20]),
            (256, [255, 255, 255]),
        ]),
    }
}

/// Rendered RGB pixels (width * height * 3).
pub fn render(img: &ThermalImage, settings: &DisplaySettings) -> Vec<u8> {
    let vals: Vec<u16> = img.values().collect();
    let cal = img.calibration;
    let to_raw = |c: Option<f32>| c.map(|c| ((c - cal.offset) / cal.scale).round() as i32);

    let clip_lo = to_raw(settings.clip_lo);
    let clip_hi = to_raw(settings.clip_hi);

    let mut shown_min = u16::MAX;
    let mut shown_max = 0u16;
    let mut any_shown = false;
    for &v in &vals {
        let vi = v as i32;
        if clip_lo.is_some_and(|lo| vi < lo) || clip_hi.is_some_and(|hi| vi > hi) {
            continue;
        }
        shown_min = shown_min.min(v);
        shown_max = shown_max.max(v);
        any_shown = true;
    }
    if !any_shown {
        shown_min = *vals.iter().min().unwrap_or(&0);
        shown_max = *vals.iter().max().unwrap_or(&0);
    }

    let span_lo = to_raw(settings.span_lo).unwrap_or(shown_min as i32);
    let span_hi = to_raw(settings.span_hi).unwrap_or(shown_max as i32);
    let span = (span_hi - span_lo).max(1);

    let palette = lut(&settings.palette);
    let mut rgb = vec![0u8; vals.len() * 3];
    for (i, &v) in vals.iter().enumerate() {
        let vi = v as i32;
        let pixel = if clip_lo.is_some_and(|lo| vi < lo) || clip_hi.is_some_and(|hi| vi > hi) {
            [255, 255, 255]
        } else {
            let idx = (((vi - span_lo) * 255) / span).clamp(0, 255) as usize;
            palette[idx]
        };
        rgb[i * 3..i * 3 + 3].copy_from_slice(&pixel);
    }
    rgb
}

pub fn render_jpeg(
    img: &ThermalImage,
    settings: &DisplaySettings,
    quality: u8,
) -> anyhow::Result<Vec<u8>> {
    let rgb = render(img, settings);
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100));
    encoder.encode(
        &rgb,
        img.width as u16,
        img.height as u16,
        jpeg_encoder::ColorType::Rgb,
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::camera::thermal_test_image;

    #[test]
    fn clip_paints_outside_white() {
        let img = thermal_test_image(&[1000, 2000, 3000, 4000], 2, 2);
        let settings = DisplaySettings {
            clip_lo: Some(15.0),
            clip_hi: Some(35.0),
            ..Default::default()
        };
        let rgb = render(&img, &settings);
        // 10 C and 40 C are clipped; 20 C and 30 C are inside.
        assert_eq!(&rgb[0..3], &[255, 255, 255]);
        assert_ne!(&rgb[3..6], &[255, 255, 255]);
        assert_ne!(&rgb[6..9], &[255, 255, 255]);
        assert_eq!(&rgb[9..12], &[255, 255, 255]);
    }

    #[test]
    fn span_controls_colour_mapping() {
        // Three samples, so a pinned span that is wider than the data maps the
        // middle one differently from the automatic span.
        let img = thermal_test_image(&[2000, 2500, 3000], 3, 1);
        let auto = DisplaySettings::default();
        let pinned = DisplaySettings {
            span_lo: Some(20.0),
            span_hi: Some(40.0),
            ..Default::default()
        };
        assert_ne!(render(&img, &auto), render(&img, &pinned));
    }

    #[test]
    fn encodes_a_jpeg() {
        let img = thermal_test_image(&vec![2500u16; 160 * 120], 160, 120);
        let jpeg = render_jpeg(&img, &DisplaySettings::default(), 80).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert!(jpeg.len() > 500);
    }
}

/// Signed difference image: blue = colder than the reference, white = same,
/// red = hotter, clamped to +/- `span_c`.
pub fn render_dt(img: &ThermalImage, reference: &[u16], span_c: f32) -> Vec<u8> {
    let n = img.pixel_count();
    let mut rgb = vec![0u8; n * 3];
    let span = if span_c.abs() < 0.01 { 1.0 } else { span_c.abs() };
    for (i, v) in img.values().enumerate() {
        let Some(&r) = reference.get(i) else {
            rgb[i * 3..i * 3 + 3].copy_from_slice(&[255, 255, 255]);
            continue;
        };
        let d = (v as f32 - r as f32) * img.calibration.scale;
        let t = (d / span).clamp(-1.0, 1.0);
        let colour = if t < 0.0 {
            let k = 1.0 + t; // 0 at -span, 1 at zero
            [
                (255.0 * k) as u8,
                (255.0 * k) as u8,
                255,
            ]
        } else {
            [
                255,
                (255.0 * (1.0 - t)) as u8,
                (255.0 * (1.0 - t)) as u8,
            ]
        };
        rgb[i * 3..i * 3 + 3].copy_from_slice(&colour);
    }
    rgb
}

pub fn dt_jpeg(
    img: &ThermalImage,
    reference: &[u16],
    span_c: f32,
    quality: u8,
) -> anyhow::Result<Vec<u8>> {
    let rgb = render_dt(img, reference, span_c);
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100));
    encoder.encode(
        &rgb,
        img.width as u16,
        img.height as u16,
        jpeg_encoder::ColorType::Rgb,
    )?;
    Ok(out)
}
