use thermolink::config::{CameraConfig, DeviceSelector};

#[test]
fn old_camera_json_still_parses() {
    let old = r#"{
        "driver":"qianli",
        "host":"192.168.1.64",
        "control_port":3001,
        "media_port":3000,
        "reconnect_secs":8,
        "options":{}
    }"#;
    let cfg: CameraConfig = serde_json::from_str(old).expect("legacy camera.json");
    assert_eq!(cfg.driver, "qianli");
    assert!(cfg.auto_detect, "new fields default to auto-detect");
    assert!(cfg.profile.is_none());
    assert!(cfg.device.vid.is_none());
}

#[test]
fn uvc_camera_json_round_trips() {
    let cfg = CameraConfig {
        driver: "uvc_radiometric".into(),
        profile: Some("topdon_tc001".into()),
        auto_detect: false,
        device: DeviceSelector {
            vid: Some(0x0bda),
            pid: Some(0x5830),
            serial: None,
            path: Some("/dev/video2".into()),
        },
        ..Default::default()
    };
    let text = serde_json::to_string(&cfg).expect("encode");
    let back: CameraConfig = serde_json::from_str(&text).expect("decode");
    assert_eq!(back.profile.as_deref(), Some("topdon_tc001"));
    assert!(!back.auto_detect);
    assert_eq!(back.device.vid, Some(0x0bda));
    assert_eq!(back.device.pid, Some(0x5830));
    assert_eq!(back.device.path.as_deref(), Some("/dev/video2"));
}
