//! HTTP surface. Deliberately imports `camera` and `render`, never a driver.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::camera::{Calibration, CameraSpec, FramePayload, StreamKind, ThermalImage};
use crate::render::{dt_jpeg, render_jpeg, DisplaySettings, PALETTES};
use crate::stream::FrameHub;
use crate::video::VideoTranscoder;

#[derive(Clone, Serialize, Deserialize)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub pts: Vec<[i32; 2]>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Alarm {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub threshold_c: f32,
    pub dwell_s: u32,
}

#[derive(Clone, Serialize)]
pub struct Sample {
    pub t: u64,
    pub min_c: f32,
    pub max_c: f32,
    pub mean_c: f32,
}

pub struct History {
    buf: RwLock<VecDeque<Sample>>,
}

impl History {
    fn new() -> Self {
        Self {
            buf: RwLock::new(VecDeque::with_capacity(3600)),
        }
    }

    async fn push(&self, sample: Sample) {
        let mut buf = self.buf.write().await;
        if buf.len() >= 3600 {
            buf.pop_front();
        }
        buf.push_back(sample);
    }

    async fn tail(&self, limit: usize) -> Vec<Sample> {
        let buf = self.buf.read().await;
        let skip = buf.len().saturating_sub(limit);
        buf.iter().skip(skip).cloned().collect()
    }
}

