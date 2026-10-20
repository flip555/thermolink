//! Linux UVC transport via ffmpeg's `v4l2` input.
//!
//! ffmpeg is already a runtime dependency for the visible-stream transcode.
//! Using its V4L2 demuxer avoids a libclang/bindgen build dependency and keeps
//! the transport boundary replaceable by a direct-ioctl backend later.
//!
//! This backend is intentionally small: it enumerates sysfs video nodes,
//! negotiates a YUYV frame size, and pipes complete raw frames. It does not
//! expose vendor controls.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

use super::{UvcStream, UvcTransport};
use crate::camera::uvc_radiometric::types::{StreamConfig, StreamInfo, UvcDevice};

pub struct LinuxV4l2Transport;

#[async_trait]
impl UvcTransport for LinuxV4l2Transport {
    async fn enumerate(&self) -> Result<Vec<UvcDevice>> {
        tokio::task::spawn_blocking(enumerate_blocking).await?
    }

    async fn open(&self, device: &UvcDevice, config: StreamConfig) -> Result<Box<dyn UvcStream>> {
        let path = device.path.clone().context("UVC device has no /dev path")?;
        let (width, height) = match (config.width, config.height) {
            (Some(w), Some(h)) => (w, h),
            _ => probe_default(&path).await?,
        };
        if let Some(value) = config.raw_mode {
            set_zoom_control(&path, value)?;
        }
        let fps = config.fps.unwrap_or(25);
        let frame_bytes = (width as usize) * (height as usize) * 2;
        if frame_bytes == 0 {
            bail!("zero-sized UVC frame");
        }

        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "v4l2",
                "-input_format",
                "yuyv422",
                "-video_size",
                &format!("{width}x{height}"),
                "-framerate",
                &fps.to_string(),
                "-i",
                &path,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuyv422",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn ffmpeg; is it installed and is the device readable?")?;

        let stdout = child.stdout.take().context("ffmpeg stdout")?;
        let mut stderr = child.stderr.take().context("ffmpeg stderr")?;
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            if !text.trim().is_empty() {
                tracing::debug!(ffmpeg = %text.trim(), "UVC ffmpeg stderr");
            }
        });

        let (tx, rx) = mpsc::channel(4);
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = vec![0u8; frame_bytes];
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    result = reader.read_exact(&mut buf) => {
                        match result {
                            Ok(_) => {
                                if tx.send(Bytes::copy_from_slice(&buf)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "UVC ffmpeg stream ended");
                                break;
                            }
                        }
                    }
                }
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        });

        Ok(Box::new(FfmpegStream {
            info: StreamInfo {
                width,
                height,
                fourcc: *b"YUYV",
                fps: Some(fps),
            },
            rx,
            stop: stop_tx,
            handle: Some(handle),
        }))
    }
}

/// HT301/T2S enter raw 16-bit mode through the UVC zoom control. This is the
/// same vendor-specific trick used by `ht301_hacklib` (`CAP_PROP_ZOOM=0x8004`).
fn set_zoom_control(path: &str, value: u32) -> Result<()> {
    // V4L2_CID_ZOOM_ABSOLUTE and VIDIOC_S_CTRL for 64-bit Linux.
    const V4L2_CID_ZOOM_ABSOLUTE: u32 = 0x0098_0917;
    const VIDIOC_S_CTRL: libc::c_ulong = 0xC008_561C;

    #[repr(C)]
    struct V4l2Control {
        id: u32,
        value: i32,
    }

    let c_path = std::ffi::CString::new(path).context("V4L2 control path")?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        bail!(
            "open {path} for V4L2 control: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut control = V4l2Control {
        id: V4L2_CID_ZOOM_ABSOLUTE,
        value: value as i32,
    };
    let rc = unsafe { libc::ioctl(fd, VIDIOC_S_CTRL, &mut control) };
    let err = std::io::Error::last_os_error();
    unsafe {
        libc::close(fd);
    }
    if rc < 0 {
        bail!("set V4L2 zoom control on {path}: {err}");
    }
    Ok(())
}

struct FfmpegStream {
    info: StreamInfo,
    rx: mpsc::Receiver<Bytes>,
    stop: watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

#[async_trait]
impl UvcStream for FfmpegStream {
    fn info(&self) -> StreamInfo {
        self.info
    }

    async fn next_frame(&mut self) -> Result<Bytes> {
        match tokio::time::timeout(Duration::from_secs(10), self.rx.recv()).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => bail!("UVC stream ended"),
            Err(_) => bail!("no UVC frame for 10s"),
        }
    }

    async fn close(&mut self) {
        let _ = self.stop.send(true);
        if let Some(handle) = self.handle.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
    }
}

fn enumerate_blocking() -> Result<Vec<UvcDevice>> {
    let dir = match std::fs::read_dir("/sys/class/video4linux") {
        Ok(dir) => dir,
        // A host without V4L2 is not an error; detection will simply report
        // that no UVC devices are present.
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("video") {
            continue;
        }
        let Some((vid, pid, product, serial)) = usb_ids(&entry.path()) else {
            continue;
        };
        out.push(UvcDevice {
            vid,
            pid,
            product,
            serial,
            path: Some(format!("/dev/{name}")),
            bus: None,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn usb_ids(dev: &Path) -> Option<(u16, u16, Option<String>, Option<String>)> {
    let mut cur = std::fs::canonicalize(dev.join("device")).ok()?;
    for _ in 0..8 {
        let vid = std::fs::read_to_string(cur.join("idVendor")).ok();
        let pid = std::fs::read_to_string(cur.join("idProduct")).ok();
        if let (Some(v), Some(p)) = (vid, pid) {
            let vid = u16::from_str_radix(v.trim(), 16).ok()?;
            let pid = u16::from_str_radix(p.trim(), 16).ok()?;
            let product = std::fs::read_to_string(cur.join("product"))
                .ok()
                .map(|s| s.trim().to_string());
            let serial = std::fs::read_to_string(cur.join("serial"))
                .ok()
                .map(|s| s.trim().to_string());
            return Some((vid, pid, product, serial));
        }
        cur = cur.parent()?.to_path_buf();
    }
    None
}

async fn probe_default(path: &str) -> Result<(u32, u32)> {
    // Ordered from the known M1 layout outward. This is a best-effort
    // fallback, not a claim that every size is supported.
    for (width, height) in [
        (256u32, 384u32),
        (256, 392),
        (384, 292),
        (256, 196),
        (640, 516),
        (256, 192),
    ] {
        if probe_size(path, width, height).await.is_ok() {
            return Ok((width, height));
        }
    }
    bail!("could not negotiate a known UVC frame size on {path}")
}

async fn probe_size(path: &str, width: u32, height: u32) -> Result<()> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "v4l2",
            "-input_format",
            "yuyv422",
            "-video_size",
            &format!("{width}x{height}"),
            "-framerate",
            "25",
            "-i",
            path,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuyv422",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn ffmpeg probe")?;
    let mut stdout = child.stdout.take().context("ffmpeg probe stdout")?;
    let mut buf = vec![0u8; width as usize * height as usize * 2];
    let result = tokio::time::timeout(Duration::from_secs(3), stdout.read_exact(&mut buf)).await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    match result {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => bail!("ffmpeg probe timed out"),
    }
}
