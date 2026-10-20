use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use thermolink::camera::{registry, CameraDriver, FramePayload};
use thermolink::config::{CameraConfig, Config};
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
        /// UVC profile id (for example topdon_tc001); omit to auto-detect.
        #[arg(long)]
        profile: Option<String>,
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
        /// UVC profile id (for example topdon_tc001); omit to auto-detect.
        #[arg(long)]
        profile: Option<String>,
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
        /// UVC profile id (for example topdon_tc001); omit to auto-detect.
        #[arg(long)]
        profile: Option<String>,
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
            println!("profiles:");
            for profile in registry::profiles() {
                println!(
                    "  {} — {} (tested: {}, confidence: {:?})",
                    profile.info.id,
                    profile.info.display_name,
                    profile.info.tested,
                    profile.info.confidence
                );
            }
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
            profile,
            seconds,
        } => probe(driver, host, control_port, media_port, profile, seconds).await,
        Command::Stream {
            driver,
            host,
            control_port,
            media_port,
            profile,
            seconds,
            log_every_frames,
        } => {
            stream(
                driver,
                host,
                control_port,
                media_port,
                profile,
                seconds,
                log_every_frames,
            )
            .await
        }
        Command::Serve {
            driver,
            host,
            control_port,
            media_port,
            profile,
            bind,
            demo,
            data_dir,
        } => {
            serve(
                driver,
                host,
                control_port,
                media_port,
                profile,
                bind,
                demo,
                data_dir,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    driver: String,
    host: String,
    control_port: u16,
    media_port: u16,
    profile: Option<String>,
    bind: String,
    demo: bool,
    data_dir: PathBuf,
) -> Result<()> {
    let bind: std::net::SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address {bind:?}"))?;
    let https_addr: std::net::SocketAddr = "0.0.0.0:8443".parse()?;
    let running = thermolink::runtime::start(thermolink::runtime::RuntimeConfig {
        driver,
        host,
        control_port,
        media_port,
        profile,
        bind,
        demo,
        data_dir,
        tls: true,
        https_addr,
    })
    .await?;
    running.wait().await
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
    profile: Option<String>,
    seconds: u64,
) -> Result<()> {
    let mut cfg = camera_config(driver, host, control_port, media_port);
    cfg.profile = profile;
    if registry::is_uvc(&cfg.driver) {
        registry::resolve(&mut cfg).await?;
    }
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
    profile: Option<String>,
    seconds: u64,
    log_every_frames: u64,
) -> Result<()> {
    let mut cfg = camera_config(driver, host, control_port, media_port);
    cfg.profile = profile;
    if registry::is_uvc(&cfg.driver) {
        registry::resolve(&mut cfg).await?;
    }
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
