//! HTTP surface. Deliberately imports `camera` and `render`, never a driver.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::camera::{Calibration, CameraSpec, FramePayload, StreamKind, ThermalImage};
use crate::render::{render_jpeg, DisplaySettings, PALETTES};
use crate::stream::FrameHub;
use crate::video::VideoTranscoder;

pub struct AppState {
    pub spec: CameraSpec,
    pub hub: FrameHub,
    pub display: RwLock<DisplaySettings>,
    pub calibration: RwLock<Calibration>,
    pub connected: Arc<AtomicBool>,
    pub source: String,
    pub video: Option<VideoTranscoder>,
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn new(
        spec: CameraSpec,
        hub: FrameHub,
        source: String,
        connected: Arc<AtomicBool>,
        video: Option<VideoTranscoder>,
        data_dir: PathBuf,
    ) -> Self {
        let calibration = std::fs::read_to_string(data_dir.join("calibration.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<CalibrationFile>(&t).ok())
            .map(|c| Calibration {
                scale: c.scale,
                offset: c.offset,
            })
            .unwrap_or_default();
        Self {
            spec,
            hub,
            display: RwLock::new(DisplaySettings::default()),
            calibration: RwLock::new(calibration),
            connected,
            source,
            video,
            data_dir,
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Latest thermal frame with the server's active calibration applied.
    pub async fn thermal_image(&self) -> Option<ThermalImage> {
        let frame = self.hub.latest(StreamKind::Thermal).await?;
        let img = frame.thermal()?.clone();
        let cal = *self.calibration.read().await;
        Some(ThermalImage {
            calibration: cal,
            ..img
        })
    }

    fn snapshots_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
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
        .route("/api/video.jpg", get(video_jpg))
        .route("/api/video.mjpg", get(video_mjpg))
        .route("/api/raw.h264", get(raw_h264))
        .route("/api/display", get(get_display).post(set_display))
        .route("/api/point", get(point))
        .route("/api/region", post(region))
        .route("/api/calibration", get(get_calibration).post(set_calibration))
        .route("/api/snapshot", post(snapshot))
        .route("/api/snapshots", get(list_snapshots))
        .route("/api/snapshots/{name}", get(snapshot_file))
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
        "video": st.video.is_some(),
        "seq": latest.map(|f| f.seq).unwrap_or(0),
    }))
}

async fn thermal_json(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let display = st.display.read().await.clone();
    let calibration = *st.calibration.read().await;
    let Some(frame) = st.hub.latest(StreamKind::Thermal).await else {
        return Json(json!({
            "ready": false,
            "connected": st.connected(),
            "display": display,
            "calibration": calibration,
            "palettes": PALETTES,
            "spec": st.spec,
        }));
    };
    let Some(img) = frame.thermal() else {
        return Json(json!({ "ready": false, "connected": st.connected() }));
    };
    let img = ThermalImage {
        calibration,
        ..img.clone()
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
        "calibration": calibration,
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
    let Some(img) = st.thermal_image().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let display = st.display.read().await.clone();
    match render_jpeg(&img, &display, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn thermal_mjpg(State(st): State<Arc<AppState>>) -> Response {
    let stream = async_stream::stream! {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(img) = st.thermal_image().await else { continue };
            let display = st.display.read().await.clone();
            let Ok(jpeg) = render_jpeg(&img, &display, 80) else { continue };
            yield Ok::<Bytes, std::io::Error>(Bytes::from(multipart_head(&jpeg)));
            yield Ok(Bytes::from(jpeg));
            yield Ok(Bytes::from_static(b"\r\n"));
        }
    };
    mjpeg_response(stream)
}

async fn video_jpg(State(st): State<Arc<AppState>>) -> Response {
    match st.video.as_ref().and_then(|v| v.latest()) {
        Some(jpeg) => jpeg_response(jpeg.to_vec()),
        None => (StatusCode::SERVICE_UNAVAILABLE, "no visible frame yet").into_response(),
    }
}

async fn video_mjpg(State(st): State<Arc<AppState>>) -> Response {
    let Some(video) = st.video.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "ffmpeg unavailable").into_response();
    };
    let stream = async_stream::stream! {
        let mut tick = tokio::time::interval(Duration::from_millis(66));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(jpeg) = video.latest() else { continue };
            yield Ok::<Bytes, std::io::Error>(Bytes::from(multipart_head(&jpeg)));
            yield Ok(jpeg);
            yield Ok(Bytes::from_static(b"\r\n"));
        }
    };
    mjpeg_response(stream)
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

#[derive(Deserialize)]
struct PointQuery {
    x: u32,
    y: u32,
}

async fn point(
    State(st): State<Arc<AppState>>,
    Query(q): Query<PointQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let c = img
        .celsius_at(q.x, q.y)
        .ok_or((StatusCode::BAD_REQUEST, "point outside the frame".into()))?;
    Ok(Json(json!({
        "x": q.x, "y": q.y, "c": round2(c),
        "raw": img.raw_value(q.x, q.y)
    })))
}

#[derive(Deserialize)]
struct RegionRequest {
    pts: Vec<[i32; 2]>,
    #[serde(default)]
    name: String,
}

async fn region(
    State(st): State<Arc<AppState>>,
    Json(req): Json<RegionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.pts.len() < 2 {
        return Err((StatusCode::BAD_REQUEST, "need two corners".into()));
    }
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let (w, h) = (img.width as i32, img.height as i32);
    let x0 = req.pts[0][0].min(req.pts[1][0]).clamp(0, w - 1);
    let x1 = req.pts[0][0].max(req.pts[1][0]).clamp(0, w - 1);
    let y0 = req.pts[0][1].min(req.pts[1][1]).clamp(0, h - 1);
    let y1 = req.pts[0][1].max(req.pts[1][1]).clamp(0, h - 1);

    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0f32;
    let mut n = 0u32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            if let Some(c) = img.celsius_at(x as u32, y as u32) {
                min = min.min(c);
                max = max.max(c);
                sum += c;
                n += 1;
            }
        }
    }
    if n == 0 {
        return Err((StatusCode::BAD_REQUEST, "empty region".into()));
    }
    Ok(Json(json!({
        "name": req.name,
        "x0": x0, "y0": y0, "x1": x1, "y1": y1,
        "n": n,
        "min_c": round2(min),
        "max_c": round2(max),
        "avg_c": round2(sum / n as f32),
    })))
}

