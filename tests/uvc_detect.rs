use thermolink::camera::uvc_radiometric::detect::detect;
use thermolink::camera::uvc_radiometric::transport::mock::MockTransport;
use thermolink::camera::uvc_radiometric::types::UvcDevice;
use thermolink::config::CameraConfig;

fn device(vid: u16, pid: u16, product: Option<&str>) -> UvcDevice {
    UvcDevice {
        vid,
        pid,
        product: product.map(str::to_string),
        serial: None,
        path: Some("/dev/video0".into()),
        bus: None,
    }
}

#[tokio::test]
async fn shared_vid_pid_is_ambiguous_and_defaults_to_tc001() {
    let transport = MockTransport::new(vec![device(0x0bda, 0x5830, None)], vec![]);
    let detection = detect(&CameraConfig::default(), &transport).await;
    assert!(detection.ambiguous, "{detection:?}");
    assert_eq!(detection.candidates.len(), 2);
    assert_eq!(
        detection.selected.as_ref().unwrap().profile_id,
        "topdon_tc001"
    );
    assert!(detection
        .candidates
        .iter()
        .all(|c| !c.tested && c.confidence == thermolink::camera::Confidence::Documented));
}

#[tokio::test]
async fn product_string_breaks_the_tie() {
    let transport =
        MockTransport::new(vec![device(0x0bda, 0x5830, Some("InfiRay P2 Pro"))], vec![]);
    let detection = detect(&CameraConfig::default(), &transport).await;
    assert_eq!(
        detection.selected.as_ref().unwrap().profile_id,
        "infiray_p2pro"
    );
}

#[tokio::test]
async fn explicit_profile_wins() {
    let transport =
        MockTransport::new(vec![device(0x0bda, 0x5830, Some("InfiRay P2 Pro"))], vec![]);
    let cfg = CameraConfig {
        profile: Some("topdon_tc001".into()),
        ..Default::default()
    };
    let detection = detect(&cfg, &transport).await;
    assert_eq!(
        detection.selected.as_ref().unwrap().profile_id,
        "topdon_tc001"
    );
}

#[tokio::test]
async fn unknown_device_falls_back_without_failing() {
    let transport = MockTransport::new(vec![device(0x1234, 0x5678, Some("Mystery UVC"))], vec![]);
    let detection = detect(&CameraConfig::default(), &transport).await;
    assert!(detection.fallback);
    assert_eq!(
        detection.selected.as_ref().unwrap().profile_id,
        "uvc_generic"
    );
    assert!(!detection.selected.as_ref().unwrap().tested);
}

#[tokio::test]
async fn ht301_fingerprint_selects_ht301() {
    let transport = MockTransport::new(vec![device(0x1514, 0x0001, None)], vec![]);
    let detection = detect(&CameraConfig::default(), &transport).await;
    assert_eq!(detection.selected.as_ref().unwrap().profile_id, "ht301");
}

#[tokio::test]
async fn xtherm_v2_fingerprint_selects_t2s() {
    let transport = MockTransport::new(vec![device(0x04b4, 0x0100, None)], vec![]);
    let detection = detect(&CameraConfig::default(), &transport).await;
    assert_eq!(
        detection.selected.as_ref().unwrap().profile_id,
        "xtherm_t2s"
    );
}

#[tokio::test]
async fn hik_fingerprint_selects_hik_mini2() {
    let transport = MockTransport::new(vec![device(0x2bdf, 0x0102, None)], vec![]);
    let detection = detect(&CameraConfig::default(), &transport).await;
    assert_eq!(detection.selected.as_ref().unwrap().profile_id, "hik_mini2");
}
