use std::sync::atomic::{AtomicBool, Ordering};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use thermolink::camera::{registry, CameraDriver, FramePayload};
use thermolink::config::{CameraConfig, Config};
use thermolink::server::{self, AppState};
use thermolink::stream::FrameHub;
use tracing::{info, warn};

#[derive(Parser)]
#[command(
    name = "thermolink",
    version,
    about = "Modular thermal-camera relay: one UI, pluggable camera drivers"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the camera drivers compiled into this binary.
    Drivers,

    /// Connect, run the handshake, and report what the camera is.
    Probe {
        #[arg(long, default_value = "qianli")]
        driver: String,
        #[arg(long, default_value = "192.168.1.64")]
        host: String,
        #[arg(long, default_value_t = 3001)]
        control_port: u16,
        #[arg(long, default_value_t = 3000)]
        media_port: u16,
        /// How long to watch frames after the handshake.
        #[arg(long, default_value_t = 4)]
        seconds: u64,
    },

    /// Run the camera loop (reconnects forever) until interrupted.
    Stream {
        #[arg(long, default_value = "qianli")]
        driver: String,
        #[arg(long, default_value = "192.168.1.64")]
        host: String,
        #[arg(long, default_value_t = 3001)]
        control_port: u16,
        #[arg(long, default_value_t = 3000)]
        media_port: u16,
        /// Stop after N seconds; 0 means run until Ctrl-C.
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Log thermal statistics every N thermal frames.
        #[arg(long, default_value_t = 60)]
        log_every_frames: u64,
    },

    /// Print the default configuration.
    Config,

    /// Serve the web UI (camera loop + HTTP on one process).
    Serve {
        #[arg(long, default_value = "qianli")]
        driver: String,
        #[arg(long, default_value = "192.168.1.64")]
        host: String,
        #[arg(long, default_value_t = 3001)]
        control_port: u16,
        #[arg(long, default_value_t = 3000)]
        media_port: u16,
        #[arg(long, default_value = "0.0.0.0:8099")]
        bind: String,
        /// Use the simulated camera — no hardware needed.
        #[arg(long, default_value_t = false)]
        demo: bool,
        /// State directory (snapshots, captures, settings).
        #[arg(long, default_value = "thermolink-data")]
        data_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "thermolink=info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Command::Drivers => {
            println!("{}", registry::KNOWN.join("\n"));
            Ok(())
        }
        Command::Config => {
            println!("{}", serde_json::to_string_pretty(&Config::default())?);
            Ok(())
        }
        Command::Probe {
            driver,
            host,
            control_port,
            media_port,
            seconds,
        } => probe(driver, host, control_port, media_port, seconds).await,
        Command::Stream {
            driver,
            host,
            control_port,
            media_port,
            seconds,
            log_every_frames,
        } => stream(driver, host, control_port, media_port, seconds, log_every_frames).await,
        Command::Serve {
            driver,
            host,
            control_port,
            media_port,
            bind,
            demo,
            data_dir,
        } => serve(driver, host, control_port, media_port, bind, demo, data_dir).await,
    }
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
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650",
            "-keyout", key.to_str().context("key path")?,
            "-out", cert.to_str().context("cert path")?,
            "-subj", "/CN=thermolink",
            "-addext", "subjectAltName=IP:172.20.1.85,IP:172.20.5.127,IP:127.0.0.1,DNS:localhost",
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

