//! Shared process runtime used by `serve` and the future desktop shell.
//!
//! `start` builds the driver, FrameHub, AppState, supervisor and HTTP
//! listener and returns a `RunningServer`. A desktop shell can bind localhost
//! first, open a webview at the resulting address, and shut the server down on
//! exit without duplicating any of this logic.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::camera::{registry, CameraDriver};
use crate::config::CameraConfig;
use crate::server::{self, AppState, CameraRuntime};
use crate::stream::FrameHub;

pub struct RuntimeConfig {
    pub driver: String,
    pub host: String,
    pub control_port: u16,
    pub media_port: u16,
    pub profile: Option<String>,
    pub bind: SocketAddr,
    pub demo: bool,
    pub data_dir: PathBuf,
    /// Enable the self-signed HTTPS listener. Desktop localhost mode disables it.
    pub tls: bool,
    pub https_addr: SocketAddr,
}

pub struct RunningServer {
    pub addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<Result<()>>,
}

impl RunningServer {
    /// Wait until the HTTP listener exits.
    pub async fn wait(self) -> Result<()> {
        self.handle.await.context("server task panicked")?
    }

    /// Ask the HTTP and HTTPS listeners to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

pub async fn start(cfg: RuntimeConfig) -> Result<RunningServer> {
    insert_crypto_provider();

    let mut camera = load_camera_config(&cfg);
    if cfg.demo {
        camera.driver = "mock".to_string();
    }
    if camera.profile.is_none() {
        camera.profile = cfg.profile.clone();
    }
    if registry::is_uvc(&camera.driver) {
        if let Err(e) = registry::resolve(&mut camera).await {
            warn!(error = %e, "UVC detection failed; using the uncalibrated fallback");
            camera.profile = Some("uvc_generic".into());
        }
    }

    let driver: Arc<dyn CameraDriver> = Arc::from(registry::build(&camera)?);
    let hub = FrameHub::new(512);
    let connected = Arc::new(AtomicBool::new(false));
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let video = crate::video::spawn(hub.clone(), 960, 15);
    if video.is_none() {
        warn!("ffmpeg unavailable — visible view disabled");
    }
    let state = Arc::new(AppState::new(
        driver.spec(),
        hub.clone(),
        driver.id().to_string(),
        connected.clone(),
        video,
        driver,
        camera.host.clone(),
        cfg.data_dir.clone(),
    ));

    tokio::spawn(camera_supervisor(state.clone()));
    server::spawn_monitor(state.clone());
    server::spawn_prerecorder(state.clone());

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("binding {}", cfg.bind))?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let router = server::router(state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
        })
        .await
        .context("HTTP server stopped")
    });
    info!(%addr, "ThermoLink web UI ready");

    if cfg.tls {
        match ensure_tls_cert(&cfg.data_dir) {
            Ok((cert, key)) => {
                match axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await {
                    Ok(tls) => {
                        let https_router = server::router(state.clone())
                            .layer(axum::middleware::from_fn(mark_https));
                        let https_addr = cfg.https_addr;
                        let mut tls_shutdown = shutdown_tx.subscribe();
                        tokio::spawn(async move {
                            let serve = axum_server::bind_rustls(https_addr, tls).serve(
                                https_router.into_make_service_with_connect_info::<SocketAddr>(),
                            );
                            tokio::select! {
                                result = serve => {
                                    if let Err(e) = result {
                                        warn!(error = %e, "https listener stopped");
                                    }
                                }
                                _ = tls_shutdown.changed() => {}
                            }
                        });
                        info!(%https_addr, "ThermoLink HTTPS ready (self-signed)");
                    }
                    Err(e) => warn!(error = %e, "https disabled: certificate could not be loaded"),
                }
            }
            Err(e) => warn!(error = %e, "https disabled: certificate could not be created"),
        }
    }

    Ok(RunningServer {
        addr,
        shutdown: shutdown_tx,
        handle,
    })
}

fn load_camera_config(cfg: &RuntimeConfig) -> CameraConfig {
    std::fs::read_to_string(cfg.data_dir.join("camera.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| CameraConfig {
            driver: cfg.driver.clone(),
            host: cfg.host.clone(),
            control_port: cfg.control_port,
            media_port: cfg.media_port,
            profile: cfg.profile.clone(),
            ..Default::default()
        })
}

fn insert_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn ensure_tls_cert(data_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let cert = data_dir.join("tls-cert.pem");
    let key = data_dir.join("tls-key.pem");
    if cert.exists() && key.exists() {
        return Ok((cert, key));
    }
    std::fs::create_dir_all(data_dir)?;
    let status = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "3650",
            "-keyout",
            key.to_str().context("key path")?,
            "-out",
            cert.to_str().context("cert path")?,
            "-subj",
            "/CN=thermolink",
            "-addext",
            "subjectAltName=IP:172.20.1.85,IP:172.20.5.127,IP:127.0.0.1,DNS:localhost",
        ])
        .status()
        .context("running openssl")?;
    if !status.success() {
        anyhow::bail!("openssl could not generate a certificate");
    }
    Ok((cert, key))
}

async fn mark_https(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    req.headers_mut().insert(
        "x-forwarded-proto",
        axum::http::HeaderValue::from_static("https"),
    );
    next.run(req).await
}

/// Owns the camera connection: open, read, publish, reconnect.
async fn camera_supervisor(st: Arc<AppState>) {
    let mut gen = st.camera_gen.subscribe();
    let mut backoff = 2u64;
    loop {
        let before = *gen.borrow();
        let runtime = st.camera.load_full();
        let driver = runtime.driver.clone();
        match tokio::time::timeout(Duration::from_secs(20), driver.open()).await {
            Ok(Ok(mut session)) => {
                st.connected.store(true, Ordering::Relaxed);
                // The pre-open spec cannot know a negotiated UVC frame size or
                // the device auto-detection selected. Adopt the session's
                // spec so /api/spec and the UI reflect the live camera.
                let session_spec = session.spec().clone();
                let runtime = st.camera.load_full();
                st.camera.store(Arc::new(CameraRuntime {
                    driver: runtime.driver.clone(),
                    spec: session_spec,
                }));
                info!(driver = driver.id(), "camera session open");
                loop {
                    tokio::select! {
                        r = session.next_frame() => match r {
                            Ok(frame) => {
                                backoff = 2;
                                st.hub.publish(frame).await;
                            }
                            Err(e) => { warn!(error = %e, "camera stream ended"); break; }
                        },
                        _ = gen.changed() => break,
                    }
                }
                session.close().await;
                st.connected.store(false, Ordering::Relaxed);
            }
            Ok(Err(e)) => warn!(error = %e, "camera connect failed"),
            Err(_) => warn!(driver = driver.id(), "camera open timed out"),
        }
        if *gen.borrow() != before {
            continue;
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}