#[derive(Deserialize, serde::Serialize)]
struct CalibrationFile {
    scale: f32,
    offset: f32,
}

async fn get_calibration(State(st): State<Arc<AppState>>) -> Json<Calibration> {
    Json(*st.calibration.read().await)
}

#[derive(Deserialize)]
struct CalibrationRequest {
    scale: Option<f32>,
    offset: Option<f32>,
    /// Two `(raw, true_celsius)` pairs for a two-point fit.
    points: Option<[[f32; 2]; 2]>,
}

async fn set_calibration(
    State(st): State<Arc<AppState>>,
    Json(req): Json<CalibrationRequest>,
) -> Result<Json<Calibration>, (StatusCode, String)> {
    let next = if let Some(points) = req.points {
        Calibration::fit([
            (points[0][0] as u16, points[0][1]),
            (points[1][0] as u16, points[1][1]),
        ])
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    } else {
        Calibration {
            scale: req.scale.unwrap_or(0.01),
            offset: req.offset.unwrap_or(0.0),
        }
    };
    if !(0.0001..=1.0).contains(&next.scale.abs()) || !(-200.0..=200.0).contains(&next.offset) {
        return Err((StatusCode::BAD_REQUEST, "implausible calibration".into()));
    }
    *st.calibration.write().await = next;
    let _ = std::fs::create_dir_all(&st.data_dir);
    let file = CalibrationFile {
        scale: next.scale,
        offset: next.offset,
    };
    let _ = std::fs::write(
        st.data_dir.join("calibration.json"),
        serde_json::to_vec_pretty(&file).unwrap_or_default(),
    );
    Ok(Json(next))
}

async fn snapshot(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let display = st.display.read().await.clone();
    let jpeg = render_jpeg(&img, &display, 90)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stats = img.stats();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let name = format!("{}-{:03}", now.as_secs(), now.subsec_millis());
    let dir = st.snapshots_dir();
    std::fs::create_dir_all(&dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::write(dir.join(format!("{name}.jpg")), &jpeg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::write(dir.join(format!("{name}.raw")), &img.raw)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let meta = json!({
        "name": name,
        "at": now.as_secs(),
        "stats": stats,
        "calibration": img.calibration,
        "display": display,
        "w": img.width, "h": img.height,
    });
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&meta).unwrap_or_default(),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "ok": true, "name": name,
        "jpg": format!("/api/snapshots/{name}"),
        "raw": format!("/api/snapshots/{name}.raw"),
        "stats": { "min_c": round2(stats.min_c), "max_c": round2(stats.max_c), "mean_c": round2(stats.mean_c) }
    })))
}

async fn list_snapshots(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut names: Vec<String> = std::fs::read_dir(st.snapshots_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    name.strip_suffix(".jpg").map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort_by(|a, b| b.cmp(a));
    Json(json!({ "snapshots": names }))
}

async fn snapshot_file(
    State(st): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let path = st.snapshots_dir().join(format!("{name}.jpg"));
    match std::fs::read(path) {
        Ok(bytes) => jpeg_response(bytes),
        Err(_) => (StatusCode::NOT_FOUND, "no such snapshot").into_response(),
    }
}

fn multipart_head(jpeg: &[u8]) -> String {
    format!(
        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    )
}

fn jpeg_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .unwrap()
}

fn mjpeg_response<S>(stream: S) -> Response
where
    S: futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
{
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

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}