async fn serve(
    driver: String,
    host: String,
    control_port: u16,
    media_port: u16,
    bind: String,
    demo: bool,
    data_dir: PathBuf,
) -> Result<()> {
    insert_crypto_provider();
    let saved: Option<thermolink::config::CameraConfig> = std::fs::read_to_string(data_dir.join("camera.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let (driver, host, control_port, media_port) = match saved {
        Some(c) => (c.driver, c.host, c.control_port, c.media_port),
        None => (driver, host, control_port, media_port),
    };
    let driver = if demo { "mock".to_string() } else { driver };
    let cfg = camera_config(driver, host, control_port, media_port);
    let driver: Arc<dyn CameraDriver> = Arc::from(registry::build(&cfg)?);
    let hub = FrameHub::new(512);
    let connected = Arc::new(AtomicBool::new(false));
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let video = thermolink::video::spawn(hub.clone(), 960, 15);
    if video.is_none() {
        tracing::warn!("ffmpeg unavailable — visible view disabled");
    }
    let state = Arc::new(AppState::new(
        driver.spec(),
        hub.clone(),
        driver.id().to_string(),
        connected.clone(),
        video,
        driver.clone(),
        cfg.host.clone(),
        data_dir.clone(),
    ));

    tokio::spawn(camera_supervisor(driver, hub.clone(), connected));
    server::spawn_monitor(state.clone());
    server::spawn_prerecorder(state.clone());

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    info!(%bind, "ThermoLink web UI ready");

    match ensure_tls_cert(&data_dir) {
        Ok((cert, key)) => match axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await {
            Ok(tls) => {
                let addr: std::net::SocketAddr = "0.0.0.0:8443".parse()?;
                let https_router = server::router(state.clone())
                    .layer(axum::middleware::from_fn(mark_https));
                tokio::spawn(async move {
                    if let Err(e) = axum_server::bind_rustls(addr, tls)
                        .serve(https_router.into_make_service_with_connect_info::<std::net::SocketAddr>())
                        .await
                    {
                        warn!(error = %e, "https listener stopped");
                    }
                });
                info!(%addr, "ThermoLink HTTPS ready (self-signed)");
            }
            Err(e) => warn!(error = %e, "https disabled: certificate could not be loaded"),
        },
        Err(e) => warn!(error = %e, "https disabled: certificate could not be created"),
    }

    axum::serve(
        listener,
        server::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Owns the camera connection: open, read, publish, reconnect.
async fn camera_supervisor(
    driver: Arc<dyn CameraDriver>,
    hub: FrameHub,
    connected: Arc<AtomicBool>,
) {
    let mut backoff = 2u64;
    loop {
        match tokio::time::timeout(Duration::from_secs(20), driver.open()).await {
            Ok(Ok(mut session)) => {
                connected.store(true, Ordering::Relaxed);
                info!(driver = driver.id(), "camera session open");
                loop {
                    match session.next_frame().await {
                        Ok(frame) => {
                            backoff = 2;
                            hub.publish(frame).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "camera stream ended");
                            break;
                        }
                    }
                }
                session.close().await;
                connected.store(false, Ordering::Relaxed);
            }
            Ok(Err(e)) => warn!(error = %e, "camera connect failed"),
            Err(_) => warn!(driver = driver.id(), "camera open timed out"),
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

fn camera_config(driver: String, host: String, control_port: u16, media_port: u16) -> CameraConfig {
    CameraConfig {
        driver,
        host,
        control_port,
        media_port,
        ..Default::default()
    }
}

async fn probe(
    driver: String,
    host: String,
    control_port: u16,
    media_port: u16,
    seconds: u64,
) -> Result<()> {
    let cfg = camera_config(driver, host, control_port, media_port);
    let driver = registry::build(&cfg)?;
    let spec = driver.spec();
    println!("{}", serde_json::to_string_pretty(&spec)?);

    let mut session = tokio::time::timeout(Duration::from_secs(12), driver.open())
        .await
        .context("handshake timed out after 12s")??;
    info!("handshake ok — watching frames for {seconds}s");

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let (mut visible_bytes, mut thermal_frames) = (0u64, 0u64);
    let mut first_stats = None;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = match tokio::time::timeout(remaining, session.next_frame()).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                warn!(error = %e, "stream ended");
                break;
            }
            Err(_) => break,
        };
        match &frame.payload {
            FramePayload::Video(b) => visible_bytes += b.len() as u64,
            FramePayload::Thermal(img) => {
                thermal_frames += 1;
                if first_stats.is_none() {
                    first_stats = Some(img.stats());
                }
            }
        }
    }

    session.close().await;
    println!(
        "visible: {:.1} KiB, thermal frames: {thermal_frames}",
        visible_bytes as f64 / 1024.0
    );
    if let Some(s) = first_stats {
        println!(
            "thermal: {:.2} .. {:.2} °C (mean {:.2}), hot {}x{}",
            s.min_c, s.max_c, s.mean_c, s.hot.0, s.hot.1
        );
    } else {
        anyhow::bail!("no thermal frames arrived — the session flag was probably rejected");
    }
    Ok(())
}

async fn stream(
    driver: String,
    host: String,
    control_port: u16,
    media_port: u16,
    seconds: u64,
    log_every_frames: u64,
) -> Result<()> {
    let cfg = camera_config(driver, host, control_port, media_port);
    let driver = registry::build(&cfg)?;
    let hub = FrameHub::new(256);
    let deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds));
    let mut backoff = Duration::from_secs(cfg.reconnect_secs.max(1));

    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let run = run_session(driver.as_ref(), &hub, log_every_frames, deadline);
        tokio::select! {
            result = run => {
                match result {
                    Ok(()) => break,
                    Err(e) => warn!(error = %e, "session ended; reconnecting in {}s", backoff.as_secs()),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("interrupted");
                return Ok(());
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = tokio::signal::ctrl_c() => {
                info!("interrupted");
                return Ok(());
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
    Ok(())
}

async fn run_session(
    driver: &dyn CameraDriver,
    hub: &FrameHub,
    log_every_frames: u64,
    deadline: Option<Instant>,
) -> Result<()> {
    let mut session = driver.open().await?;
    info!(driver = driver.id(), "session open");

    let (mut visible_bytes, mut thermal_frames) = (0u64, 0u64);
    let mut window = Instant::now();
    let log_every = log_every_frames.max(1);

    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            session.close().await;
            return Ok(());
        }
        let frame = session.next_frame().await?;
        match &frame.payload {
            FramePayload::Video(b) => visible_bytes += b.len() as u64,
            FramePayload::Thermal(img) => {
                thermal_frames += 1;
                if thermal_frames % log_every == 0 {
                    let s = img.stats();
                    info!(
                        frames = thermal_frames,
                        min = s.min_c,
                        max = s.max_c,
                        mean = s.mean_c,
                        "thermal"
                    );
                }
            }
        }
        hub.publish(frame).await;

        if window.elapsed() >= Duration::from_secs(5) {
            info!(
                visible_kib = visible_bytes / 1024,
                thermal_fps = thermal_frames as f64 / window.elapsed().as_secs_f64(),
                subscribers = hub.subscribers(),
                "rate"
            );
            visible_bytes = 0;
            thermal_frames = 0;
            window = Instant::now();
        }
    }
}
