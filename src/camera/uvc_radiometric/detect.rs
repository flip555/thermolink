//! Profile detection: explicit override, then VID/PID + product string, then
//! a generic best-effort fallback. Detection never fails because a device is
//! unknown; it reports why it could not identify it.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::camera::Confidence;
use crate::config::CameraConfig;

use super::fingerprint::FINGERPRINTS;
use super::profiles::{profile_by_id, UVC_GENERIC};
use super::transport::UvcTransport;
use super::types::{RadiometricProfile, UvcDevice};

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub profile_id: &'static str,
    pub display_name: &'static str,
    pub vendor: &'static str,
    pub model: &'static str,
    pub device: UvcDevice,
    pub score: u32,
    pub reasons: Vec<String>,
    pub tested: bool,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct Detection {
    pub candidates: Vec<Candidate>,
    pub selected: Option<Candidate>,
    pub fallback: bool,
    pub ambiguous: bool,
    pub notes: Vec<String>,
}

fn candidate(
    profile: &'static RadiometricProfile,
    device: &UvcDevice,
    score: u32,
    reasons: Vec<String>,
) -> Candidate {
    Candidate {
        profile_id: profile.info.id,
        display_name: profile.info.display_name,
        vendor: profile.info.vendor,
        model: profile.info.model,
        device: device.clone(),
        score,
        reasons,
        tested: profile.info.tested,
        confidence: profile.info.confidence,
    }
}

/// Detect a profile for the attached devices. Returns candidates even when
/// the result is ambiguous or falls back to `uvc_generic`.
pub async fn detect(cfg: &CameraConfig, transport: &dyn UvcTransport) -> Detection {
    let mut notes = Vec::new();
    let devices = match transport.enumerate().await {
        Ok(d) => d,
        Err(e) => {
            return Detection {
                candidates: Vec::new(),
                selected: None,
                fallback: true,
                ambiguous: false,
                notes: vec![format!("USB enumeration failed: {e}")],
            };
        }
    };
    if devices.is_empty() {
        return Detection {
            candidates: Vec::new(),
            selected: None,
            fallback: true,
            ambiguous: false,
            notes: vec!["no UVC devices found".into()],
        };
    }

    let explicit = cfg
        .profile
        .as_deref()
        .filter(|p| *p != "auto" && *p != "uvc_generic");

    // Optional device filter from camera.json.
    let filtered: Vec<&UvcDevice> = if cfg.device.path.is_some() || cfg.device.vid.is_some() {
        devices
            .iter()
            .filter(|d| {
                cfg.device
                    .path
                    .as_deref()
                    .is_none_or(|p| d.path.as_deref() == Some(p))
                    && cfg.device.vid.is_none_or(|v| d.vid == v)
                    && cfg.device.pid.is_none_or(|p| d.pid == p)
            })
            .collect()
    } else {
        devices.iter().collect()
    };
    if filtered.is_empty() {
        notes.push("no device matched the configured USB selector".into());
    }

    let mut map: BTreeMap<(&'static str, String), Candidate> = BTreeMap::new();
    for device in &filtered {
        let product = device.product.as_deref().unwrap_or("").to_ascii_lowercase();
        for fp in FINGERPRINTS {
            if fp.vid != device.vid || fp.pid != device.pid {
                continue;
            }
            if let Some(want) = fp.product_substring {
                if !product.contains(&want.to_ascii_lowercase()) {
                    continue;
                }
            }
            if let Some(want) = explicit {
                if fp.profile_id != want {
                    continue;
                }
            }
            let Some(profile) = profile_by_id(fp.profile_id) else {
                continue;
            };
            let mut reasons = vec![format!("USB {}", device.pretty_id())];
            if fp.product_substring.is_some() {
                reasons.push("product string match".into());
            }
            let score = if fp.product_substring.is_some() {
                120
            } else {
                100
            };
            let key = (fp.profile_id, device.path.clone().unwrap_or_default());
            let next = candidate(profile, device, score, reasons);
            map.entry(key)
                .and_modify(|current| {
                    if next.score > current.score {
                        *current = next.clone();
                    }
                })
                .or_insert(next);
        }

        // An explicit profile is a user instruction; do not require a
        // fingerprint hit (the device may report a generic product string).
        if let Some(want) = explicit {
            if let Some(profile) = profile_by_id(want) {
                let key = (profile.info.id, device.path.clone().unwrap_or_default());
                map.entry(key).or_insert_with(|| {
                    candidate(
                        profile,
                        device,
                        110,
                        vec![
                            format!("USB {}", device.pretty_id()),
                            "explicit profile override".into(),
                        ],
                    )
                });
            }
        }
    }

    let mut candidates: Vec<Candidate> = map.into_values().collect();
    let mut fallback = false;
    if candidates.is_empty() {
        fallback = true;
        let device = filtered.first().copied().unwrap_or(&devices[0]);
        candidates.push(candidate(
            &UVC_GENERIC,
            device,
            0,
            vec![
                format!("USB {}", device.pretty_id()),
                "fallback: unknown VID/PID or layout".into(),
            ],
        ));
    }

    // Deterministic preference when scores tie: the operator-facing default
    // is the TC001, with the P2 Pro as the alternative in the same cluster.
    fn priority(id: &str) -> u8 {
        match id {
            "topdon_tc001" => 0,
            "infiray_p2pro" => 1,
            "uvc_generic" => 9,
            _ => 5,
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| priority(a.profile_id).cmp(&priority(b.profile_id)))
            .then_with(|| a.profile_id.cmp(b.profile_id))
    });
    let ambiguous = candidates.len() > 1
        && candidates[0].score == candidates[1].score
        && candidates[0].profile_id != candidates[1].profile_id;
    if ambiguous {
        notes.push("multiple profiles match this VID/PID; choose one explicitly".into());
    }
    let selected = candidates.first().cloned();
    Detection {
        candidates,
        selected,
        fallback,
        ambiguous,
        notes,
    }
}

pub async fn detect_json(cfg: &CameraConfig) -> serde_json::Value {
    let transport = super::transport::default_transport();
    match serde_json::to_value(detect(cfg, transport.as_ref()).await) {
        Ok(value) => value,
        Err(e) => serde_json::json!({
            "candidates": [],
            "selected": null,
            "fallback": true,
            "ambiguous": false,
            "notes": [e.to_string()]
        }),
    }
}
