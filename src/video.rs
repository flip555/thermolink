//! Visible-stream transcode: H.264 from the camera -> MJPEG for the browser.
//!
//! One ffmpeg process serves every viewer. It is fed from the frame hub and its
//! JPEG frames are published on a watch channel, so a slow client can never
//! back-pressure the camera.

use std::process::Stdio;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;

use crate::camera::FramePayload;
use crate::stream::FrameHub;

#[derive(Clone)]
pub struct VideoTranscoder {
    latest: watch::Receiver<Option<Bytes>>,
}

impl VideoTranscoder {
    pub fn latest(&self) -> Option<Bytes> {
        self.latest.borrow().clone()
    }
}

/// Start the transcoder. Returns `None` when ffmpeg is unavailable; the rest of
/// the app keeps working, only the visible view is disabled.
pub fn spawn(hub: FrameHub, width: u32, fps: u32) -> Option<VideoTranscoder> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "+genpts+igndts",
            "-err_detect",
            "ignore_err",
            "-analyzeduration",
            "500000",
            "-probesize",
            "200000",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-vf",
            &format!("scale={width}:-2"),
            "-r",
            &fps.to_string(),
            "-q:v",
            "6",
            "-f",
            "mjpeg",
            "-flush_packets",
            "1",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take();
    let (tx, rx) = watch::channel(None);

    // ffmpeg errors are worth seeing; the process otherwise fails silently.
    if let Some(mut err) = stderr.take() {
        tokio::spawn(async move {
            let mut line = String::new();
            let mut byte = [0u8; 1];
            loop {
                match err.read(&mut byte).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if byte[0] == b'\n' {
                            if !line.trim().is_empty() {
                                tracing::debug!(ffmpeg = %line.trim(), "transcoder");
                            }
                            line.clear();
                        } else if line.len() < 512 {
                            line.push(byte[0] as char);
                        }
                    }
                }
            }
        });
    }

    // Feed H.264 chunks. Re-sync on the next SPS is ffmpeg's job.
    tokio::spawn(async move {
        let mut frames = hub.subscribe();
        loop {
            match frames.recv().await {
                Ok(frame) => {
                    if let FramePayload::Video(bytes) = &frame.payload {
                        if stdin.write_all(bytes).await.is_err() {
                            break;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    // Split the MJPEG stream into complete JPEGs.
    tokio::spawn(async move {
        // Dropping `Child` with kill_on_drop(true) would kill ffmpeg, so the
        // handle lives here for as long as the reader does.
        let _child = child;
        let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&chunk[..n]);

            loop {
                let Some(start) = find(&buf, &[0xff, 0xd8]) else {
                    if buf.len() > 1 {
                        let keep = buf.len() - 1;
                        buf.drain(..keep);
                    }
                    break;
                };
                if start > 0 {
                    buf.drain(..start);
                }
                let Some(end) = find(&buf, &[0xff, 0xd9]) else {
                    break;
                };
                let jpeg = Bytes::copy_from_slice(&buf[..end + 2]);
                buf.drain(..end + 2);
                let _ = tx.send(Some(jpeg));
            }

            if buf.len() > 8 * 1024 * 1024 {
                buf.clear();
            }
        }
    });

    Some(VideoTranscoder { latest: rx })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
