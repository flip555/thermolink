#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use thermolink::runtime::{RunningServer, RuntimeConfig};

struct ServerState(Mutex<Option<RunningServer>>);

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
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
