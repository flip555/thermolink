//! HTTP surface. Deliberately imports `camera` and `render`, never a driver.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bytes::Bytes;
use serde_json::json;
use tokio::sync::RwLock;

use crate::camera::{CameraSpec, FramePayload, StreamKind};
use crate::render::{render_jpeg, DisplaySettings, PALETTES};
use crate::stream::FrameHub;

pub struct AppState {
    pub spec: CameraSpec,
    pub hub: FrameHub,
    pub display: RwLock<DisplaySettings>,
    pub connected: Arc<AtomicBool>,
    pub source: String,
}

impl AppState {
    pub fn new(
        spec: CameraSpec,
        hub: FrameHub,
        source: String,
        connected: Arc<AtomicBool>,
    ) -> Self {
        Self {
            spec,
            hub,
            display: RwLock::new(DisplaySettings::default()),
            connected,
            source,
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/spec", get(spec))
        .route("/api/health", get(health))
        .route("/api/thermal.json", get(thermal_json))
        .route("/api/thermal.jpg", get(thermal_jpg))
        .route("/api/thermal.mjpg", get(thermal_mjpg))
        .route("/api/display", get(get_display).post(set_display))
        .route("/api/raw.h264", get(raw_h264))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../web/index.html"))
}

async fn spec(State(st): State<Arc<AppState>>) -> Json<CameraSpec> {
    Json(st.spec.clone())
}

async fn health(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let latest = st.hub.latest(StreamKind::Thermal).await;
    Json(json!({
        "ok": true,
        "source": st.source,
        "connected": st.connected(),
        "ready": latest.is_some(),
        "seq": latest.map(|f| f.seq).unwrap_or(0),
    }))
}

async fn thermal_json(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let display = st.display.read().await.clone();
    let latest = st.hub.latest(StreamKind::Thermal).await;
    let Some(frame) = latest else {
        return Json(json!({
            "ready": false,
            "connected": st.connected(),
            "display": display,
            "palettes": PALETTES,
            "spec": st.spec,
        }));
    };
    let Some(img) = frame.thermal() else {
        return Json(json!({ "ready": false, "connected": st.connected() }));
    };
    let stats = img.stats();
    Json(json!({
        "ready": true,
        "connected": st.connected(),
        "seq": frame.seq,
        "stats": {
            "min_c": round2(stats.min_c),
            "max_c": round2(stats.max_c),
            "mean_c": round2(stats.mean_c),
            "hot": [stats.hot.0, stats.hot.1],
            "cold": [stats.cold.0, stats.cold.1],
            "w": stats.width,
            "h": stats.height,
        },
        "peaks": img.peaks(3).iter().map(|p| json!({
            "x": p.x, "y": p.y, "c": round2(p.celsius)
        })).collect::<Vec<_>>(),
        "display": display,
        "palettes": PALETTES,
        "spec": st.spec,
    }))
}

async fn get_display(State(st): State<Arc<AppState>>) -> Json<DisplaySettings> {
    Json(st.display.read().await.clone())
}

async fn set_display(
    State(st): State<Arc<AppState>>,
    Json(next): Json<DisplaySettings>,
) -> Result<Json<DisplaySettings>, (StatusCode, String)> {
    if !PALETTES.contains(&next.palette.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown palette {:?}", next.palette),
        ));
    }
    *st.display.write().await = next.clone();
    Ok(Json(next))
}

async fn thermal_jpg(State(st): State<Arc<AppState>>) -> Response {
    let Some(frame) = st.hub.latest(StreamKind::Thermal).await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let Some(img) = frame.thermal() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let display = st.display.read().await.clone();
    match render_jpeg(img, &display, 85) {
        Ok(jpeg) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(jpeg))
            .unwrap(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn thermal_mjpg(State(st): State<Arc<AppState>>) -> Response {
    let stream = async_stream::stream! {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(frame) = st.hub.latest(StreamKind::Thermal).await else { continue };
            let Some(img) = frame.thermal() else { continue };
            let display = st.display.read().await.clone();
            let Ok(jpeg) = render_jpeg(img, &display, 80) else { continue };
            let head = format!(
                "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                jpeg.len()
            );
            yield Ok::<Bytes, std::io::Error>(Bytes::from(head));
            yield Ok(Bytes::from(jpeg));
            yield Ok(Bytes::from_static(b"\r\n"));
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Raw visible stream passthrough for clients that decode H.264 themselves.
async fn raw_h264(State(st): State<Arc<AppState>>) -> Response {
    let mut rx = st.hub.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if let FramePayload::Video(bytes) = &frame.payload {
                        yield Ok::<Bytes, std::io::Error>(bytes.clone());
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/h264")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}
