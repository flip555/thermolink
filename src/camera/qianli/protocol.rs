//! Byte-exact Qianli / Sunshine SuperCam ("Neptune", `LAUNCHDIGITAL`) packets.
//!
//! Derived from the reverse-engineered Python client. All integers are
//! little-endian. Every builder returns the complete wire packet.

use std::net::Ipv4Addr;

pub const MAGIC: [u8; 4] = [0x78, 0xb6, 0x3a, 0x12];
pub const SERVICE: &[u8; 12] = b"video server";
pub const SERIAL: &[u8; 6] = b"888888";
pub const PAD: u8 = 0xcc;
pub const DEFAULT_SID: [u8; 4] = [0x21, 0xd3, 0x6c, 0x87];

/// Captured vendor write-mode probe: `0x6c` with the write flag set.
const WRITE_MODE_HEX: &str = "78b63a12766964656f20736572766572000000000000000000000000383838383838000000000000000000000000000038383838383800000000000000000000000000006c00010000000000000000007801000021d36c87";
/// Captured `6a` COMMIT packet. The vendor capture is 87 bytes (one short of
/// the usual 88); it is sent verbatim because that is what the camera acks.
const COMMIT_HEX: &str = "78b63a12766964656f20736572766572000000000000000000000000383838383838000000000000000000000000000038383838383800000000000000000000000000006a000000000000000000000000000021d36c87";

/// Enter the camera's settings write mode.
pub fn write_mode() -> Vec<u8> {
    decode_hex(WRITE_MODE_HEX)
}

/// Commit the preceding 376-byte settings block.
pub fn commit() -> Vec<u8> {
    decode_hex(COMMIT_HEX)
}

fn decode_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("static hex"))
        .collect()
}

/// 88-byte registration packet used on the control port.
///
/// `cmd` values seen live: `0x65`/`0x66` for client registration and `0x78`
/// immediately before a CTRL announce.
pub fn register(cmd: u64, flags: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(88);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(SERVICE);
    p.extend_from_slice(&[0u8; 12]);
    p.extend_from_slice(SERIAL);
    p.extend_from_slice(&[0u8; 14]);
    p.extend_from_slice(SERIAL);
    p.extend_from_slice(&[0u8; 14]);
    p.extend_from_slice(&cmd.to_le_bytes());
    p.extend_from_slice(&flags.to_le_bytes());
    p.extend_from_slice(&DEFAULT_SID);
    debug_assert_eq!(p.len(), 88);
    p
}

/// 48-byte media probe. The camera answers with a 56-byte reply whose bytes
/// 28..30 are `01 00` and whose bytes 48..52 carry the live session flag.
pub fn probe(ip: Ipv4Addr, port: u16, value: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(48);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(SERVICE);
    p.push(0);
    p.extend_from_slice(&[PAD; 11]);
    p.extend_from_slice(&1u16.to_le_bytes());
    p.extend_from_slice(&ip.octets());
    p.extend_from_slice(&port.to_le_bytes());
    p.extend_from_slice(&value.to_le_bytes());
    p.extend_from_slice(&0x28u32.to_le_bytes());
    p.extend_from_slice(&DEFAULT_SID);
    debug_assert_eq!(p.len(), 48);
    p
}

/// 40-byte serial handshake, sent on each probe socket right after the probe.
pub fn serial_packet() -> Vec<u8> {
    let mut unit = Vec::with_capacity(20);
    unit.extend_from_slice(SERIAL);
    unit.push(0);
    unit.extend_from_slice(&[PAD; 13]);
    let mut p = unit.clone();
    p.extend_from_slice(&unit);
    debug_assert_eq!(p.len(), 40);
    p
}

/// 48-byte INIT: binds a *video* socket to a channel and a value (0/1).
pub fn init(value: u32, channel: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(48);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(&[PAD; 24]);
    p.extend_from_slice(&2u16.to_le_bytes());
    p.extend_from_slice(&[PAD; 6]);
    p.extend_from_slice(&value.to_le_bytes());
    p.extend_from_slice(&channel.to_le_bytes());
    p.extend_from_slice(&DEFAULT_SID);
    debug_assert_eq!(p.len(), 48);
    p
}

