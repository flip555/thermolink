//! Frame layout extraction.
//!
//! Every extractor is bounds-checked and returns an error for short or
//! malformed frames rather than panicking.

use anyhow::{bail, ensure, Result};
use bytes::Bytes;

use super::types::{Endian, FrameLayout, ThermalHalf};

#[derive(Debug, Clone)]
pub struct ExtractedFrame {
    pub width: u32,
    pub height: u32,
    pub raw: Bytes,
}

impl FrameLayout {
    pub fn thermal_size(self) -> Option<(u32, u32)> {
        match self {
            FrameLayout::SplitYuyvU16 { width, height, .. } => Some((width, height)),
            FrameLayout::PackedYuyvU16 {
                width,
                thermal_rows,
                ..
            } => Some((width, thermal_rows)),
            FrameLayout::RawU16WithMetadata { width, height, .. } => Some((width, height)),
            FrameLayout::UnknownRawU16 { width, height } => Some((width, height)),
        }
    }

    pub fn frame_size(self) -> Option<(u32, u32)> {
        match self {
            FrameLayout::SplitYuyvU16 {
                width,
                frame_height,
                ..
            } => Some((width, frame_height)),
            FrameLayout::PackedYuyvU16 {
                width,
                frame_height,
                ..
            } => Some((width, frame_height)),
            FrameLayout::RawU16WithMetadata {
                width,
                height,
                metadata_rows,
                ..
            } => Some((width, height + metadata_rows)),
            FrameLayout::UnknownRawU16 { width, height } => Some((width, height)),
        }
    }

    pub fn extract(self, frame: &[u8]) -> Result<ExtractedFrame> {
        match self {
            FrameLayout::SplitYuyvU16 {
                width,
                height,
                frame_height,
                thermal_half,
                endian,
            } => {
                let row_bytes = width as usize * 2;
                let start_row = match thermal_half {
                    ThermalHalf::Top => 0,
                    ThermalHalf::Bottom => height,
                };
                let start = start_row as usize * row_bytes;
                let len = height as usize * row_bytes;
                let needed = frame_height as usize * row_bytes;
                ensure!(
                    frame.len() >= needed,
                    "short UVC frame: {} bytes, need {} for {}x{}",
                    frame.len(),
                    needed,
                    width,
                    frame_height
                );
                let mut raw = frame[start..start + len].to_vec();
                if endian == Endian::Big {
                    for pair in raw.chunks_exact_mut(2) {
                        pair.swap(0, 1);
                    }
                }
                Ok(ExtractedFrame {
                    width,
                    height,
                    raw: Bytes::from(raw),
                })
            }
            FrameLayout::PackedYuyvU16 {
                width,
                thermal_rows,
                ..
            } => {
                let row_bytes = width as usize * 2;
                let needed = thermal_rows as usize * row_bytes;
                ensure!(
                    frame.len() >= needed,
                    "short packed UVC frame: {} bytes, need {}",
                    frame.len(),
                    needed
                );
                let mut raw = Vec::with_capacity(needed);
                for y in 0..thermal_rows as usize {
                    let row = y * row_bytes;
                    for x in (0..width as usize).step_by(2) {
                        let i = row + x * 2;
                        ensure!(i + 3 < frame.len(), "packed row overrun");
                        let y0 = frame[i] as u16;
                        let u0 = frame[i + 1] as u16;
                        let y1 = frame[i + 2] as u16;
                        let v0 = frame[i + 3] as u16;
                        raw.extend_from_slice(&((u0 << 8) | y0).to_le_bytes());
                        raw.extend_from_slice(&((v0 << 8) | y1).to_le_bytes());
                    }
                }
                Ok(ExtractedFrame {
                    width,
                    height: thermal_rows,
                    raw: Bytes::from(raw),
                })
            }
            FrameLayout::RawU16WithMetadata { width, height, .. } => {
                let len = width as usize * height as usize * 2;
                ensure!(
                    frame.len() >= len,
                    "short raw U16 frame: {} bytes, need {}",
                    frame.len(),
                    len
                );
                Ok(ExtractedFrame {
                    width,
                    height,
                    raw: Bytes::copy_from_slice(&frame[..len]),
                })
            }
            FrameLayout::UnknownRawU16 { width, height } => {
                if width == 0 || height == 0 {
                    bail!("unknown UVC layout has no negotiated size");
                }
                let len = width as usize * height as usize * 2;
                ensure!(
                    frame.len() >= len,
                    "short unknown UVC frame: {} bytes, need {}",
                    frame.len(),
                    len
                );
                Ok(ExtractedFrame {
                    width,
                    height,
                    raw: Bytes::copy_from_slice(&frame[..len]),
                })
            }
        }
    }
}
