//! Visible-stream transcode: H.264 from the camera -> MJPEG for the browser.
//!
//! One supervised ffmpeg process serves every viewer. It is fed from the frame
//! hub and its JPEG frames are published on a watch channel, so a slow client
//! can never back-pressure the camera. If ffmpeg exits, the supervisor restarts
//! it with backoff and the health endpoint reports the truth.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;

use crate::camera::FramePayload;
use crate::stream::FrameHub;

#[derive(Clone)]
pub struct VideoTranscoder {
    latest: watch::Receiver<Option<Bytes>>,
    alive: Arc<AtomicBool>,
    restarts: Arc<AtomicUsize>,
    error: Arc<Mutex<Option<String>>>,
}

impl VideoTranscoder {
    pub fn latest(&self) -> Option<Bytes> {
        self.latest.borrow().clone()
    }

    /// True once the current ffmpeg instance has produced a JPEG.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn restarts(&self) -> usize {
        self.restarts.load(Ordering::Relaxed)
    }

    pub fn last_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
}

/// Start the supervised transcoder.
pub fn spawn(hub: FrameHub, width: u32, fps: u32) -> Option<VideoTranscoder> {
    let (tx, rx) = watch::channel(None);
    let alive = Arc::new(AtomicBool::new(false));
    let restarts = Arc::new(AtomicUsize::new(0));
    let error = Arc::new(Mutex::new(None));
    let state = VideoTranscoder {
        latest: rx,
        alive: alive.clone(),
        restarts: restarts.clone(),
        error: error.clone(),
    };
    tokio::spawn(supervise(hub, width, fps, tx, alive, restarts, error));
    Some(state)
}

async fn supervise(
    hub: FrameHub,
    width: u32,
    fps: u32,
    tx: watch::Sender<Option<Bytes>>,
    alive: Arc<AtomicBool>,
    restarts: Arc<AtomicUsize>,
    error: Arc<Mutex<Option<String>>>,
) {
    let mut backoff = 1u64;
    loop {
        match run_once(&hub, width, fps, &tx, &alive, &error).await {
            Ok(()) => set_error(&error, "ffmpeg exited"),
            Err(e) => set_error(&error, &e),
        }
        alive.store(false, Ordering::Relaxed);
        restarts.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            restarts = restarts.load(Ordering::Relaxed),
            "visible transcoder stopped; restarting"
        );
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(10);
    }
}

async fn run_once(
    hub: &FrameHub,
    width: u32,
    fps: u32,
    tx: &watch::Sender<Option<Bytes>>,
    alive: &Arc<AtomicBool>,
    error: &Arc<Mutex<Option<String>>>,
) -> Result<(), String> {
    let ffmpeg =
        std::env::var_os("THERMOLINK_FFMPEG").unwrap_or_else(|| std::ffi::OsString::from("ffmpeg"));
    let mut command = Command::new(&ffmpeg);
    #[cfg(windows)]
    {
        // A GUI process spawning ffmpeg would otherwise leave a blank console
        // window titled "ffmpeg" on Windows.
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
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
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("ffmpeg has no stdin")?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg has no stdout")?;

    if let Some(mut err) = child.stderr.take() {
        let error = error.clone();
        tokio::spawn(async move {
            let mut line = String::new();
            let mut byte = [0u8; 1];
            loop {
                match err.read(&mut byte).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if byte[0] == b'\n' {
                            let text = line.trim().to_string();
                            if !text.is_empty() {
                                tracing::debug!(ffmpeg = %text, "transcoder");
                                set_error(&error, &text);
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

    // Feed H.264 chunks. When ffmpeg dies, stdin closes and this task exits.
    {
        let mut frames = hub.subscribe();
        tokio::spawn(async move {
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
    }

    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut first = true;
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
            if first {
                first = false;
                alive.store(true, Ordering::Relaxed);
                set_error(error, "");
                tracing::info!("visible transcoder producing frames");
            }
            let _ = tx.send(Some(jpeg));
        }

        if buf.len() > 8 * 1024 * 1024 {
            buf.clear();
        }
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(())
}

fn set_error(slot: &Arc<Mutex<Option<String>>>, text: &str) {
    if let Ok(mut e) = slot.lock() {
        *e = if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
