//! Thermal wrapper constants and frame extraction for the Qianli family.
//!
//! The stream is block-aligned but captures do not necessarily start on a block
//! boundary, so frames are located by magic + the `RAW` marker at offset 48 —
//! never by assuming the first byte is a frame start.

use bytes::{Buf, Bytes, BytesMut};

pub const WIDTH: u32 = 160;
pub const HEIGHT: u32 = 120;
pub const FRAME_SIZE: usize = 39448;
pub const PAYLOAD_OFFSET: usize = 88;
pub const PAYLOAD_BYTES: usize = WIDTH as usize * HEIGHT as usize * 2; // 38400
pub const MAGIC: [u8; 4] = [0x00, 0x00, 0x01, 0xb3];
/// "RAW" sits at magic + 48; "H264" marks the visible wrapper.
pub const KIND_OFFSET: usize = 48;
pub const RAW_MARK: &[u8; 3] = b"RAW";

/// One extracted radiometric frame.
#[derive(Debug, Clone)]
pub struct RawThermalFrame {
    pub seq: u64,
    /// `PAYLOAD_BYTES` little-endian `u16` samples.
    pub raw: Bytes,
}

/// Reassembles fixed-size thermal wrappers from an arbitrary byte stream.
pub struct ThermalAssembler {
    buf: BytesMut,
    seq: u64,
}

impl Default for ThermalAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl ThermalAssembler {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(FRAME_SIZE * 2),
            seq: 0,
        }
    }

    /// Feed bytes read from the media socket; returns every complete frame.
    pub fn push(&mut self, data: &[u8]) -> Vec<RawThermalFrame> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();

        loop {
            let Some(i) = find_magic(&self.buf) else {
                // Keep a tail shorter than the magic so a split signature still
                // matches next time.
                if self.buf.len() > MAGIC.len() {
                    let keep = MAGIC.len() - 1;
                    let drop = self.buf.len() - keep;
                    self.buf.advance(drop);
                }
                break;
            };
            if i > 0 {
                self.buf.advance(i);
            }
            if self.buf.len() < KIND_OFFSET + RAW_MARK.len() {
                break;
            }
            if &self.buf[KIND_OFFSET..KIND_OFFSET + RAW_MARK.len()] != RAW_MARK {
                // False positive (e.g. the visible H.264 wrapper): skip the
                // candidate and rescan from the next byte.
                self.buf.advance(4);
                continue;
            }
            if self.buf.len() < FRAME_SIZE {
                break;
            }
            let raw =
                Bytes::copy_from_slice(&self.buf[PAYLOAD_OFFSET..PAYLOAD_OFFSET + PAYLOAD_BYTES]);
            self.seq += 1;
            out.push(RawThermalFrame { seq: self.seq, raw });
            self.buf.advance(FRAME_SIZE);
        }

        // Absolute guard: never let a misaligned stream grow without bound.
        const MAX_BUFFER: usize = FRAME_SIZE * 4;
        if self.buf.len() > MAX_BUFFER {
            let drop = self.buf.len() - MAX_BUFFER;
            self.buf.advance(drop);
        }
        out
    }

    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

fn find_magic(buf: &[u8]) -> Option<usize> {
    buf.windows(MAGIC.len()).position(|w| w == MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; FRAME_SIZE];
        f[..4].copy_from_slice(&MAGIC);
        f[KIND_OFFSET..KIND_OFFSET + 3].copy_from_slice(b"RAW");
        f[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload.len()].copy_from_slice(payload);
        f
    }

    #[test]
    fn ignores_preamble_and_emits_one_frame() {
        let mut a = ThermalAssembler::new();
        let payload: Vec<u8> = (0..PAYLOAD_BYTES).map(|i| (i % 251) as u8).collect();
        let mut stream = vec![0x55u8; 35352];
        stream.extend_from_slice(&frame_with(&payload));

        let frames = a.push(&stream);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 1);
        assert_eq!(&frames[0].raw[..], &payload[..]);
        assert_eq!(a.buffered(), 0);
    }

    #[test]
    fn handles_split_reads() {
        let payload = vec![7u8; PAYLOAD_BYTES];
        let f = frame_with(&payload);
        let mut a = ThermalAssembler::new();
        assert!(a.push(&f[..100]).is_empty());
        assert!(a.push(&f[100..20_000]).is_empty());
        let out = a.push(&f[20_000..]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw.len(), PAYLOAD_BYTES);
    }

    #[test]
    fn skips_non_raw_wrapper() {
        let mut other = frame_with(&vec![1u8; PAYLOAD_BYTES]);
        other[KIND_OFFSET..KIND_OFFSET + 4].copy_from_slice(b"H264");
        let mut a = ThermalAssembler::new();
        let frames = a.push(&other);
        assert!(frames.is_empty());
    }
}