pub struct AppState {
    pub spec: CameraSpec,
    pub hub: FrameHub,
    pub display: RwLock<DisplaySettings>,
    pub calibration: RwLock<Calibration>,
    pub connected: Arc<AtomicBool>,
    pub source: String,
    pub video: Option<VideoTranscoder>,
    pub data_dir: PathBuf,
    pub history: Arc<History>,
    pub regions: RwLock<Vec<Region>>,
    pub alarms: RwLock<Vec<Alarm>>,
    alarm_started: Mutex<HashMap<String, Instant>>,
    alarm_active: Mutex<HashSet<String>>,
    pub last_alarm: RwLock<Option<serde_json::Value>>,
    pub reference: RwLock<Option<Vec<u16>>>,
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
        let regions = std::fs::read_to_string(data_dir.join("regions.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Region>>(&t).ok())
            .unwrap_or_default();
        let alarms = std::fs::read_to_string(data_dir.join("alarms.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Alarm>>(&t).ok())
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
            history: Arc::new(History::new()),
            regions: RwLock::new(regions),
            alarms: RwLock::new(alarms),
            alarm_started: Mutex::new(HashMap::new()),
            alarm_active: Mutex::new(HashSet::new()),
            last_alarm: RwLock::new(None),
            reference: RwLock::new(None),
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

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

    async fn persist_regions(&self) {
        let list = self.regions.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(
            self.data_dir.join("regions.json"),
            serde_json::to_vec_pretty(&list).unwrap_or_default(),
        );
    }

    async fn persist_alarms(&self) {
        let list = self.alarms.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(
            self.data_dir.join("alarms.json"),
            serde_json::to_vec_pretty(&list).unwrap_or_default(),
        );
    }
}

/// Samples history and evaluates alarms once a second.
pub fn spawn_monitor(st: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(img) = st.thermal_image().await else {
                continue;
            };
            let stats = img.stats();
            st.history
                .push(Sample {
                    t: now_secs(),
                    min_c: round2(stats.min_c),
                    max_c: round2(stats.max_c),
                    mean_c: round2(stats.mean_c),
                })
                .await;
            evaluate_alarms(&st, &stats).await;
        }
    });
}

async fn evaluate_alarms(st: &Arc<AppState>, stats: &crate::camera::ThermalStats) {
    let alarms = st.alarms.read().await.clone();
    let mut started = st.alarm_started.lock().await;
    let mut active = st.alarm_active.lock().await;

    for alarm in &alarms {
        let condition = match alarm.kind.as_str() {
            "below" => stats.min_c <= alarm.threshold_c,
            _ => stats.max_c >= alarm.threshold_c,
        };
        if !condition {
            started.remove(&alarm.id);
            active.remove(&alarm.id);
            continue;
        }
        let since = *started.entry(alarm.id.clone()).or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_secs(alarm.dwell_s as u64)
            && active.insert(alarm.id.clone())
        {
            let event = json!({
                "id": alarm.id,
                "name": alarm.name,
                "kind": alarm.kind,
                "threshold_c": alarm.threshold_c,
                "value_c": if alarm.kind == "below" { stats.min_c } else { stats.max_c },
                "at": now_secs(),
            });
            *st.last_alarm.write().await = Some(event.clone());
            let line = format!(
                "{}\t{}\t{}\t{:.2}C\n",
                now_secs(),
                alarm.name,
                alarm.kind,
                event["value_c"].as_f64().unwrap_or_default()
            );
            let dir = st.data_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("alarms.log"))
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
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
        .route("/api/history.json", get(history_json))
        .route("/api/point", get(point))
        .route("/api/regions", get(get_regions).post(add_region).delete(clear_regions))
        .route("/api/regions/{id}", delete(delete_region))
        .route("/api/alarms", get(get_alarms).post(add_alarm).delete(clear_alarms))
        .route("/api/alarms/{id}", delete(delete_alarm))
        .route("/api/reference", post(set_reference).delete(clear_reference))
        .route("/api/dt.jpg", get(dt_image))
        .route("/api/dt.json", get(dt_json))
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
    let has_reference = st.reference.read().await.is_some();
    let Some(frame) = st.hub.latest(StreamKind::Thermal).await else {
        return Json(json!({
            "ready": false,
            "connected": st.connected(),
            "video": st.video.is_some(),
            "display": display,
            "calibration": calibration,
            "palettes": PALETTES,
            "spec": st.spec,
            "has_reference": false,
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
        "video": st.video.is_some(),
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
        "has_reference": has_reference,
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
struct HistoryQuery {
    #[serde(default = "default_history_limit")]
    limit: usize,
}

fn default_history_limit() -> usize {
    600
}

async fn history_json(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HistoryQuery>,
) -> Json<serde_json::Value> {
    Json(json!({ "samples": st.history.tail(q.limit.min(3600)).await }))
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

async fn get_regions(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let regions = st.regions.read().await.clone();
    let img = st.thermal_image().await;
    let stats: Vec<_> = img
        .as_ref()
        .map(|i| regions.iter().filter_map(|r| region_stats(i, r)).collect())
        .unwrap_or_default();
    Json(json!({ "regions": regions, "stats": stats }))
}

#[derive(Deserialize)]
struct RegionRequest {
    #[serde(default)]
    name: String,
    #[serde(default = "default_kind")]
    kind: String,
    pts: Vec<[i32; 2]>,
}

fn default_kind() -> String {
    "rect".into()
}

async fn add_region(
    State(st): State<Arc<AppState>>,
    Json(req): Json<RegionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.pts.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "region has no points".into()));
    }
    let region = Region {
        id: format!("{}", now_secs() * 1000 + (now_ms() % 1000)),
        name: if req.name.is_empty() {
            format!("{:?} {},{}", req.kind, req.pts[0][0], req.pts[0][1])
        } else {
            req.name
        },
        kind: req.kind,
        pts: req.pts,
    };
    st.regions.write().await.push(region.clone());
    st.persist_regions().await;
    let img = st.thermal_image().await;
    let stats = img.as_ref().and_then(|i| region_stats(i, &region));
    Ok(Json(json!({ "region": region, "stats": stats, "regions": st.regions.read().await.clone() })))
}

async fn delete_region(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Json<serde_json::Value> {
    st.regions.write().await.retain(|r| r.id != id);
    st.persist_regions().await;
    Json(json!({ "ok": true }))
}

async fn clear_regions(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    st.regions.write().await.clear();
    st.persist_regions().await;
    Json(json!({ "ok": true }))
}

async fn get_alarms(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "alarms": st.alarms.read().await.clone(),
        "last_alarm": st.last_alarm.read().await.clone(),
    }))
}

#[derive(Deserialize)]
struct AlarmRequest {
    #[serde(default)]
    name: String,
    #[serde(default = "default_alarm_kind")]
    kind: String,
    threshold_c: f32,
    #[serde(default)]
    dwell_s: u32,
}

fn default_alarm_kind() -> String {
    "above".into()
}

async fn add_alarm(
    State(st): State<Arc<AppState>>,
    Json(req): Json<AlarmRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !(-200.0..=500.0).contains(&req.threshold_c) {
        return Err((StatusCode::BAD_REQUEST, "implausible threshold".into()));
    }
    let alarm = Alarm {
        id: format!("a{}", now_secs() * 1000 + (now_ms() % 1000)),
        name: if req.name.is_empty() {
            format!("{} {:.0}C", req.kind, req.threshold_c)
        } else {
            req.name
        },
        kind: if req.kind == "below" { "below".into() } else { "above".into() },
        threshold_c: req.threshold_c,
        dwell_s: req.dwell_s.min(3600),
    };
    st.alarms.write().await.push(alarm.clone());
    st.persist_alarms().await;
    Ok(Json(json!({ "alarm": alarm, "alarms": st.alarms.read().await.clone() })))
}

async fn delete_alarm(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Json<serde_json::Value> {
    st.alarms.write().await.retain(|a| a.id != id);
    st.persist_alarms().await;
    Json(json!({ "ok": true }))
}

async fn clear_alarms(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    st.alarms.write().await.clear();
    st.persist_alarms().await;
    *st.last_alarm.write().await = None;
    Json(json!({ "ok": true }))
}

async fn set_reference(State(st): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let vals: Vec<u16> = img.values().collect();
    *st.reference.write().await = Some(vals);
    Ok(Json(json!({ "ok": true, "at": now_secs() })))
}

async fn clear_reference(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    *st.reference.write().await = None;
    Json(json!({ "ok": true }))
}

async fn dt_image(State(st): State<Arc<AppState>>) -> Response {
    let Some(img) = st.thermal_image().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let Some(reference) = st.reference.read().await.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no reference frame").into_response();
    };
    match dt_jpeg(&img, &reference, 5.0, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn dt_json(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let img = st.thermal_image().await;
    let reference = st.reference.read().await.clone();
    let (Some(img), Some(reference)) = (img, reference) else {
        return Json(json!({ "ready": false }));
    };
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0;
    let mut n = 0;
    for (i, v) in img.values().enumerate() {
        if let Some(&r) = reference.get(i) {
            let d = (v as f32 - r as f32) * img.calibration.scale;
            min = min.min(d);
            max = max.max(d);
            sum += d;
            n += 1;
        }
    }
    Json(json!({
        "ready": n > 0,
        "min_c": round2(if n > 0 { min } else { 0.0 }),
        "max_c": round2(if n > 0 { max } else { 0.0 }),
        "avg_c": round2(if n > 0 { sum / n as f32 } else { 0.0 }),
        "n": n,
    }))
}

#[derive(Deserialize, Serialize)]
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
        "regions": st.regions.read().await.clone(),
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

fn region_stats(img: &ThermalImage, region: &Region) -> Option<serde_json::Value> {
    let pts = &region.pts;
    if pts.is_empty() {
        return None;
    }
    let xs: Vec<i32> = pts.iter().map(|p| p[0]).collect();
    let ys: Vec<i32> = pts.iter().map(|p| p[1]).collect();
    let x0 = xs.iter().copied().min()?.clamp(0, img.width as i32 - 1);
    let x1 = xs.iter().copied().max()?.clamp(0, img.width as i32 - 1);
    let y0 = ys.iter().copied().min()?.clamp(0, img.height as i32 - 1);
    let y1 = ys.iter().copied().max()?.clamp(0, img.height as i32 - 1);

    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0;
    let mut n = 0u32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !inside(region, x, y) {
                continue;
            }
            if let Some(c) = img.celsius_at(x as u32, y as u32) {
                min = min.min(c);
                max = max.max(c);
                sum += c;
                n += 1;
            }
        }
    }
    if n == 0 {
        return None;
    }
    Some(json!({
        "id": region.id,
        "name": region.name,
        "kind": region.kind,
        "x0": x0, "y0": y0, "x1": x1, "y1": y1,
        "n": n,
        "min_c": round2(min),
        "max_c": round2(max),
        "avg_c": round2(sum / n as f32),
    }))
}

fn inside(region: &Region, x: i32, y: i32) -> bool {
    let pts = &region.pts;
    match region.kind.as_str() {
        "point" => pts.first().is_some_and(|p| p[0] == x && p[1] == y),
        "poly" if pts.len() >= 3 => {
            let mut inside = false;
            let mut j = pts.len() - 1;
            for i in 0..pts.len() {
                let (xi, yi) = (pts[i][0], pts[i][1]);
                let (xj, yj) = (pts[j][0], pts[j][1]);
                if (yi > y) != (yj > y)
                    && x < (xj - xi) * (y - yi) / (yj - yi).max(1) + xi
                {
                    inside = !inside;
                }
                j = i;
            }
            inside
        }
        _ => {
            if pts.len() < 2 {
                return false;
            }
            let x0 = pts[0][0].min(pts[1][0]);
            let x1 = pts[0][0].max(pts[1][0]);
            let y0 = pts[0][1].min(pts[1][1]);
            let y1 = pts[0][1].max(pts[1][1]);
            x0 <= x && x <= x1 && y0 <= y && y <= y1
        }
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}
