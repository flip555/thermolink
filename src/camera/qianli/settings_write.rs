//! Rewrite the camera's 376-byte network block.
//!
//! The vendor sequence, confirmed against the captured client, is:
//!
//! ```text
//! probe (read settings) -> write-mode probe -> block -> 6a COMMIT
//! ```
//!
//! The camera stores the new values and reports them immediately, but a
//! network change only reaches the wire after a reboot.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::sleep;

use super::{connect, protocol as p, settings};

const STEP_GAP: Duration = Duration::from_millis(450);
const READ_BUDGET: Duration = Duration::from_secs(3);

pub async fn write(host: Ipv4Addr, port: u16, changes: &Map<String, Value>) -> Result<Value> {
    let mut steps: Vec<Value> = Vec::new();
    let mut s = connect(host, port)
        .await
        .context("opening the camera control port")?;

    let before = read_block(&mut s, READ_BUDGET)
        .await
        .context("reading the current settings")?;
    let (_, current) = settings::find_block(&before).context("no settings block in the reply")?;
    let previous = settings::parse(&before)
        .and_then(|v| v.get("ip").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let wanted = changes
        .get("ip")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| previous.clone());
    let block = settings::set_fields(current, changes).map_err(anyhow::Error::msg)?;
    steps.push(json!({ "step": "read", "reply_bytes": before.len(), "ip": previous }));

    s.write_all(&p::write_mode())
        .await
        .context("write-mode probe")?;
    sleep(STEP_GAP).await;
    drain(&mut s, Duration::from_millis(250)).await;
    steps.push(json!({ "step": "write-mode" }));

    s.write_all(&block).await.context("settings block")?;
    sleep(STEP_GAP).await;
    drain(&mut s, Duration::from_millis(250)).await;
    steps.push(json!({ "step": "settings", "bytes": block.len() }));

    s.write_all(&p::commit()).await.context("commit packet")?;
    let ack = read_until(&mut s, Duration::from_secs(2), |buf| {
        buf.len() >= 70 && buf[68..70] == [0x6a, 0x00]
    })
    .await;
    let acknowledged = ack.len() >= 70 && ack[68..70] == [0x6a, 0x00];
    steps.push(json!({
        "step": "commit",
        "reply_bytes": ack.len(),
        "acknowledged": acknowledged,
    }));

    // Let the camera settle before the socket disappears.
    sleep(Duration::from_millis(1500)).await;
    let _ = s.shutdown().await;
    drop(s);

    // Read back from a fresh connection: stored values are visible before the
    // reboot that puts them on the wire.
    let mut applied = false;
    let mut now = Value::Null;
    for _ in 0..3 {
        sleep(Duration::from_millis(1500)).await;
        match probe_ip(host, port).await {
            Ok((ip, parsed)) => {
                now = Value::String(ip.clone());
                if ip == wanted {
                    applied = true;
                    if let Some(obj) = parsed.as_object() {
                        now = json!({
                            "ip": obj.get("ip").cloned().unwrap_or(Value::Null),
                            "mask": obj.get("mask").cloned().unwrap_or(Value::Null),
                            "gw": obj.get("gw").cloned().unwrap_or(Value::Null),
                            "dns": obj.get("dns").cloned().unwrap_or(Value::Null),
                        });
                    }
                    break;
                }
                steps.push(json!({ "verify": format!("still {ip}") }));
                break;
            }
            Err(e) => steps.push(json!({ "verify_error": e.to_string() })),
        }
    }

    Ok(json!({
        "ok": applied,
        "applied": applied,
        "requested": wanted,
        "previous": previous,
        "now": now,
        "attempts": steps,
        "note": "a network change makes the camera reboot",
    }))
}

async fn probe_ip(host: Ipv4Addr, port: u16) -> Result<(String, Value)> {
    let mut s = connect(host, port).await?;
    let blob = read_block(&mut s, READ_BUDGET).await?;
    let parsed = settings::parse(&blob).context("unparseable settings reply")?;
    let ip = parsed
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((ip, parsed))
}

async fn read_block(s: &mut TcpStream, budget: Duration) -> Result<Vec<u8>> {
    s.write_all(&p::register(0x6c, 0))
        .await
        .context("settings probe")?;
    let blob = read_until(s, budget, |buf| settings::find_block(buf).is_some()).await;
    anyhow::ensure!(!blob.is_empty(), "no reply from the camera");
    Ok(blob)
}

/// Read until `done` accepts the accumulated bytes or `budget` elapses.
async fn read_until<F>(s: &mut TcpStream, budget: Duration, done: F) -> Vec<u8>
where
    F: Fn(&[u8]) -> bool,
{
    use tokio::io::AsyncReadExt;
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + budget;
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, s.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => {
                out.extend_from_slice(&chunk[..n]);
                if done(&out) {
                    break;
                }
                if out.len() > 8192 {
                    let keep = out.split_off(out.len() - 1024);
                    out = keep;
                }
            }
            Ok(Err(_)) => break,
        }
    }
    out
}

async fn drain(s: &mut TcpStream, budget: Duration) {
    let _ = read_until(s, budget, |_| false).await;
}
