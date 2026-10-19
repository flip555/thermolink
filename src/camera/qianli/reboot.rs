//! Restart the camera over its own Telnet service (root, blank password).
//!
//! This is how a stored network change takes effect: the camera keeps the old
//! address on the wire until it reboots.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

pub async fn reboot(host: Ipv4Addr) -> Result<Value> {
    let mut s = timeout(Duration::from_secs(5), TcpStream::connect((host, 23)))
        .await
        .context("telnet connect timed out")?
        .context("telnet connect failed")?;

    let greeting = read_for(&mut s, Duration::from_secs(4)).await;
    if !contains(&greeting, b"login:") {
        anyhow::bail!("no login prompt");
    }
    let opts = refuse_options(&greeting);
    if !opts.is_empty() {
        s.write_all(&opts).await?;
    }

    s.write_all(b"root\r\n").await?;
    let prompt = read_for(&mut s, Duration::from_secs(4)).await;
    if contains(&prompt, b"assword") {
        s.write_all(b"\r\n").await?;
        let _ = read_for(&mut s, Duration::from_secs(5)).await;
    }

    s.write_all(b"sync; reboot -f\n").await?;
    sleep(Duration::from_millis(500)).await;
    Ok(json!({ "ok": true, "host": host.to_string(), "method": "telnet" }))
}

async fn read_for(s: &mut TcpStream, window: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match timeout(left, s.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => out.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) => break,
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Answer Telnet IAC option negotiation so the shell accepts input.
fn refuse_options(greeting: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < greeting.len() {
        if greeting[i] == 0xFF {
            let cmd = greeting[i + 1];
            let opt = greeting[i + 2];
            match cmd {
                0xFD => out.extend_from_slice(&[0xFF, 0xFC, opt]),
                0xFB => out.extend_from_slice(&[0xFF, 0xFE, opt]),
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}
