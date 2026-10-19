//! End-to-end handshake test against a mock camera.
//!
//! The mock speaks the captured wire format: it answers probes with a session
//! flag, records every packet the driver sends, and emits one wrapper per
//! stream after START. This exercises the real driver code without touching a
//! live camera (a second session can disturb a running stream).

use std::sync::Arc;
use std::time::Duration;

use thermolink::camera::qianli::QianliDriver;
use thermolink::camera::{CameraDriver, CameraSession, FramePayload};
use thermolink::config::CameraConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::timeout;

const FLAG: [u8; 4] = [0xab, 0xcd, 0xef, 0x12];
const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;
const FRAME_SIZE: usize = 39448;
const PAYLOAD_OFFSET: usize = 88;

#[tokio::test]
async fn handshake_then_frames() {
    let control = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let media = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_port = control.local_addr().unwrap().port();
    let media_port = media.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

    // Control sockets: absorb registration/CTRL traffic.
    let seen_ctl = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = control.accept().await.unwrap();
            let seen = seen_ctl.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    seen.lock().await.push(buf[..n].to_vec());
                    let _ = s.write_all(&[0u8; 16]).await;
                }
            });
        }
    });

    let seen_media = seen.clone();
    tokio::spawn(async move {
        loop {
            let (s, _) = media.accept().await.unwrap();
            let seen = seen_media.clone();
            tokio::spawn(async move {
                handle_media(s, seen).await;
            });
        }
    });

    let cfg = CameraConfig {
        driver: "qianli".into(),
        host: "127.0.0.1".into(),
        control_port,
        media_port,
        ..Default::default()
    };
    let driver = QianliDriver::from_config(&cfg).unwrap();
    let mut session = timeout(Duration::from_secs(5), driver.open())
        .await
        .expect("handshake timed out")
        .expect("handshake failed");

    let (mut video, mut thermal) = (false, false);
    for _ in 0..8 {
        let frame = timeout(Duration::from_secs(3), session.next_frame())
            .await
            .expect("no frame")
            .expect("read failed");
        match frame.payload {
            FramePayload::Video(b) => {
                assert!(b.starts_with(&[0x00, 0x00, 0x00, 0x01, 0x67]));
                video = true;
            }
            FramePayload::Thermal(img) => {
                assert_eq!((img.width, img.height), (WIDTH, HEIGHT));
                assert_eq!(img.raw.len(), WIDTH as usize * HEIGHT as usize * 2);
                let stats = img.stats();
                // Payload was filled with 2000..=2099 raw, i.e. 20.00..20.99 C.
                assert!(stats.min_c >= 19.99 && stats.max_c <= 21.0, "{stats:?}");
                assert!(stats.mean_c > 20.0 && stats.mean_c < 21.0);
                thermal = true;
            }
        }
        if video && thermal {
            break;
        }
    }
    session.close().await;
    assert!(video, "no visible frame");
    assert!(thermal, "no thermal frame");

    // The driver must have echoed the mock's live flag in START (8-byte packet).
    let packets = seen.lock().await;
    let starts: Vec<_> = packets.iter().filter(|p| p.len() == 8).collect();
    assert_eq!(starts.len(), 2, "expected a START per stream");
    for p in starts {
        assert_eq!(&p[..4], &FLAG, "START did not carry the live session flag");
        assert_eq!(&p[4..8], &1u32.to_le_bytes());
    }
}

async fn handle_media(mut s: TcpStream, seen: Arc<Mutex<Vec<Vec<u8>>>>) {
    let mut buf = Vec::new();
    let mut value = 0u32;

    loop {
        let mut chunk = [0u8; 4096];
        let n = match s.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);

        loop {
            let Some(packet) = take_packet(&mut buf) else {
                break;
            };
            seen.lock().await.push(packet.clone());

            // Probe: 48 bytes, type 1 at 28..30 -> answer with the flag.
            if packet.len() == 48 && packet[28..30] == [1, 0] {
                let mut reply = vec![0u8; 56];
                reply[28] = 1;
                reply[48..52].copy_from_slice(&FLAG);
                s.write_all(&reply).await.unwrap();
            }
            // INIT: 48 bytes, type 2 at 28..30 -> remember the value.
            if packet.len() == 48 && packet[28..30] == [2, 0] {
                value = u32::from_le_bytes(packet[36..40].try_into().unwrap());
            }
            // START: 8 bytes -> the stream is live.
            if packet.len() == 8 {
                let frame = if value == 1 {
                    thermal_frame()
                } else {
                    vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e]
                };
                let _ = s.write_all(&frame).await;
            }
        }
    }
}

/// Split the next complete client packet off the buffer.
fn take_packet(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    if buf.len() < 4 {
        return None;
    }
    let magic = [0x78, 0xb6, 0x3a, 0x12];
    let len = if buf.starts_with(&magic) {
        48
    } else if buf.starts_with(b"888888") {
        40
    } else {
        8
    };
    if buf.len() < len {
        return None;
    }
    let packet = buf[..len].to_vec();
    buf.drain(..len);
    Some(packet)
}

fn thermal_frame() -> Vec<u8> {
    let mut f = vec![0u8; FRAME_SIZE];
    f[..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xb3]);
    f[48..51].copy_from_slice(b"RAW");
    for i in 0..(WIDTH * HEIGHT) as usize {
        let v = 2000u16 + (i % 100) as u16;
        f[PAYLOAD_OFFSET + i * 2..PAYLOAD_OFFSET + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    f
}
