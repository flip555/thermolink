#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use thermolink::runtime::{RunningServer, RuntimeConfig};

struct ServerState(Mutex<Option<RunningServer>>);

fn init_logging() {
    let path = std::env::temp_dir().join("thermolink-desktop.log");
    if let Ok(file) = std::fs::File::create(path) {
        let writer = Mutex::new(file);
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("thermolink=debug"));
        let _ = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_env_filter(filter)
            .try_init();
    }
}

fn main() {
    init_logging();
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            if let Ok(resource_dir) = app.path().resource_dir() {
                for bundled in [
                    resource_dir.join("ffmpeg"),
                    resource_dir.join("ffmpeg.exe"),
                    resource_dir.join("resources").join("ffmpeg"),
                    resource_dir.join("resources").join("ffmpeg.exe"),
                ] {
                    if bundled.is_file() {
                        std::env::set_var("THERMOLINK_FFMPEG", bundled);
                        break;
                    }
                }
            }
            let localhost = IpAddr::V4(Ipv4Addr::LOCALHOST);
            let server =
                tauri::async_runtime::block_on(thermolink::runtime::start(RuntimeConfig {
                    driver: "uvc_radiometric".into(),
                    host: String::new(),
                    control_port: 3001,
                    media_port: 3000,
                    profile: None,
                    bind: SocketAddr::new(localhost, 0),
                    demo: false,
                    data_dir,
                    tls: false,
                    https_addr: SocketAddr::new(localhost, 0),
                }))?;
            let url = url::Url::parse(&format!("http://{}/", server.addr))?;
            app.manage(ServerState(Mutex::new(Some(server))));
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("ThermoLink")
                .inner_size(1280.0, 800.0)
                .min_inner_size(960.0, 600.0)
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building ThermoLink")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<ServerState>() {
                    if let Some(server) = state.0.lock().ok().and_then(|mut slot| slot.take()) {
                        server.shutdown();
                    }
                }
            }
        });
}