/// 48-byte post-registration handshake on a probe socket (type `0x14`).
pub fn type14(value: u32) -> Vec<u8> {
    media_op(0x14, value)
}

/// 48-byte periodic keepalive on a media/probe socket (type `0x05`).
pub fn keepalive(value: u32) -> Vec<u8> {
    media_op(0x05, value)
}

fn media_op(op: u32, value: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(48);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(&[PAD; 24]);
    p.extend_from_slice(&op.to_le_bytes());
    p.extend_from_slice(&[PAD; 4]);
    p.extend_from_slice(&value.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p.extend_from_slice(&DEFAULT_SID);
    debug_assert_eq!(p.len(), 48);
    p
}

/// 8-byte START. The first four bytes are the LIVE session flag from the probe
/// reply — never a constant. A stale flag makes the camera accept everything
/// and then close the media sockets without sending a single frame.
pub fn start(flag: [u8; 4], value: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&flag);
    p.extend_from_slice(&value.to_le_bytes());
    p
}

/// 16-byte CTRL: announces a stream flag for a value on a media port.
/// `value` is 2 for the visible stream and 4 for the secondary/thermal one.
pub fn ctrl(value: u32, flag: [u8; 4], port: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(16);
    p.extend_from_slice(&3u32.to_le_bytes());
    p.extend_from_slice(&value.to_le_bytes());
    p.extend_from_slice(&flag);
    p.extend_from_slice(&(port as u32).to_le_bytes());
    debug_assert_eq!(p.len(), 16);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_lengths() {
        assert_eq!(register(0x65, 0).len(), 88);
        assert_eq!(probe(Ipv4Addr::new(192, 168, 1, 64), 3000, 1).len(), 48);
        assert_eq!(serial_packet().len(), 40);
        assert_eq!(init(1, 8).len(), 48);
        assert_eq!(type14(1).len(), 48);
        assert_eq!(keepalive(0).len(), 48);
        assert_eq!(start([0x33, 0, 0, 0], 1).len(), 8);
        assert_eq!(ctrl(4, [0x35, 0, 0, 0], 3000).len(), 16);
    }

    #[test]
    fn probe_matches_captured_bytes() {
        // Captured packet for the thermal value, port 3000.
        let expected = hex(
            "78b63a12766964656f2073657276657200cccccccccccccccccccccc0100c0a80140b80b010000002800000021d36c87",
        );
        assert_eq!(probe(Ipv4Addr::new(192, 168, 1, 64), 3000, 1), expected);
    }

    #[test]
    fn probe_visible_matches_capture() {
        let expected = hex(
            "78b63a12766964656f2073657276657200cccccccccccccccccccccc0100c0a80140b80b000000002800000021d36c87",
        );
        assert_eq!(probe(Ipv4Addr::new(192, 168, 1, 64), 3000, 0), expected);
    }

    #[test]
    fn init_matches_capture() {
        let expected = hex(
            "78b63a12cccccccccccccccccccccccccccccccccccccccccccccccc0200cccccccccccc010000000800000021d36c87",
        );
        assert_eq!(init(1, 8), expected);
    }

    #[test]
    fn ctrl_matches_capture() {
        let expected = hex("030000000400000035000000b80b0000");
        assert_eq!(ctrl(4, [0x35, 0, 0, 0], 3000), expected);
    }

    #[test]
    fn write_packets_match_capture() {
        let mode = write_mode();
        assert_eq!(mode.len(), 88);
        assert_eq!(&mode[68..72], &[0x6c, 0x00, 0x01, 0x00]);
        assert_eq!(mode, hex(WRITE_MODE_HEX));

        let commit = commit();
        assert_eq!(commit.len(), 87);
        assert_eq!(&commit[68..70], &[0x6a, 0x00]);
        assert_eq!(commit, hex(COMMIT_HEX));
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
