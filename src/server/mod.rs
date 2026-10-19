//! HTTP surface. Deliberately imports `camera` and `render`, never a driver.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::camera::{Calibration, CameraDriver, CameraSpec, FramePayload, StreamKind, ThermalImage};
use crate::render::{dt_jpeg, render_jpeg, DisplaySettings, IsoBand, PALETTES};
use crate::stream::FrameHub;
use crate::video::VideoTranscoder;

#[derive(Clone, Serialize, Deserialize)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub pts: Vec<[i32; 2]>,
    #[serde(default, rename = "ref")]
    pub is_ref: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Alarm {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub threshold_c: f32,
    pub dwell_s: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Align {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub rot: f32,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Default for Align {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, scale: 1.0, rot: 0.0, flip_h: false, flip_v: false }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub discord: Option<String>,
    pub slack: Option<String>,
    pub telegram_token: Option<String>,
    pub telegram_chat: Option<String>,
    pub webhook: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrerecordConfig {
    pub enabled: bool,
    pub before_s: u32,
    pub after_s: u32,
}

impl Default for PrerecordConfig {
    fn default() -> Self {
        Self { enabled: true, before_s: 5, after_s: 5 }
    }
}

#[derive(Clone)]
struct RingFrame {
    at: Instant,
    jpeg: Bytes,
}

struct ActiveClip {
    until: Instant,
    frames: Vec<RingFrame>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Reference {
    pub id: String,
    pub name: String,
    pub at: u64,
    pub vals: Vec<u16>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthFile {
    pub username: String,
    pub hash: String,
}

#[derive(Clone, Serialize, PartialEq)]
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

    async fn clear(&self) {
        self.buf.write().await.clear();
    }

    async fn tail(&self, limit: usize) -> Vec<Sample> {
        let buf = self.buf.read().await;
        let skip = buf.len().saturating_sub(limit);
        buf.iter().skip(skip).cloned().collect()
    }
}

static SEQ: AtomicU64 = AtomicU64::new(1);

pub struct AppState {
    pub spec: CameraSpec,
    pub hub: FrameHub,
    pub display: RwLock<DisplaySettings>,
    pub align: RwLock<Align>,
    pub notify: RwLock<NotifyConfig>,
    pub auth: RwLock<Option<AuthFile>>,
    sessions: RwLock<HashMap<String, Instant>>,
    setup_token: String,
    pub calibration: RwLock<Calibration>,
    pub connected: Arc<AtomicBool>,
    pub source: String,
    pub video: Option<VideoTranscoder>,
    pub driver: Arc<dyn CameraDriver>,
    pub camera_host: String,
    pub data_dir: PathBuf,
    pub history: Arc<History>,
    pub regions: RwLock<Vec<Region>>,
    pub alarms: RwLock<Vec<Alarm>>,
    alarm_started: Mutex<HashMap<String, Instant>>,
    alarm_active: Mutex<HashSet<String>>,
    pub last_alarm: RwLock<Option<serde_json::Value>>,
    pub reference: RwLock<Option<Vec<u16>>>,
    pub references: RwLock<Vec<Reference>>,
    pub active_ref: RwLock<Option<String>>,
    pub prerecord: RwLock<PrerecordConfig>,
    prerec_ring: Arc<Mutex<VecDeque<RingFrame>>>,
    active_clip: Arc<Mutex<Option<ActiveClip>>>,
}

impl AppState {
    pub fn new(
        spec: CameraSpec,
        hub: FrameHub,
        source: String,
        connected: Arc<AtomicBool>,
        video: Option<VideoTranscoder>,
        driver: Arc<dyn CameraDriver>,
        camera_host: String,
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
        let display = std::fs::read_to_string(data_dir.join("display.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<DisplaySettings>(&t).ok())
            .unwrap_or_default();
        let align = std::fs::read_to_string(data_dir.join("align.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Align>(&t).ok())
            .unwrap_or_default();
        let notify = std::fs::read_to_string(data_dir.join("notifications.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<NotifyConfig>(&t).ok())
            .unwrap_or_default();
        let auth = std::fs::read_to_string(data_dir.join("auth.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<AuthFile>(&t).ok());
        let prerecord = std::fs::read_to_string(data_dir.join("prerecord.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<PrerecordConfig>(&t).ok())
            .unwrap_or_default();
        let references = std::fs::read_to_string(data_dir.join("references.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Reference>>(&t).ok())
            .unwrap_or_default();
        let active_id = std::fs::read_to_string(data_dir.join("reference-active.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .filter(|id| references.iter().any(|r| &r.id == id));
        let active_vals = active_id
            .as_ref()
            .and_then(|id| references.iter().find(|r| &r.id == id))
            .map(|r| r.vals.clone());
        let mut token_bytes = [0u8; 8];
        let mut rng = OsRng;
        rng.fill_bytes(&mut token_bytes);
        let setup_token: String = token_bytes.iter().map(|b| format!("{b:02x}")).collect();
        if auth.is_none() {
            tracing::info!(setup_token = %setup_token, "first-run setup token for non-local clients");
        }
        Self {
            spec,
            hub,
            display: RwLock::new(display),
            align: RwLock::new(align),
            notify: RwLock::new(notify),
            auth: RwLock::new(auth),
            sessions: RwLock::new(HashMap::new()),
            setup_token,
            calibration: RwLock::new(calibration),
            connected,
            source,
            video,
            driver,
            camera_host,
            data_dir,
            history: Arc::new(History::new()),
            regions: RwLock::new(regions),
            alarms: RwLock::new(alarms),
            alarm_started: Mutex::new(HashMap::new()),
            alarm_active: Mutex::new(HashSet::new()),
            last_alarm: RwLock::new(None),
            reference: RwLock::new(active_vals),
            references: RwLock::new(references),
            active_ref: RwLock::new(active_id),
            prerecord: RwLock::new(prerecord),
            prerec_ring: Arc::new(Mutex::new(VecDeque::with_capacity(400))),
            active_clip: Arc::new(Mutex::new(None)),
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

    async fn persist_display(&self) {
        let d = self.display.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(self.data_dir.join("display.json"), serde_json::to_vec_pretty(&d).unwrap_or_default());
    }

    async fn persist_align(&self) {
        let a = self.align.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(self.data_dir.join("align.json"), serde_json::to_vec_pretty(&a).unwrap_or_default());
    }

    async fn persist_notify(&self) {
        let n = self.notify.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(self.data_dir.join("notifications.json"), serde_json::to_vec_pretty(&n).unwrap_or_default());
    }

    async fn persist_references(&self) {
        let list = self.references.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(self.data_dir.join("references.json"), serde_json::to_vec_pretty(&list).unwrap_or_default());
    }

    async fn persist_active_ref(&self) {
        let id = self.active_ref.read().await.clone();
        let path = self.data_dir.join("reference-active.json");
        let _ = std::fs::create_dir_all(&self.data_dir);
        match id {
            Some(id) => {
                let _ = std::fs::write(path, serde_json::to_vec(&json!({ "id": id })).unwrap_or_default());
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    async fn persist_prerecord(&self) {
        let c = self.prerecord.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(self.data_dir.join("prerecord.json"), serde_json::to_vec_pretty(&c).unwrap_or_default());
    }

    fn captures_dir(&self) -> PathBuf {
        self.data_dir.join("captures")
    }

    async fn persist_auth(&self) {
        let auth = self.auth.read().await.clone();
        let _ = std::fs::create_dir_all(&self.data_dir);
        if let Some(auth) = auth {
            let _ = std::fs::write(self.data_dir.join("auth.json"), serde_json::to_vec_pretty(&auth).unwrap_or_default());
        }
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
            append_region_log(&st, &img).await;
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
            let text = format!(
                "ThermoLink alarm: {} — {:.1}°C ({} {:.1}°C)",
                alarm.name,
                event["value_c"].as_f64().unwrap_or_default(),
                alarm.kind,
                alarm.threshold_c
            );
            let st2 = st.clone();
            tokio::spawn(async move { send_notifications(&st2, &text).await; });
            let st3 = st.clone();
            tokio::spawn(async move { begin_clip(&st3).await; });
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
        .route("/api/align", get(get_align).post(set_align))
        .route("/api/notify", get(get_notify).post(set_notify))
        .route("/api/version", get(version))
        .route("/api/whoami", get(whoami))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/regions.csv", get(regions_csv))
        .route("/api/history.csv", get(history_csv))
        .route("/api/history.json", get(history_json))
        .route("/api/point", get(point))
        .route("/api/regions", get(get_regions).post(add_region).delete(clear_regions))
        .route("/api/regions/{id}", delete(delete_region))
        .route("/api/regions/{id}/ref", post(set_region_ref))
        .route("/api/alarms", get(get_alarms).post(add_alarm).delete(clear_alarms))
        .route("/api/alarms/{id}", delete(delete_alarm))
        .route("/api/reference", post(set_reference).delete(clear_reference))
        .route("/api/references", get(list_references).post(add_reference).delete(clear_references))
        .route("/api/references/{id}", delete(delete_reference))
        .route("/api/references/{id}/activate", post(activate_reference))
        .route("/api/compare.json", get(compare_json))
        .route("/api/compare.jpg", get(compare_jpg))
        .route("/api/dt.jpg", get(dt_image))
        .route("/api/dt.json", get(dt_json))
        .route("/api/reanalyze.jpg", get(reanalyze_jpg))
        .route("/api/reanalyze.json", get(reanalyze_json))
        .route("/api/prerecord", get(get_prerecord).post(set_prerecord))
        .route("/api/captures", get(list_captures))
        .route("/api/captures/{name}", get(capture_file))
        .route("/api/calibration", get(get_calibration).post(set_calibration))
        .route("/api/camera/settings", get(camera_settings))
        .route("/api/camera/reboot", post(camera_reboot))
        .route("/api/discover", get(discover))
        .route("/api/snapshot", post(snapshot))
        .route("/api/snapshots", get(list_snapshots))
        .route("/api/snapshots.zip", get(snapshots_zip))
        .route("/api/captures.zip", get(captures_zip))
        .route("/api/snapshots/{name}", get(snapshot_file).delete(delete_snapshot))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, auth_mw))
}

fn public_path(path: &str) -> bool {
    matches!(
        path,
        "/" | "/api/health" | "/api/version" | "/api/whoami" | "/api/setup" | "/api/login"
    )
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .find_map(|part| part.trim().strip_prefix("thermolink_session=").map(str::to_string))
}

async fn auth_mw(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if public_path(req.uri().path()) {
        return next.run(req).await;
    }
    // Closed by default: with no account only the setup/login surface is
    // reachable. This prevents an unauthenticated visitor from claiming admin.
    if st.auth.read().await.is_none() {
        return (StatusCode::SERVICE_UNAVAILABLE, "setup required").into_response();
    }
    if let Some(token) = session_token(req.headers()) {
        let valid = st
            .sessions
            .read()
            .await
            .get(&token)
            .is_some_and(|expires| *expires > Instant::now());
        if valid {
            return next.run(req).await;
        }
    }
    (StatusCode::UNAUTHORIZED, "login required").into_response()
}

async fn whoami(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Json<serde_json::Value> {
    let auth = st.auth.read().await.clone();
    let setup_required = auth.is_none();
    let configured = auth.is_some();
    let authenticated = if setup_required {
        false
    } else {
        match session_token(&headers) {
            Some(token) => st
                .sessions
                .read()
                .await
                .get(&token)
                .is_some_and(|expires| *expires > Instant::now()),
            None => false,
        }
    };
    Json(json!({
        "authenticated": authenticated,
        "configured": configured,
        "setup_required": setup_required,
        "user": auth.map(|a| a.username),
    }))
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

async fn new_session(st: &Arc<AppState>) -> String {
    let mut bytes = [0u8; 32];
    let mut rng = OsRng;
    rng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    {
        let mut sessions = st.sessions.write().await;
        let now = Instant::now();
        sessions.retain(|_, expires| *expires > now);
        sessions.insert(token.clone(), now + Duration::from_secs(7 * 24 * 3600));
    }
    token
}

fn session_response(token: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::SET_COOKIE,
            format!("thermolink_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800"),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ok":true}"#))
        .unwrap()
}

async fn setup(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(c): Json<Credentials>,
) -> Result<Response, (StatusCode, String)> {
    if st.auth.read().await.is_some() {
        return Err((StatusCode::CONFLICT, "already set up".into()));
    }
    let local = addr.ip().is_loopback()
        || matches!(addr.ip(), IpAddr::V4(v4) if v4.is_private());
    let token_ok = headers
        .get("x-setup-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == st.setup_token);
    if !local && !token_ok {
        return Err((
            StatusCode::FORBIDDEN,
            "setup is limited to the local console; send X-Setup-Token for remote setup".into(),
        ));
    }
    let username = c.username.trim().to_string();
    if username.len() < 2 {
        return Err((StatusCode::BAD_REQUEST, "username too short".into()));
    }
    if c.password.len() < 6 {
        return Err((StatusCode::BAD_REQUEST, "password must be at least 6 characters".into()));
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(c.password.as_bytes(), &salt)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .to_string();
    *st.auth.write().await = Some(AuthFile {
        username: username.clone(),
        hash,
    });
    st.persist_auth().await;
    tracing::info!(user = %username, "admin account created");
    Ok(session_response(new_session(&st).await))
}

async fn login(
    State(st): State<Arc<AppState>>,
    Json(c): Json<Credentials>,
) -> Result<Response, (StatusCode, String)> {
    let Some(auth) = st.auth.read().await.clone() else {
        return Err((StatusCode::CONFLICT, "setup required".into()));
    };
    if auth.username != c.username.trim() {
        return Err((StatusCode::UNAUTHORIZED, "invalid credentials".into()));
    }
    let parsed = PasswordHash::new(&auth.hash)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if Argon2::default()
        .verify_password(c.password.as_bytes(), &parsed)
        .is_err()
    {
        return Err((StatusCode::UNAUTHORIZED, "invalid credentials".into()));
    }
    Ok(session_response(new_session(&st).await))
}

async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        st.sessions.write().await.remove(&token);
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::SET_COOKIE,
            "thermolink_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ok":true}"#))
        .unwrap()
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
        "video": st.video.as_ref().is_some_and(|v| v.alive()),
        "video_restarts": st.video.as_ref().map(|v| v.restarts()).unwrap_or(0),
        "video_error": st.video.as_ref().and_then(|v| v.last_error()),
        "seq": latest.map(|f| f.seq).unwrap_or(0),
    }))
}

async fn thermal_json(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let display = st.display.read().await.clone();
    let align = st.align.read().await.clone();
    let calibration = *st.calibration.read().await;
    let has_reference = st.reference.read().await.is_some();
    let Some(frame) = st.hub.latest(StreamKind::Thermal).await else {
        return Json(json!({
            "ready": false,
            "connected": st.connected(),
            "video": st.video.as_ref().is_some_and(|v| v.alive()),
            "display": display,
            "align": align,
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
        "video": st.video.as_ref().is_some_and(|v| v.alive()),
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
        "align": align,
        "calibration": calibration,
        "palettes": PALETTES,
        "has_reference": has_reference,
        "spec": st.spec,
    }))
}

async fn get_display(State(st): State<Arc<AppState>>) -> Json<DisplaySettings> {
    Json(st.display.read().await.clone())
}

#[derive(Deserialize)]
struct DisplayPatch {
    palette: Option<String>,
    clip_lo: Option<Option<f32>>,
    clip_hi: Option<Option<f32>>,
    span_lo: Option<Option<f32>>,
    span_hi: Option<Option<f32>>,
    iso_bands: Option<Vec<IsoBand>>,
}

async fn set_display(
    State(st): State<Arc<AppState>>,
    Json(patch): Json<DisplayPatch>,
) -> Result<Json<DisplaySettings>, (StatusCode, String)> {
    let mut next = st.display.read().await.clone();
    if let Some(palette) = patch.palette {
        if !PALETTES.contains(&palette.as_str()) {
            return Err((StatusCode::BAD_REQUEST, format!("unknown palette {palette:?}")));
        }
        next.palette = palette;
    }
    if let Some(v) = patch.clip_lo { next.clip_lo = v; }
    if let Some(v) = patch.clip_hi { next.clip_hi = v; }
    if let Some(v) = patch.span_lo { next.span_lo = v; }
    if let Some(v) = patch.span_hi { next.span_hi = v; }
    if let Some(v) = patch.iso_bands { next.iso_bands = v; }
    *st.display.write().await = next.clone();
    st.persist_display().await;
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
    let Some(video) = st.video.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "ffmpeg unavailable").into_response();
    };
    if !video.alive() {
        let reason = video.last_error().unwrap_or_else(|| "transcoder restarting".into());
        return (StatusCode::SERVICE_UNAVAILABLE, reason).into_response();
    }
    match video.latest() {
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
            if !video.alive() { break; }
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
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    clear: Option<bool>,
}

fn default_history_limit() -> usize {
    600
}

async fn history_json(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HistoryQuery>,
) -> Json<serde_json::Value> {
    if q.clear.unwrap_or(false) {
        st.history.clear().await;
    }
    let mut samples = st.history.tail(q.limit.min(3600)).await;
    if let Some(since) = q.since {
        samples.retain(|s| s.t >= since);
    }
    Json(json!({ "samples": samples }))
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
    let mut stats: Vec<serde_json::Value> = img
        .as_ref()
        .map(|i| regions.iter().filter_map(|r| region_stats(i, r)).collect())
        .unwrap_or_default();
    if let Some(img) = img.as_ref() {
        attach_deltas(img, &regions, &mut stats);
    }
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
    if req.pts.len() > 16 {
        return Err((StatusCode::BAD_REQUEST, "too many points".into()));
    }
    let kind = match req.kind.as_str() {
        "point" | "rect" | "poly" => req.kind.clone(),
        _ => return Err((StatusCode::BAD_REQUEST, "unknown region kind".into())),
    };
    if kind == "poly" && req.pts.len() < 3 {
        return Err((StatusCode::BAD_REQUEST, "polygon needs 3 points".into()));
    }
    if st.regions.read().await.len() >= 32 {
        return Err((StatusCode::CONFLICT, "region limit reached (32)".into()));
    }
    let region = Region {
        id: format!("r{}-{}", now_secs(), SEQ.fetch_add(1, Ordering::Relaxed)),
        name: if req.name.is_empty() {
            format!("{} {},{}", kind, req.pts[0][0], req.pts[0][1])
        } else {
            req.name
        },
        kind,
        pts: req.pts,
        is_ref: false,
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut regions = st.regions.write().await;
    let before = regions.len();
    regions.retain(|r| r.id != id);
    if regions.len() == before {
        return Err((StatusCode::NOT_FOUND, "no such region".into()));
    }
    drop(regions);
    st.persist_regions().await;
    Ok(Json(json!({ "ok": true })))
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
    if st.alarms.read().await.len() >= 16 {
        return Err((StatusCode::CONFLICT, "alarm limit reached (16)".into()));
    }
    let alarm = Alarm {
        id: format!("a{}-{}", now_secs(), SEQ.fetch_add(1, Ordering::Relaxed)),
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut alarms = st.alarms.write().await;
    let before = alarms.len();
    alarms.retain(|a| a.id != id);
    if alarms.len() == before {
        return Err((StatusCode::NOT_FOUND, "no such alarm".into()));
    }
    drop(alarms);
    st.alarm_started.lock().await.remove(&id);
    st.alarm_active.lock().await.remove(&id);
    st.persist_alarms().await;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_alarms(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    st.alarms.write().await.clear();
    st.alarm_started.lock().await.clear();
    st.alarm_active.lock().await.clear();
    st.persist_alarms().await;
    *st.last_alarm.write().await = None;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct ReferenceRequest {
    #[serde(default)]
    name: String,
}

async fn list_references(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let list = st.references.read().await.clone();
    let active = st.active_ref.read().await.clone();
    Json(json!({
        "active": active,
        "references": list.iter().map(|r| json!({
            "id": r.id,
            "name": r.name,
            "at": r.at,
            "active": active.as_deref() == Some(r.id.as_str()),
        })).collect::<Vec<_>>(),
    }))
}

async fn add_reference(
    State(st): State<Arc<AppState>>,
    Json(req): Json<ReferenceRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let vals: Vec<u16> = img.values().collect();
    let id = format!("r{}", now_secs() * 1000 + now_ms() % 1000);
    let name = if req.name.trim().is_empty() {
        format!("ref {}", now_secs())
    } else {
        req.name.trim().to_string()
    };
    let reference = Reference {
        id: id.clone(),
        name,
        at: now_secs(),
        vals: vals.clone(),
    };
    st.references.write().await.push(reference.clone());
    *st.reference.write().await = Some(vals);
    *st.active_ref.write().await = Some(id);
    st.persist_references().await;
    st.persist_active_ref().await;
    Ok(Json(json!({ "ok": true, "id": reference.id })))
}

async fn activate_reference(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let list = st.references.read().await.clone();
    let Some(reference) = list.into_iter().find(|r| r.id == id) else {
        return Err((StatusCode::NOT_FOUND, "no such reference".into()));
    };
    *st.reference.write().await = Some(reference.vals);
    *st.active_ref.write().await = Some(reference.id);
    st.persist_active_ref().await;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_reference(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Json<serde_json::Value> {
    st.references.write().await.retain(|r| r.id != id);
    if st.active_ref.read().await.as_deref() == Some(id.as_str()) {
        *st.reference.write().await = None;
        *st.active_ref.write().await = None;
    }
    st.persist_references().await;
    st.persist_active_ref().await;
    Json(json!({ "ok": true }))
}

async fn clear_references(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    st.references.write().await.clear();
    *st.reference.write().await = None;
    *st.active_ref.write().await = None;
    st.persist_references().await;
    st.persist_active_ref().await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct CompareQuery {
    name: String,
}

async fn compare_json(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CompareQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let snap = load_snapshot(&st, &q.name)?;
    let cur = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let snap_vals: Vec<u16> = snap.values().collect();
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0;
    let mut n = 0u32;
    let mut hot = (0u32, 0u32);
    let mut hot_abs = f32::MIN;
    for (i, a) in cur.values().enumerate() {
        let Some(&b) = snap_vals.get(i) else { break };
        let ta = a as f32 * cur.calibration.scale + cur.calibration.offset;
        let tb = b as f32 * snap.calibration.scale + snap.calibration.offset;
        let d = ta - tb;
        if d.abs() > hot_abs {
            hot_abs = d.abs();
            hot = ((i as u32) % cur.width, (i as u32) / cur.width);
        }
        min = min.min(d);
        max = max.max(d);
        sum += d;
        n += 1;
    }
    Ok(Json(json!({
        "name": q.name,
        "ready": n > 0,
        "min_c": round2(if n > 0 { min } else { 0.0 }),
        "max_c": round2(if n > 0 { max } else { 0.0 }),
        "avg_c": round2(if n > 0 { sum / n as f32 } else { 0.0 }),
        "n": n,
        "max_at": [hot.0, hot.1],
    })))
}

async fn compare_jpg(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CompareQuery>,
) -> Response {
    let snap = match load_snapshot(&st, &q.name) {
        Ok(s) => s,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let Some(cur) = st.thermal_image().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let vals: Vec<u16> = snap.values().collect();
    match dt_jpeg(&cur, &vals, 5.0, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn set_reference(State(st): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    let vals: Vec<u16> = img.values().collect();
    *st.reference.write().await = Some(vals);
    *st.active_ref.write().await = None;
    st.persist_active_ref().await;
    Ok(Json(json!({ "ok": true, "at": now_secs() })))
}

async fn clear_reference(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    *st.reference.write().await = None;
    *st.active_ref.write().await = None;
    st.persist_active_ref().await;
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

#[derive(Deserialize)]
struct ConfirmRequest {
    confirm: String,
}

async fn camera_reboot(
    State(st): State<Arc<AppState>>,
    Json(req): Json<ConfirmRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.confirm != "reboot" {
        return Err((
            StatusCode::BAD_REQUEST,
            "send {\"confirm\":\"reboot\"} to confirm".into(),
        ));
    }
    st.driver
        .reboot()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}

async fn discover(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let parts: Vec<&str> = st.camera_host.split('.').collect();
    if parts.len() != 4 {
        return Json(json!({ "hosts": [], "error": "camera host is not an IPv4 address" }));
    }
    let base = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    let mut tasks = Vec::new();
    for last in 1..=254u8 {
        let ip = format!("{base}.{last}");
        let sem = sem.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            match tokio::time::timeout(
                Duration::from_millis(220),
                tokio::net::TcpStream::connect((ip.as_str(), 3001)),
            )
            .await
            {
                Ok(Ok(_)) => Some(ip),
                _ => None,
            }
        }));
    }
    let mut hosts = Vec::new();
    for task in tasks {
        if let Ok(Some(ip)) = task.await {
            hosts.push(ip);
        }
    }
    hosts.sort_by_key(|ip| ip.rsplit('.').next().and_then(|n| n.parse::<u16>().ok()));
    Json(json!({ "hosts": hosts, "scanned": format!("{base}.0/24") }))
}

async fn camera_settings(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match st.driver.read_settings().await {
        Ok(Some(settings)) => Ok(Json(settings)),
        Ok(None) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "camera did not report its settings".into(),
        )),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
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

#[derive(Deserialize)]
struct NameQuery {
    name: String,
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn load_snapshot(st: &AppState, name: &str) -> Result<ThermalImage, (StatusCode, String)> {
    if !safe_name(name) {
        return Err((StatusCode::BAD_REQUEST, "bad name".into()));
    }
    let dir = st.snapshots_dir();
    let raw = std::fs::read(dir.join(format!("{name}.raw")))
        .map_err(|_| (StatusCode::NOT_FOUND, "no stored frame".into()))?;
    let meta: serde_json::Value = std::fs::read(dir.join(format!("{name}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let scale = meta["calibration"]["scale"].as_f64().unwrap_or(0.01) as f32;
    let offset = meta["calibration"]["offset"].as_f64().unwrap_or(0.0) as f32;
    let width = meta["w"].as_u64().unwrap_or(160) as u32;
    let height = meta["h"].as_u64().unwrap_or(120) as u32;
    if width == 0 || height == 0 {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "invalid stored frame size".into()));
    }
    if raw.len() != (width * height * 2) as usize {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "stored frame size mismatch".into()));
    }
    Ok(ThermalImage {
        width,
        height,
        raw: Bytes::from(raw),
        calibration: Calibration { scale, offset },
    })
}

async fn reanalyze_jpg(
    State(st): State<Arc<AppState>>,
    Query(q): Query<NameQuery>,
) -> Response {
    let img = match load_snapshot(&st, &q.name) {
        Ok(img) => img,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let display = st.display.read().await.clone();
    match render_jpeg(&img, &display, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn reanalyze_json(
    State(st): State<Arc<AppState>>,
    Query(q): Query<NameQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = load_snapshot(&st, &q.name)?;
    let stats = img.stats();
    Ok(Json(json!({
        "name": q.name,
        "stats": {
            "min_c": round2(stats.min_c),
            "max_c": round2(stats.max_c),
            "mean_c": round2(stats.mean_c),
            "hot": [stats.hot.0, stats.hot.1],
            "cold": [stats.cold.0, stats.cold.1],
            "w": stats.width, "h": stats.height
        },
        "calibration": img.calibration,
    })))
}

fn zip_directory(dir: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut paths: Vec<_> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            paths.sort();
            for path in paths {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
                let bytes = std::fs::read(&path)?;
                writer.start_file(name, options)?;
                writer.write_all(&bytes)?;
            }
        }
        writer.finish()?;
    }
    Ok(cursor.into_inner())
}

fn zip_response(result: anyhow::Result<Vec<u8>>, filename: &str) -> Response {
    match result {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/zip")
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            )
            .body(Body::from(bytes))
            .unwrap(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn snapshots_zip(State(st): State<Arc<AppState>>) -> Response {
    zip_response(zip_directory(&st.snapshots_dir()), "snapshots.zip")
}

async fn captures_zip(State(st): State<Arc<AppState>>) -> Response {
    zip_response(zip_directory(&st.captures_dir()), "captures.zip")
}

async fn delete_snapshot(
    State(st): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Json<serde_json::Value> {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        let dir = st.snapshots_dir();
        for ext in ["jpg", "raw", "json"] {
            let _ = std::fs::remove_file(dir.join(format!("{name}.{ext}")));
        }
    }
    Json(json!({ "ok": true }))
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

/// Renders a low-rate JPEG ring and turns alarm events into clips.
pub fn spawn_prerecorder(st: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let cfg = st.prerecord.read().await.clone();
            let Some(img) = st.thermal_image().await else { continue };
            let display = st.display.read().await.clone();
            let Ok(jpeg) = render_jpeg(&img, &display, 75) else { continue };
            let now = Instant::now();
            let jpeg = Bytes::from(jpeg);

            {
                let mut ring = st.prerec_ring.lock().await;
                ring.push_back(RingFrame { at: now, jpeg: jpeg.clone() });
                let keep = Duration::from_secs(cfg.before_s.max(1) as u64 + 10);
                while ring.front().is_some_and(|f| now.duration_since(f.at) > keep) {
                    ring.pop_front();
                }
                if ring.len() > 2000 {
                    ring.pop_front();
                }
            }

            let finished = {
                let mut active = st.active_clip.lock().await;
                if let Some(clip) = active.as_mut() {
                    clip.frames.push(RingFrame { at: now, jpeg });
                    if now >= clip.until {
                        active.take()
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(clip) = finished {
                if let Err(e) = write_clip(&st, clip).await {
                    tracing::warn!(error = %e, "clip write failed");
                }
            }
        }
    });
}

async fn begin_clip(st: &Arc<AppState>) {
    let cfg = st.prerecord.read().await.clone();
    if !cfg.enabled {
        return;
    }
    let now = Instant::now();
    let frames: Vec<RingFrame> = {
        let ring = st.prerec_ring.lock().await;
        ring.iter()
            .filter(|f| now.duration_since(f.at) <= Duration::from_secs(cfg.before_s as u64))
            .cloned()
            .collect()
    };
    let mut active = st.active_clip.lock().await;
    if active.is_none() {
        *active = Some(ActiveClip {
            until: now + Duration::from_secs(cfg.after_s as u64),
            frames,
        });
    }
}

async fn write_clip(st: &Arc<AppState>, clip: ActiveClip) -> anyhow::Result<()> {
    let dir = st.captures_dir();
    tokio::fs::create_dir_all(&dir).await?;
    let name = format!("{}-{:03}", now_secs(), now_ms() % 1000);
    let mut body = Vec::new();
    for frame in &clip.frames {
        body.extend_from_slice(&frame.jpeg);
    }
    tokio::fs::write(dir.join(format!("{name}.mjpg")), &body).await?;
    let meta = json!({
        "name": name,
        "at": now_secs(),
        "frames": clip.frames.len(),
        "bytes": body.len(),
        "before_s": st.prerecord.read().await.before_s,
        "after_s": st.prerecord.read().await.after_s,
    });
    tokio::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&meta)?,
    )
    .await?;
    tracing::info!(name = %name, frames = clip.frames.len(), "saved pre-record clip");
    Ok(())
}

async fn get_prerecord(State(st): State<Arc<AppState>>) -> Json<PrerecordConfig> {
    Json(st.prerecord.read().await.clone())
}

async fn set_prerecord(
    State(st): State<Arc<AppState>>,
    Json(next): Json<PrerecordConfig>,
) -> Json<PrerecordConfig> {
    let cfg = PrerecordConfig {
        enabled: next.enabled,
        before_s: next.before_s.min(60),
        after_s: next.after_s.min(60),
    };
    *st.prerecord.write().await = cfg.clone();
    st.persist_prerecord().await;
    Json(cfg)
}

async fn list_captures(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut names: Vec<String> = std::fs::read_dir(st.captures_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    name.strip_suffix(".mjpg").map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort_by(|a, b| b.cmp(a));
    Json(json!({ "captures": names }))
}

async fn capture_file(
    State(st): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    match std::fs::read(st.captures_dir().join(format!("{name}.mjpg"))) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "video/x-motion-jpeg")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => (StatusCode::NOT_FOUND, "no such capture").into_response(),
    }
}

async fn get_align(State(st): State<Arc<AppState>>) -> Json<Align> {
    Json(st.align.read().await.clone())
}

async fn set_align(
    State(st): State<Arc<AppState>>,
    Json(next): Json<Align>,
) -> Json<Align> {
    let clamped = Align {
        x: next.x.clamp(-50.0, 50.0),
        y: next.y.clamp(-50.0, 50.0),
        scale: next.scale.clamp(0.25, 4.0),
        rot: next.rot.rem_euclid(360.0),
        flip_h: next.flip_h,
        flip_v: next.flip_v,
    };
    *st.align.write().await = clamped.clone();
    st.persist_align().await;
    Json(clamped)
}

#[derive(Deserialize)]
struct NotifyRequest {
    discord: Option<String>,
    slack: Option<String>,
    telegram_token: Option<String>,
    telegram_chat: Option<String>,
    webhook: Option<String>,
}

fn notify_status(n: &NotifyConfig) -> serde_json::Value {
    json!({
        "discord": n.discord.is_some(),
        "slack": n.slack.is_some(),
        "telegram": n.telegram_token.is_some() && n.telegram_chat.is_some(),
        "webhook": n.webhook.is_some(),
    })
}

async fn get_notify(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let n = st.notify.read().await;
    Json(notify_status(&n))
}

async fn set_notify(
    State(st): State<Arc<AppState>>,
    Json(req): Json<NotifyRequest>,
) -> Json<serde_json::Value> {
    let mut n = st.notify.write().await;
    let put = |slot: &mut Option<String>, value: Option<String>| {
        if let Some(v) = value {
            *slot = if v.trim().is_empty() { None } else { Some(v.trim().to_string()) };
        }
    };
    put(&mut n.discord, req.discord);
    put(&mut n.slack, req.slack);
    put(&mut n.telegram_token, req.telegram_token);
    put(&mut n.telegram_chat, req.telegram_chat);
    put(&mut n.webhook, req.webhook);
    let status = notify_status(&n);
    drop(n);
    st.persist_notify().await;
    Json(status)
}

async fn send_notifications(st: &Arc<AppState>, text: &str) {
    let cfg = st.notify.read().await.clone();
    if cfg.discord.is_none()
        && cfg.slack.is_none()
        && cfg.webhook.is_none()
        && (cfg.telegram_token.is_none() || cfg.telegram_chat.is_none())
    {
        return;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap_or_default();

    if let Some(url) = cfg.discord.as_deref() {
        if let Err(e) = client.post(url).json(&json!({ "content": text })).send().await {
            tracing::warn!(error = %e, "discord notification failed");
        }
    }
    if let Some(url) = cfg.slack.as_deref() {
        if let Err(e) = client.post(url).json(&json!({ "text": text })).send().await {
            tracing::warn!(error = %e, "slack notification failed");
        }
    }
    if let (Some(token), Some(chat)) = (cfg.telegram_token.as_deref(), cfg.telegram_chat.as_deref()) {
        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        if let Err(e) = client
            .post(url)
            .json(&json!({ "chat_id": chat, "text": text }))
            .send()
            .await
        {
            tracing::warn!(error = %e, "telegram notification failed");
        }
    }
    if let Some(url) = cfg.webhook.as_deref() {
        if let Err(e) = client.post(url).json(&json!({ "text": text })).send().await {
            tracing::warn!(error = %e, "webhook notification failed");
        }
    }
}

async fn version() -> Json<serde_json::Value> {
    Json(json!({ "name": "thermolink", "version": env!("CARGO_PKG_VERSION") }))
}

async fn set_region_ref(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Json<serde_json::Value> {
    {
        let mut regions = st.regions.write().await;
        for r in regions.iter_mut() {
            r.is_ref = r.id == id;
        }
    }
    st.persist_regions().await;
    Json(json!({ "ok": true }))
}

async fn regions_csv(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CsvClear>,
) -> Response {
    let log = st.data_dir.join("regions.csv");
    if q.clear.unwrap_or(false) {
        let _ = std::fs::remove_file(&log);
    }
    if let Ok(body) = std::fs::read_to_string(&log) {
        return text_response(body, "text/csv; charset=utf-8");
    }
    let regions = st.regions.read().await.clone();
    let img = st.thermal_image().await;
    let mut stats: Vec<serde_json::Value> = img
        .as_ref()
        .map(|i| regions.iter().filter_map(|r| region_stats(i, r)).collect())
        .unwrap_or_default();
    if let Some(img) = img.as_ref() {
        attach_deltas(img, &regions, &mut stats);
    }
    let mut out = String::from("id,name,kind,n,min_c,avg_c,max_c,delta_c\n");
    for s in &stats {
        out.push_str(&format!(
            "{},{},{},{},{:.2},{:.2},{:.2},{}\n",
            s["id"].as_str().unwrap_or(""),
            s["name"].as_str().unwrap_or("").replace(',', " "),
            s["kind"].as_str().unwrap_or(""),
            s["n"].as_u64().unwrap_or(0),
            s["min_c"].as_f64().unwrap_or(0.0),
            s["avg_c"].as_f64().unwrap_or(0.0),
            s["max_c"].as_f64().unwrap_or(0.0),
            s["delta_c"].as_f64().map(|v| format!("{v:.2}")).unwrap_or_default(),
        ));
    }
    text_response(out, "text/csv; charset=utf-8")
}

async fn history_csv(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    if q.clear.unwrap_or(false) {
        st.history.clear().await;
    }
    let mut samples = st.history.tail(q.limit.min(3600)).await;
    if let Some(since) = q.since {
        samples.retain(|s| s.t >= since);
    }
    let mut out = String::from("t,min_c,max_c,mean_c\n");
    for s in samples {
        out.push_str(&format!("{},{:.2},{:.2},{:.2}\n", s.t, s.min_c, s.max_c, s.mean_c));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=\"temperature.csv\"")
        .body(Body::from(out))
        .unwrap()
}

async fn append_region_log(st: &Arc<AppState>, img: &ThermalImage) {
    use std::io::Write;
    let regions = st.regions.read().await.clone();
    if regions.is_empty() {
        return;
    }
    let mut stats: Vec<serde_json::Value> = regions.iter().filter_map(|r| region_stats(img, r)).collect();
    attach_deltas(img, &regions, &mut stats);
    let path = st.data_dir.join("regions.csv");
    let fresh = !path.exists();
    let mut out = String::new();
    if fresh {
        out.push_str("t,id,name,kind,n,min_c,avg_c,max_c,delta_c\n");
    }
    let t = now_secs();
    for s in &stats {
        out.push_str(&format!(
            "{t},{},{},{},{},{:.2},{:.2},{:.2},{}\n",
            s["id"].as_str().unwrap_or(""),
            s["name"].as_str().unwrap_or("").replace(',', " "),
            s["kind"].as_str().unwrap_or(""),
            s["n"].as_u64().unwrap_or(0),
            s["min_c"].as_f64().unwrap_or(0.0),
            s["avg_c"].as_f64().unwrap_or(0.0),
            s["max_c"].as_f64().unwrap_or(0.0),
            s["delta_c"].as_f64().map(|v| format!("{v:.2}")).unwrap_or_default(),
        ));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(out.as_bytes());
    }
    if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, st.data_dir.join("regions.csv.1"));
    }
}

#[derive(Deserialize)]
struct CsvClear {
    #[serde(default)]
    clear: Option<bool>,
}

fn attach_deltas(img: &ThermalImage, regions: &[Region], stats: &mut [serde_json::Value]) {
    let base = regions
        .iter()
        .find(|r| r.is_ref)
        .and_then(|r| region_stats(img, r))
        .and_then(|s| s["avg_c"].as_f64());
    if let Some(base) = base {
        for s in stats.iter_mut() {
            if let Some(avg) = s["avg_c"].as_f64() {
                s["delta_c"] = json!(round2((avg - base) as f32));
            }
        }
    }
}

fn text_response(body: String, content_type: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .unwrap()
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
                    && (x as f32)
                        < (xj - xi) as f32 * (y - yi) as f32 / (yj - yi) as f32
                            + xi as f32
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
