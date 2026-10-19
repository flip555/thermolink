//! Lightweight USB fingerprint table.
//!
//! This is intentionally a static table, not a database. A VID/PID match is a
//! candidate, not proof of a model; product strings and user overrides resolve
//! ambiguity.

use crate::camera::Confidence;

#[derive(Debug, Clone, Copy)]
pub struct Fingerprint {
    pub vid: u16,
    pub pid: u16,
    /// Case-insensitive substring of the USB product string. `None` means
    /// "any product with this VID/PID" and produces an ambiguity candidate.
    pub product_substring: Option<&'static str>,
    pub profile_id: &'static str,
    pub confidence: Confidence,
}

pub static FINGERPRINTS: &[Fingerprint] = &[
    // 0bda:5830 is shared by the TC001, P2 Pro, TS001, MaAnt and UTi261M.
    // Keep the cluster explicit; never silently pick one model.
    Fingerprint {
        vid: 0x0bda,
        pid: 0x5830,
        product_substring: Some("p2 pro"),
        profile_id: "infiray_p2pro",
        confidence: Confidence::Documented,
    },
    Fingerprint {
        vid: 0x0bda,
        pid: 0x5830,
        product_substring: Some("p2"),
        profile_id: "infiray_p2pro",
        confidence: Confidence::Documented,
    },
    Fingerprint {
        vid: 0x0bda,
        pid: 0x5830,
        product_substring: Some("tc001"),
        profile_id: "topdon_tc001",
        confidence: Confidence::Documented,
    },
    Fingerprint {
        vid: 0x0bda,
        pid: 0x5830,
        product_substring: None,
        profile_id: "topdon_tc001",
        confidence: Confidence::Documented,
    },
    Fingerprint {
        vid: 0x0bda,
        pid: 0x5830,
        product_substring: None,
        profile_id: "infiray_p2pro",
        confidence: Confidence::Documented,
    },
];
