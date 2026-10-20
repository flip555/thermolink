use std::time::Duration;

use thermolink::runtime::{start, RuntimeConfig};

#[tokio::test]
async fn runtime_binds_localhost_and_serves_health() {
    let data_dir = std::env::temp_dir().join(format!("thermolink-runtime-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let running = start(RuntimeConfig {
        driver: "mock".into(),
        host: "127.0.0.1".into(),
        control_port: 3001,
        media_port: 3000,
        profile: None,
        bind: "127.0.0.1:0".parse().expect("bind"),
        demo: true,
        data_dir: data_dir.clone(),
        tls: false,
        https_addr: "127.0.0.1:0".parse().expect("https bind"),
    })
    .await
    .expect("runtime start");

    let url = format!("http://{}/api/health", running.addr);
    let body = reqwest::get(&url)
        .await
        .expect("health request")
        .text()
        .await
        .expect("health body");
    assert!(body.contains("\"ok\":true"), "{body}");

    running.shutdown();
    let stopped = tokio::time::timeout(Duration::from_secs(5), running.wait())
        .await
        .expect("shutdown timeout");
    stopped.expect("server task");
    let _ = std::fs::remove_dir_all(&data_dir);
}
