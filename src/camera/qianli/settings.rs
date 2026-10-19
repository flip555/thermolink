//! Read the camera's own 376-byte configuration block.
//!
//! A control probe (`0x6c`) returns an 88-byte echo followed by the block.
//! Writing is a separate ritual and is not implemented here.

use serde_json::{json, Value};

const BLOCK_LEN: usize = 376;
const ECHO_LEN: usize = 88;

const FIELDS: &[(&str, usize, usize)] = &[
    ("name", 0, 24),
    ("ip", 24, 16),
    ("mask", 40, 16),
    ("gw", 56, 16),
    ("dns", 72, 16),
    ("misc", 88, 16),
    ("firmware", 176, 48),
    ("date", 276, 48),
];

pub fn parse(blob: &[u8]) -> Option<Value> {
    let (offset, block) = find_block(blob)?;
    let mut out = serde_json::Map::new();
    for &(name, off, size) in FIELDS {
        out.insert(
            name.to_string(),
            Value::String(text(&block[off..off + size])),
        );
    }
    out.insert("reply_offset".into(), json!(offset));
    out.insert("reply_bytes".into(), json!(blob.len()));
    Some(Value::Object(out))
}

fn find_block(blob: &[u8]) -> Option<(usize, &[u8])> {
    // The documented layout first: an 88-byte echo, or a bare block.
    for offset in [ECHO_LEN, 0] {
        if let Some(block) = block_at(blob, offset) {
            return Some((offset, block));
        }
    }
    // A reused control socket can carry unrelated bytes before the reply.
    // Resynchronise on the block's own shape rather than giving up.
    if blob.len() >= BLOCK_LEN {
        for offset in 0..=blob.len() - BLOCK_LEN {
            if let Some(block) = block_at(blob, offset) {
                return Some((offset, block));
            }
        }
    }
    None
}

fn block_at(blob: &[u8], offset: usize) -> Option<&[u8]> {
    if blob.len() < offset + BLOCK_LEN {
        return None;
    }
    let name = &blob[offset..offset + 24];
    let ip = &blob[offset + 24..offset + 40];
    if dotted_ip(ip) && name.iter().any(|b| b.is_ascii_graphic()) {
        Some(&blob[offset..offset + BLOCK_LEN])
    } else {
        None
    }
}

fn dotted_ip(bytes: &[u8]) -> bool {
    let text = bytes.split(|b| *b == 0).next().unwrap_or_default();
    let text = match std::str::from_utf8(text) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|v| v <= 255))
}

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_block_after_echo() {
        let mut blob = vec![0u8; ECHO_LEN + BLOCK_LEN];
        blob[ECHO_LEN..ECHO_LEN + 9].copy_from_slice(b"SuperCam\0");
        blob[ECHO_LEN + 24..ECHO_LEN + 36].copy_from_slice(b"192.168.1.64");
        blob[ECHO_LEN + 40..ECHO_LEN + 52].copy_from_slice(b"255.255.255\0");
        let v = parse(&blob).expect("block");
        assert_eq!(v["name"], "SuperCam");
        assert_eq!(v["ip"], "192.168.1.64");
        assert_eq!(v["mask"], "255.255.255");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0u8; 600]).is_none());
    }
}
