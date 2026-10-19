//! HTTP surface. Deliberately imports `camera` and `render`, never a driver.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::body::Body;
use axum::extract::{ConnectInfo, Extension, Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::camera::{Calibration, CameraDriver, CameraSpec, FramePayload, StreamKind, ThermalImage};
use crate::render::{
    builtin_iso_profiles, dt_jpeg, render_jpeg, validate_iso_bands, DisplaySettings, IsoBand,
    IsothermState, PALETTES, ISO_PROFILE_LIMIT, ISO_PROFILE_NAME_LIMIT,
};
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
    /// Calibration at capture time. Older files omit it; callers then fall
    /// back to the live calibration.
    #[serde(default)]
    pub calibration: Option<Calibration>,
}

/// A restricted second account. It may only read and, when explicitly
/// granted, use a small set of write capabilities; the camera, updater,
/// account and notification credentials always stay with the admin.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GuestAccount {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub hash: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthFile {
    pub username: String,
    pub hash: String,
    /// Omitted by auth.json written before guest support; defaults closed.
    #[serde(default)]
    pub guest: GuestAccount,
    /// Omitted by older files; missing entries fall back to the defaults.
    #[serde(default)]
    pub guest_perms: BTreeMap<String, bool>,
}

/// Capabilities a guest may be granted. Anything outside this list is
/// admin-only, no matter what a stored file says.
pub const GUEST_CAPS: [&str; 7] =
    ["measure", "snapshot", "display", "reference", "calibration", "alarms", "prerecord"];

fn default_guest_perms() -> BTreeMap<String, bool> {
    BTreeMap::from([
        ("measure".to_string(), true),
        ("snapshot".to_string(), true),
        ("display".to_string(), true),
        ("reference".to_string(), true),
        ("calibration".to_string(), false),
        ("alarms".to_string(), false),
        ("prerecord".to_string(), false),
    ])
}

impl AuthFile {
    /// Defaults merged with the stored values, restricted to known caps.
    fn perms(&self) -> BTreeMap<String, bool> {
        let mut out = default_guest_perms();
        for (key, value) in &self.guest_perms {
            if GUEST_CAPS.contains(&key.as_str()) {
                out.insert(key.clone(), *value);
            }
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Guest,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Guest => "guest",
        }
    }
}

#[derive(Clone)]
struct Session {
    expires: Instant,
    role: Role,
    user: String,
}

/// What the auth middleware learned about the caller. Handlers that need a
/// role decision read it from the request extensions.
#[derive(Clone)]
struct AuthContext {
    role: Role,
    user: String,
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
    pub isotherm: RwLock<IsothermState>,
    pub align: RwLock<Align>,
    pub notify: RwLock<NotifyConfig>,
    pub auth: RwLock<Option<AuthFile>>,
    sessions: RwLock<HashMap<String, Session>>,
    setup_token: String,
    pub calibration: RwLock<Calibration>,
    pub connected: Arc<AtomicBool>,
    pub source: String,
    started: Instant,
    pub video: Option<VideoTranscoder>,
    thermal_cache: Mutex<Option<(u64, String, Bytes)>>,
    events: Mutex<VecDeque<serde_json::Value>>,
    pub driver: Arc<dyn CameraDriver>,
    pub camera_host: String,
    pub data_dir: PathBuf,
    /// Path of the running executable, captured before an update can
    /// replace it: `/proc/self/exe` gains a `(deleted)` suffix after the
    /// rename and would no longer be executable.
    pub exe: PathBuf,
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
        let mut display = std::fs::read_to_string(data_dir.join("display.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<DisplaySettings>(&t).ok())
            .unwrap_or_default();
        let mut isotherm = std::fs::read_to_string(data_dir.join("isotherm.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<IsothermState>(&t).ok())
            .unwrap_or_default();
        // Built-in profiles are always available; a saved profile with the
        // same name deliberately overrides one for this installation.
        for (name, bands) in builtin_iso_profiles() {
            isotherm.profiles.entry(name).or_insert(bands);
        }
        if !isotherm.profiles.contains_key(&isotherm.current) {
            isotherm.current = "pcba 25-60".into();
        }
        if let Some(bands) = isotherm.profiles.get(&isotherm.current) {
            display.iso_bands = bands.clone();
        }
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
        let mut session_map: HashMap<String, Session> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(data_dir.join("sessions.json")) {
            if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(&text) {
                let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                for (token, val) in obj {
                    let exp = val["exp"].as_u64().unwrap_or(0);
                    if exp <= now_unix { continue; }
                    let role = match val["role"].as_str() { Some("guest") => Role::Guest, _ => Role::Admin };
                    let user = val["user"].as_str().unwrap_or("admin").to_string();
                    session_map.insert(token, Session {
                        expires: Instant::now() + Duration::from_secs(exp - now_unix),
                        role,
                        user,
                    });
                }
            }
        }
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
            isotherm: RwLock::new(isotherm),
            align: RwLock::new(align),
            notify: RwLock::new(notify),
            auth: RwLock::new(auth),
            sessions: RwLock::new(session_map),
            setup_token,
            calibration: RwLock::new(calibration),
            connected,
            source,
            started: Instant::now(),
            video,
            thermal_cache: Mutex::new(None),
            events: Mutex::new(VecDeque::new()),
            driver,
            camera_host,
            data_dir,
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thermolink")),
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

    async fn persist_regions(&self) -> io::Result<()> {
        let list = self.regions.read().await.clone();
        write_atomic(&self.data_dir, "regions.json", &json_bytes(&list)?).await
    }

    async fn persist_display(&self) -> io::Result<()> {
        let d = self.display.read().await.clone();
        write_atomic(&self.data_dir, "display.json", &json_bytes(&d)?).await
    }

    async fn persist_isotherm(&self) -> io::Result<()> {
        let d = self.isotherm.read().await.clone();
        write_atomic(&self.data_dir, "isotherm.json", &json_bytes(&d)?).await
    }

    async fn persist_align(&self) -> io::Result<()> {
        let a = self.align.read().await.clone();
        write_atomic(&self.data_dir, "align.json", &json_bytes(&a)?).await
    }

    async fn persist_notify(&self) -> io::Result<()> {
        let n = self.notify.read().await.clone();
        write_atomic(&self.data_dir, "notifications.json", &json_bytes(&n)?).await
    }

    async fn persist_references(&self) -> io::Result<()> {
        let list = self.references.read().await.clone();
        write_atomic(&self.data_dir, "references.json", &json_bytes(&list)?).await
    }

    async fn persist_sessions(&self) {
        let now = Instant::now();
        let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut out = serde_json::Map::new();
        let sessions = self.sessions.read().await;
        for (token, s) in sessions.iter() {
            if s.expires <= now { continue; }
            let exp = now_unix + s.expires.saturating_duration_since(now).as_secs();
            out.insert(token.clone(), json!({
                "exp": exp,
                "role": s.role.as_str(),
                "user": s.user,
            }));
        }
        let _ = write_atomic(&self.data_dir, "sessions.json", &serde_json::to_vec(&out).unwrap_or_default()).await;
    }

    async fn persist_active_ref(&self) -> io::Result<()> {
        let id = self.active_ref.read().await.clone();
        match id {
            Some(id) => {
                write_atomic(
                    &self.data_dir,
                    "reference-active.json",
                    &serde_json::to_vec(&json!({ "id": id })).map_err(io::Error::other)?,
                )
                .await
            }
            None => match tokio::fs::remove_file(self.data_dir.join("reference-active.json")).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            },
        }
    }

    async fn persist_prerecord(&self) -> io::Result<()> {
        let c = self.prerecord.read().await.clone();
        write_atomic(&self.data_dir, "prerecord.json", &json_bytes(&c)?).await
    }

    fn captures_dir(&self) -> PathBuf {
        self.data_dir.join("captures")
    }

    async fn persist_auth(&self) -> io::Result<()> {
        let auth = self.auth.read().await.clone();
        if let Some(auth) = auth {
            write_atomic(&self.data_dir, "auth.json", &json_bytes(&auth)?).await
        } else {
            Ok(())
        }
    }

    async fn persist_alarms(&self) -> io::Result<()> {
        let list = self.alarms.read().await.clone();
        write_atomic(&self.data_dir, "alarms.json", &json_bytes(&list)?).await
    }
}

/// Write `bytes` to `dir/name` atomically: a temporary file in the same
/// directory is renamed over the destination, so a crash or a full disk
/// leaves either the old file or the new one, never a truncated file.
async fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let tmp = dir.join(format!(".{name}.{}.tmp", SEQ.fetch_add(1, Ordering::Relaxed)));
    if let Err(e) = tokio::fs::write(&tmp, bytes).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(&tmp, dir.join(name)).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}

fn json_bytes<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(io::Error::other)
}

/// Turn a failed persistence write into an honest 500 so the caller does not
/// report "saved" for state that will disappear on restart.
fn persist_error(e: io::Error) -> (StatusCode, String) {
    tracing::error!(error = %e, "failed to persist state");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("state was applied but could not be saved: {e}"),
    )
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
        let above = alarm.kind != "below";
        let fire = if above {
            stats.max_c >= alarm.threshold_c
        } else {
            stats.min_c <= alarm.threshold_c
        };
        let clear = if above {
            stats.max_c < alarm.threshold_c - 0.5
        } else {
            stats.min_c > alarm.threshold_c + 0.5
        };
        if clear {
            started.remove(&alarm.id);
            active.remove(&alarm.id);
            continue;
        }
        if active.contains(&alarm.id) {
            continue;
        }
        if !fire {
            started.remove(&alarm.id);
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
            {
                let mut events = st.events.lock().await;
                events.push_back(event.clone());
                while events.len() > 50 {
                    events.pop_front();
                }
            }
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
        .route("/i18n/{file}", get(i18n_file))
        .route("/api/spec", get(spec))
        .route("/api/health", get(health))
        .route("/api/thermal.json", get(thermal_json))
        .route("/api/thermal.jpg", get(thermal_jpg))
        .route("/api/thermal.mjpg", get(thermal_mjpg))
        .route("/api/video.jpg", get(video_jpg))
        .route("/api/video.mjpg", get(video_mjpg))
        .route("/api/raw.h264", get(raw_h264))
        .route("/api/display", get(get_display).post(set_display))
        .route("/api/isotherm", get(get_isotherm).post(set_isotherm))
        .route("/api/align", get(get_align).post(set_align))
        .route("/api/notify", get(get_notify).post(set_notify))
        .route("/api/version", get(version))
        .route("/api/update", get(update_check).post(update_apply))
        .route("/api/restart", post(restart_relay))
        .route("/api/whoami", get(whoami))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/account", get(get_account).post(update_account))
        .route("/api/regions.csv", get(regions_csv))
        .route("/api/history.csv", get(history_csv))
        .route("/api/history.json", get(history_json))
        .route("/api/point", get(point))
        .route("/api/regions", get(get_regions).post(add_region).delete(clear_regions))
        .route("/api/regions/{id}", delete(delete_region))
        .route("/api/regions/{id}/ref", post(set_region_ref))
        .route("/api/alarms", get(get_alarms).post(add_alarm).delete(clear_alarms))
        .route("/api/events", get(list_events))
        .route("/api/alarms/{id}", delete(delete_alarm))
        .route("/api/reference", post(set_reference).delete(clear_reference))
        .route("/api/references", get(list_references).post(add_reference).delete(clear_references))
        .route("/api/references/{id}", delete(delete_reference))
        .route("/api/references/{id}/activate", post(activate_reference))
        .route("/api/compare.json", get(compare_json))
        .route("/api/compare.jpg", get(compare_jpg))
        .route("/api/dt.jpg", get(dt_image))
        .route("/api/dt.mjpg", get(dt_mjpg))
        .route("/api/dt.json", get(dt_json))
        .route("/api/reanalyze.jpg", get(reanalyze_jpg))
        .route("/api/reanalyze.json", get(reanalyze_json))
        .route("/api/prerecord", get(get_prerecord).post(set_prerecord))
        .route("/api/captures", get(list_captures))
        .route("/api/captures/{name}", get(capture_file))
        .route("/api/calibration", get(get_calibration).post(set_calibration))
        .route(
            "/api/camera/settings",
            get(camera_settings).post(camera_settings_write),
        )
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
    path.starts_with("/i18n/")
        || matches!(
            path,
            "/" | "/api/health" | "/api/version" | "/api/whoami" | "/api/setup" | "/api/login"
        )
}

fn query_token(uri: &axum::http::Uri) -> Option<String> {
    uri.query()?.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == "token").then(|| value.to_string())
    })
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return Some(token.trim().to_string());
        }
    }
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .find_map(|part| part.trim().strip_prefix("thermolink_session=").map(str::to_string))
}

/// What a request needs before it reaches a handler. Reads are open to any
/// signed-in account; `Admin` covers the camera, updater, account and
/// notification credentials, which are never delegated.
#[derive(Debug, PartialEq, Eq)]
enum Guard {
    Read,
    Cap(&'static str),
    Admin,
}

fn guard_for(method: &Method, path: &str, query: Option<&str>) -> Guard {
    if path == "/api/notify"
        || path == "/api/account"
        || path == "/api/update"
        || path == "/api/restart"
    {
        return Guard::Admin;
    }
    let clear = query.is_some_and(|q| q.split('&').any(|p| p == "clear=1"));
    let write = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if !write && !(clear && matches!(path, "/api/history.json" | "/api/regions.csv")) {
        return Guard::Read;
    }
    if path.starts_with("/api/regions") {
        Guard::Cap("measure")
    } else if path.starts_with("/api/alarms") {
        Guard::Cap("alarms")
    } else if path.starts_with("/api/reference") {
        Guard::Cap("reference")
    } else if path == "/api/snapshot" || path.starts_with("/api/snapshots/") {
        Guard::Cap("snapshot")
    } else if path == "/api/display" || path == "/api/isotherm" {
        Guard::Cap("display")
    } else if path == "/api/calibration" {
        Guard::Cap("calibration")
    } else if path == "/api/prerecord" {
        Guard::Cap("prerecord")
    } else if path == "/api/history.json" || path == "/api/regions.csv" {
        Guard::Cap("measure")
    } else {
        Guard::Admin
    }
}

fn verify_hash(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn hash_password(password: &str) -> Result<String, (StatusCode, String)> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .to_string())
}

async fn auth_mw(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if public_path(req.uri().path()) {
        return next.run(req).await;
    }
    // Closed by default: with no account only the setup/login surface is
    // reachable. This prevents an unauthenticated visitor from claiming admin.
    let Some(auth) = st.auth.read().await.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "setup required").into_response();
    };
    let Some(token) = session_token(req.headers()).or_else(|| query_token(req.uri())) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let session = st
        .sessions
        .read()
        .await
        .get(&token)
        .filter(|s| s.expires > Instant::now())
        .cloned();
    let Some(session) = session else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    match guard_for(req.method(), req.uri().path(), req.uri().query()) {
        Guard::Admin if session.role != Role::Admin => {
            return (StatusCode::FORBIDDEN, "admin only").into_response();
        }
        Guard::Cap(cap) if session.role != Role::Admin => {
            if !auth.guest.enabled || !auth.perms().get(cap).copied().unwrap_or(false) {
                return (StatusCode::FORBIDDEN, "not permitted for this account").into_response();
            }
        }
        _ => {}
    }
    let mut req = req;
    req.extensions_mut().insert(AuthContext {
        role: session.role,
        user: session.user,
    });
    next.run(req).await
}

async fn whoami(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Json<serde_json::Value> {
    let auth = st.auth.read().await.clone();
    let setup_required = auth.is_none();
    let session = match session_token(&headers) {
        Some(token) => st
            .sessions
            .read()
            .await
            .get(&token)
            .filter(|s| s.expires > Instant::now())
            .cloned(),
        None => None,
    };
    Json(json!({
        "authenticated": session.is_some(),
        "configured": auth.is_some(),
        "setup_required": setup_required,
        "user": session.as_ref().map(|s| s.user.clone()),
        "role": session.as_ref().map(|s| s.role.as_str()),
        "guest": auth.as_ref().is_some_and(|a| a.guest.enabled),
        "perms": auth.as_ref().map(AuthFile::perms).unwrap_or_default(),
    }))
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

async fn new_session(st: &Arc<AppState>, role: Role, user: &str) -> String {
    let mut bytes = [0u8; 32];
    let mut rng = OsRng;
    rng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    {
        let mut sessions = st.sessions.write().await;
        let now = Instant::now();
        sessions.retain(|_, s| s.expires > now);
        sessions.insert(
            token.clone(),
            Session {
                expires: now + Duration::from_secs(7 * 24 * 3600),
                role,
                user: user.to_string(),
            },
        );
    }
    st.persist_sessions().await;
    token
}

/// True when the request reached us over TLS, including a reverse proxy that
/// terminates TLS and advertises it via `X-Forwarded-Proto`.
fn is_secure_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("https"))
}

fn session_cookie(token: &str, _secure: bool) -> String {
    // Do not set Secure while the HTTPS listener uses a self-signed cert:
    // Chrome treats that origin as non-secure and drops the cookie, which
    // makes a successful login look like a 401 loop. The transport is still
    // TLS; this only relaxes the cookie's scheme restriction.
    format!("thermolink_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800")
}

fn session_response(token: String, secure: bool) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::SET_COOKIE, session_cookie(&token, secure))
        .header(header::CONTENT_TYPE, "application/json");
    if secure {
        // A proxied TLS deployment should keep browsers on HTTPS.
        builder = builder.header("strict-transport-security", "max-age=31536000");
    }
    let body = serde_json::to_vec(&json!({ "ok": true, "token": token })).unwrap_or_default();
    builder.body(Body::from(body)).unwrap()
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
    let hash = hash_password(&c.password)?;
    *st.auth.write().await = Some(AuthFile {
        username: username.clone(),
        hash,
        guest: GuestAccount::default(),
        guest_perms: BTreeMap::new(),
    });
    if let Err(e) = st.persist_auth().await {
        *st.auth.write().await = None;
        return Err(persist_error(e));
    }
    tracing::info!(user = %username, "admin account created");
    let secure = is_secure_request(&headers);
    Ok(session_response(new_session(&st, Role::Admin, &username).await, secure))
}

async fn login(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(c): Json<Credentials>,
) -> Result<Response, (StatusCode, String)> {
    let Some(auth) = st.auth.read().await.clone() else {
        return Err((StatusCode::CONFLICT, "setup required".into()));
    };
    let who = c.username.trim();
    let (role, user) = if who == auth.username && verify_hash(&c.password, &auth.hash) {
        (Role::Admin, auth.username.clone())
    } else if who == "guest"
        && auth.guest.enabled
        && !auth.guest.hash.is_empty()
        && verify_hash(&c.password, &auth.guest.hash)
    {
        (Role::Guest, "guest".to_string())
    } else {
        return Err((StatusCode::UNAUTHORIZED, "invalid credentials".into()));
    };
    let secure = is_secure_request(&headers);
    Ok(session_response(new_session(&st, role, &user).await, secure))
}

async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        st.sessions.write().await.remove(&token);
        st.persist_sessions().await;
    }
    let cookie = "thermolink_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0";
    Response::builder()
        .status(StatusCode::OK)
        .header(header::SET_COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ok":true}"#))
        .unwrap()
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum AccountAction {
    Username { username: String },
    Password { current: String, new: String },
    Guest {
        enabled: bool,
        #[serde(default)]
        password: String,
    },
    Perms {
        perms: BTreeMap<String, bool>,
    },
}

async fn admin_account(
    st: &Arc<AppState>,
    ctx: &AuthContext,
) -> Result<AuthFile, (StatusCode, String)> {
    if ctx.role != Role::Admin {
        return Err((StatusCode::FORBIDDEN, "admin only".into()));
    }
    st.auth
        .read()
        .await
        .clone()
        .ok_or((StatusCode::CONFLICT, "setup required".into()))
}

async fn get_account(
    State(st): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let auth = admin_account(&st, &ctx).await?;
    Ok(Json(json!({
        "ok": true,
        "username": auth.username,
        "user": ctx.user,
        "role": ctx.role.as_str(),
        "guest_enabled": auth.guest.enabled,
        "guest_perms": auth.perms(),
        "caps": GUEST_CAPS,
    })))
}

async fn update_account(
    State(st): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Json(action): Json<AccountAction>,
) -> Result<Response, (StatusCode, String)> {
    let before = admin_account(&st, &ctx).await?;
    let mut auth = before.clone();
    let mut revoke_all = false;
    let mut revoke_guests = false;

    match action {
        AccountAction::Username { username } => {
            let username = username.trim().to_string();
            if username.len() < 2 || username.len() > 40 {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "username must be 2-40 characters".into(),
                ));
            }
            auth.username = username;
        }
        AccountAction::Password { current, new } => {
            if !verify_hash(&current, &auth.hash) {
                return Err((StatusCode::UNAUTHORIZED, "current password is not right".into()));
            }
            if new.len() < 8 {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "password must be at least 8 characters".into(),
                ));
            }
            auth.hash = hash_password(&new)?;
            revoke_all = true;
        }
        AccountAction::Guest { enabled, password } => {
            if enabled {
                if !password.is_empty() {
                    if password.len() < 8 {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "guest password must be at least 8 characters".into(),
                        ));
                    }
                    auth.guest.hash = hash_password(&password)?;
                } else if auth.guest.hash.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "set a guest password to enable the account".into(),
                    ));
                }
            } else {
                // Removing the account also forgets the password, matching
                // the Python relay: re-enabling requires a fresh one.
                auth.guest.hash.clear();
                revoke_guests = true;
            }
            auth.guest.enabled = enabled;
        }
        AccountAction::Perms { perms } => {
            let mut merged = auth.perms();
            for (key, value) in perms {
                if GUEST_CAPS.contains(&key.as_str()) {
                    merged.insert(key, value);
                }
            }
            auth.guest_perms = merged;
        }
    }

    *st.auth.write().await = Some(auth.clone());
    if let Err(e) = st.persist_auth().await {
        *st.auth.write().await = Some(before);
        return Err(persist_error(e));
    }
    if revoke_all {
        st.sessions.write().await.clear();
        let secure = is_secure_request(&headers);
        return Ok(session_response(
            new_session(&st, Role::Admin, &auth.username).await,
            secure,
        ));
    }
    if revoke_guests {
        st.sessions
            .write()
            .await
            .retain(|_, s| s.role == Role::Admin);
    }
    Ok((
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "username": auth.username,
            "guest_enabled": auth.guest.enabled,
            "guest_perms": auth.perms(),
        })),
    )
        .into_response())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../web/index.html"))
}

/// Translations are embedded in the binary so a deployment is still a single file.
/// `/i18n/<lang>.json` is public: the sign-in dialog needs it before a session exists.
const I18N_LANGS: &[(&str, &str)] = &[
    ("en", include_str!("../../web/i18n/en.json")),
    ("es", include_str!("../../web/i18n/es.json")),
    ("fr", include_str!("../../web/i18n/fr.json")),
    ("de", include_str!("../../web/i18n/de.json")),
    ("pt", include_str!("../../web/i18n/pt.json")),
    ("zh", include_str!("../../web/i18n/zh.json")),
    ("ja", include_str!("../../web/i18n/ja.json")),
    ("ru", include_str!("../../web/i18n/ru.json")),
    ("ar", include_str!("../../web/i18n/ar.json")),
];

async fn i18n_file(AxumPath(file): AxumPath<String>) -> Response {
    let name = file.strip_suffix(".json").unwrap_or(&file);
    match I18N_LANGS.iter().find(|(lang, _)| *lang == name) {
        Some((_, body)) => (
            [
                (header::CONTENT_TYPE, "application/json; charset=utf-8"),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            *body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "unknown language").into_response(),
    }
}

async fn spec(State(st): State<Arc<AppState>>) -> Json<CameraSpec> {
    // The live calibration can change at runtime; a static copy of the boot
    // spec would keep advertising the old value to API clients.
    let mut spec = st.spec.clone();
    let calibration = *st.calibration.read().await;
    if let Some(thermal) = spec.thermal.as_mut() {
        thermal.calibration = calibration;
    }
    Json(spec)
}

async fn health(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let latest = st.hub.latest(StreamKind::Thermal).await;
    Json(json!({
        "ok": true,
        "source": st.source,
        "uptime_s": st.started.elapsed().as_secs(),
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
    dt_span: Option<f32>,
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
    if let Some(v) = patch.dt_span {
        if !(0.5..=50.0).contains(&v) {
            return Err((StatusCode::BAD_REQUEST, "dt_span must be between 0.5 and 50 C".into()));
        }
        next.dt_span = v;
    }
    if let Some(v) = patch.iso_bands { next.iso_bands = v; }
    *st.display.write().await = next.clone();
    st.persist_display().await.map_err(persist_error)?;
    Ok(Json(next))
}

async fn get_isotherm(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let state = st.isotherm.read().await.clone();
    let bands = st.display.read().await.iso_bands.clone();
    Json(isotherm_json(&state, bands))
}

fn isotherm_json(state: &IsothermState, bands: Vec<IsoBand>) -> serde_json::Value {
    json!({
        "current": state.current,
        "profiles": state.profiles,
        "bands": bands,
        "builtins": builtin_iso_profiles().keys().cloned().collect::<Vec<_>>(),
    })
}

#[derive(Deserialize)]
struct IsothermRequest {
    apply: Option<String>,
    save: Option<String>,
    delete: Option<String>,
    bands: Option<Vec<IsoBand>>,
}

async fn set_isotherm(
    State(st): State<Arc<AppState>>,
    Json(req): Json<IsothermRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut state = st.isotherm.read().await.clone();
    let mut display = st.display.read().await.clone();

    if let Some(name) = req.apply {
        let Some(bands) = state.profiles.get(&name).cloned() else {
            return Err((StatusCode::NOT_FOUND, format!("unknown isotherm profile {name:?}")));
        };
        state.current = name;
        display.iso_bands = bands;
    } else if let Some(name) = req.save {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > ISO_PROFILE_NAME_LIMIT {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("profile name must be 1..={ISO_PROFILE_NAME_LIMIT} characters"),
            ));
        }
        if name.chars().any(char::is_control) {
            return Err((
                StatusCode::BAD_REQUEST,
                "profile name contains control characters".into(),
            ));
        }
        if !state.profiles.contains_key(&name) && state.profiles.len() >= ISO_PROFILE_LIMIT {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("at most {ISO_PROFILE_LIMIT} profiles are allowed"),
            ));
        }
        let bands = req.bands.clone().unwrap_or_else(|| display.iso_bands.clone());
        validate_iso_bands(&bands).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        state.profiles.insert(name.clone(), bands.clone());
        state.current = name;
        display.iso_bands = bands;
    } else if let Some(name) = req.delete {
        let builtins = builtin_iso_profiles();
        if !state.profiles.contains_key(&name) {
            return Err((StatusCode::NOT_FOUND, format!("unknown isotherm profile {name:?}")));
        }
        if builtins.contains_key(&name) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("built-in profile {name:?} cannot be deleted"),
            ));
        }
        state.profiles.remove(&name);
        if state.current == name {
            let fallback = builtins
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "pcba 25-60".into());
            if let Some(bands) = state.profiles.get(&fallback) {
                display.iso_bands = bands.clone();
            }
            state.current = fallback;
        }
    } else if let Some(bands) = req.bands {
        // Bands alone update the live table without rewriting a named profile.
        validate_iso_bands(&bands).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        display.iso_bands = bands;
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "isotherm needs apply, save, delete or bands".into(),
        ));
    }

    *st.isotherm.write().await = state.clone();
    *st.display.write().await = display.clone();
    st.persist_isotherm().await.map_err(persist_error)?;
    st.persist_display().await.map_err(persist_error)?;
    Ok(Json(isotherm_json(&state, display.iso_bands)))
}

async fn thermal_cached(st: &Arc<AppState>) -> Result<Bytes, (StatusCode, String)> {
    let Some(frame) = st.hub.latest(StreamKind::Thermal).await else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()));
    };
    let Some(img) = frame.thermal() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "no thermal image".into()));
    };
    let calibration = *st.calibration.read().await;
    let img = ThermalImage { calibration, ..img.clone() };
    let display = st.display.read().await.clone();
    let sig = format!("{}:{calibration:?}:{}", frame.seq, serde_json::to_string(&display).unwrap_or_default());
    {
        let cache = st.thermal_cache.lock().await;
        if let Some((seq, cached_sig, jpeg)) = cache.as_ref() {
            if *seq == frame.seq && cached_sig == &sig {
                return Ok(jpeg.clone());
            }
        }
    }
    let jpeg = render_jpeg(&img, &display, 85)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let bytes = Bytes::from(jpeg);
    *st.thermal_cache.lock().await = Some((frame.seq, sig, bytes.clone()));
    Ok(bytes)
}

async fn thermal_jpg(State(st): State<Arc<AppState>>) -> Response {
    match thermal_cached(&st).await {
        Ok(jpeg) => jpeg_response(jpeg.to_vec()),
        Err((code, msg)) => (code, msg).into_response(),
    }
}

async fn thermal_mjpg(State(st): State<Arc<AppState>>) -> Response {
    let stream = async_stream::stream! {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Ok(jpeg) = thermal_cached(&st).await else { continue };
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
    st.persist_regions().await.map_err(persist_error)?;
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
    st.persist_regions().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_regions(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    st.regions.write().await.clear();
    st.persist_regions().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_events(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let events: Vec<_> = st.events.lock().await.iter().cloned().collect();
    Json(json!({ "events": events }))
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
    st.persist_alarms().await.map_err(persist_error)?;
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
    st.persist_alarms().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_alarms(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    st.alarms.write().await.clear();
    st.alarm_started.lock().await.clear();
    st.alarm_active.lock().await.clear();
    st.persist_alarms().await.map_err(persist_error)?;
    *st.last_alarm.write().await = None;
    Ok(Json(json!({ "ok": true })))
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
        calibration: Some(img.calibration),
    };
    st.references.write().await.push(reference.clone());
    *st.reference.write().await = Some(vals);
    *st.active_ref.write().await = Some(id);
    st.persist_references().await.map_err(persist_error)?;
    st.persist_active_ref().await.map_err(persist_error)?;
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
    st.persist_active_ref().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_reference(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    st.references.write().await.retain(|r| r.id != id);
    if st.active_ref.read().await.as_deref() == Some(id.as_str()) {
        *st.reference.write().await = None;
        *st.active_ref.write().await = None;
    }
    st.persist_references().await.map_err(persist_error)?;
    st.persist_active_ref().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_references(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    st.references.write().await.clear();
    *st.reference.write().await = None;
    *st.active_ref.write().await = None;
    st.persist_references().await.map_err(persist_error)?;
    st.persist_active_ref().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct CompareQuery {
    #[serde(default)]
    name: Option<String>,
    /// First named reference for an A/B difference.
    #[serde(default)]
    a: Option<String>,
    /// Second named reference for an A/B difference.
    #[serde(default)]
    b: Option<String>,
    /// Difference-view half-range in °C.
    #[serde(default)]
    span: Option<f32>,
}

fn checked_span(
    requested: Option<f32>,
    fallback: f32,
) -> Result<f32, (StatusCode, String)> {
    let span = requested.unwrap_or(fallback);
    if !span.is_finite() || !(0.5..=50.0).contains(&span) {
        return Err((
            StatusCode::BAD_REQUEST,
            "span must be between 0.5 and 50 C".into(),
        ));
    }
    Ok(span)
}

fn thermal_dims(st: &AppState) -> (u32, u32) {
    st.spec
        .thermal
        .as_ref()
        .map(|t| (t.width, t.height))
        .unwrap_or((0, 0))
}

/// Rebuild a radiometric image from a saved reference.
fn reference_image(
    reference: &Reference,
    fallback: Calibration,
    width: u32,
    height: u32,
) -> ThermalImage {
    let mut raw = Vec::with_capacity(reference.vals.len() * 2);
    for v in &reference.vals {
        raw.extend_from_slice(&v.to_le_bytes());
    }
    ThermalImage {
        width,
        height,
        raw: Bytes::from(raw),
        calibration: reference.calibration.unwrap_or(fallback),
    }
}

fn find_reference(list: &[Reference], id: &str) -> Result<Reference, (StatusCode, String)> {
    list.iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or((StatusCode::NOT_FOUND, format!("no such reference {id:?}")))
}

fn diff_stats(label: &str, img: &ThermalImage, other: &ThermalImage) -> serde_json::Value {
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0;
    let mut n = 0u32;
    let mut hot = (0u32, 0u32);
    let mut hot_abs = f32::MIN;
    let other_vals: Vec<u16> = other.values().collect();
    for (i, a) in img.values().enumerate() {
        let Some(&b) = other_vals.get(i) else { break };
        let ta = a as f32 * img.calibration.scale + img.calibration.offset;
        let tb = b as f32 * other.calibration.scale + other.calibration.offset;
        let d = ta - tb;
        if d.abs() > hot_abs {
            hot_abs = d.abs();
            hot = ((i as u32) % img.width.max(1), (i as u32) / img.width.max(1));
        }
        min = min.min(d);
        max = max.max(d);
        sum += d;
        n += 1;
    }
    json!({
        "name": label,
        "ready": n > 0,
        "min_c": round2(if n > 0 { min } else { 0.0 }),
        "max_c": round2(if n > 0 { max } else { 0.0 }),
        "avg_c": round2(if n > 0 { sum / n as f32 } else { 0.0 }),
        "n": n,
        "max_at": [hot.0, hot.1],
    })
}

async fn compare_json(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CompareQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if let (Some(a), Some(b)) = (&q.a, &q.b) {
        let list = st.references.read().await.clone();
        let ra = find_reference(&list, a)?;
        let rb = find_reference(&list, b)?;
        let fallback = *st.calibration.read().await;
        let (w, h) = thermal_dims(&st);
        let ia = reference_image(&ra, fallback, w, h);
        let ib = reference_image(&rb, fallback, w, h);
        return Ok(Json(diff_stats(&format!("{} vs {}", ra.name, rb.name), &ia, &ib)));
    }
    let Some(name) = q.name else {
        return Err((
            StatusCode::BAD_REQUEST,
            "compare needs name, or both a and b".into(),
        ));
    };
    let snap = load_snapshot(&st, &name)?;
    let cur = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    Ok(Json(diff_stats(&name, &cur, &snap)))
}

async fn compare_jpg(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CompareQuery>,
) -> Response {
    let display = st.display.read().await.clone();
    let span = match checked_span(q.span, display.dt_span) {
        Ok(v) => v,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    if let (Some(a), Some(b)) = (&q.a, &q.b) {
        let list = st.references.read().await.clone();
        let ra = match find_reference(&list, a) {
            Ok(r) => r,
            Err((code, msg)) => return (code, msg).into_response(),
        };
        let rb = match find_reference(&list, b) {
            Ok(r) => r,
            Err((code, msg)) => return (code, msg).into_response(),
        };
        let fallback = *st.calibration.read().await;
        let (w, h) = thermal_dims(&st);
        let ia = reference_image(&ra, fallback, w, h);
        return match dt_jpeg(&ia, &rb.vals, span, 85) {
            Ok(jpeg) => jpeg_response(jpeg),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
    }
    let Some(name) = q.name else {
        return (StatusCode::BAD_REQUEST, "compare needs name, or both a and b").into_response();
    };
    let snap = match load_snapshot(&st, &name) {
        Ok(s) => s,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let Some(cur) = st.thermal_image().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let vals: Vec<u16> = snap.values().collect();
    match dt_jpeg(&cur, &vals, span, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn set_reference(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CompareQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = st
        .thermal_image()
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet".into()))?;
    if q.span.is_some() {
        let display = st.display.read().await.clone();
        let span = checked_span(q.span, display.dt_span)?;
        let mut next = display;
        next.dt_span = span;
        *st.display.write().await = next;
        st.persist_display().await.map_err(persist_error)?;
    }
    let vals: Vec<u16> = img.values().collect();
    *st.reference.write().await = Some(vals);
    *st.active_ref.write().await = None;
    st.persist_active_ref().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true, "at": now_secs() })))
}

async fn clear_reference(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    *st.reference.write().await = None;
    *st.active_ref.write().await = None;
    st.persist_active_ref().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn dt_image(State(st): State<Arc<AppState>>, Query(q): Query<CompareQuery>) -> Response {
    let display = st.display.read().await.clone();
    let span = match checked_span(q.span, display.dt_span) {
        Ok(v) => v,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    if let (Some(a), Some(b)) = (&q.a, &q.b) {
        let list = st.references.read().await.clone();
        let ra = match find_reference(&list, a) {
            Ok(r) => r,
            Err((code, msg)) => return (code, msg).into_response(),
        };
        let rb = match find_reference(&list, b) {
            Ok(r) => r,
            Err((code, msg)) => return (code, msg).into_response(),
        };
        let fallback = *st.calibration.read().await;
        let (w, h) = thermal_dims(&st);
        let ia = reference_image(&ra, fallback, w, h);
        return match dt_jpeg(&ia, &rb.vals, span, 85) {
            Ok(jpeg) => jpeg_response(jpeg),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
    }
    let Some(img) = st.thermal_image().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no thermal frame yet").into_response();
    };
    let Some(reference) = st.reference.read().await.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no reference frame").into_response();
    };
    match dt_jpeg(&img, &reference, span, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn dt_mjpg(State(st): State<Arc<AppState>>) -> Response {
    let stream = async_stream::stream! {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(img) = st.thermal_image().await else { continue };
            let Some(reference) = st.reference.read().await.clone() else { continue };
            let span = st.display.read().await.dt_span;
            let Ok(jpeg) = dt_jpeg(&img, &reference, span, 80) else { continue };
            yield Ok::<Bytes, std::io::Error>(Bytes::from(multipart_head(&jpeg)));
            yield Ok(Bytes::from(jpeg));
            yield Ok(Bytes::from_static(b"\r\n"));
        }
    };
    mjpeg_response(stream)
}

async fn dt_json(State(st): State<Arc<AppState>>, Query(q): Query<CompareQuery>) -> Json<serde_json::Value> {
    if let (Some(a), Some(b)) = (&q.a, &q.b) {
        let list = st.references.read().await.clone();
        let (Ok(ra), Ok(rb)) = (find_reference(&list, a), find_reference(&list, b)) else {
            return Json(json!({ "ready": false, "error": "no such reference" }));
        };
        let fallback = *st.calibration.read().await;
        let (w, h) = thermal_dims(&st);
        let ia = reference_image(&ra, fallback, w, h);
        let ib = reference_image(&rb, fallback, w, h);
        let mut out = diff_stats(&format!("{} vs {}", ra.name, rb.name), &ia, &ib);
        if let Some(obj) = out.as_object_mut() {
            obj.insert("a".into(), json!(ra.id));
            obj.insert("b".into(), json!(rb.id));
        }
        return Json(out);
    }
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

async fn probe_camera(ip: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(Ok(mut socket)) = tokio::time::timeout(
        Duration::from_millis(300),
        tokio::net::TcpStream::connect((ip, 3001)),
    )
    .await
    else {
        return false;
    };
    let packet = crate::camera::qianli::protocol::register(0x6c, 0);
    if socket.write_all(&packet).await.is_err() {
        return false;
    }
    let mut buf = vec![0u8; 1024];
    let n = match tokio::time::timeout(Duration::from_millis(700), socket.read(&mut buf)).await {
        Ok(Ok(n)) => n,
        _ => 0,
    };
    n >= 70 && buf[..n].windows(4).any(|w| w == [0x78, 0xb6, 0x3a, 0x12])
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
            if probe_camera(&ip).await { Some(ip) } else { None }
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraSettingsRequest {
    ip: Option<String>,
    mask: Option<String>,
    gw: Option<String>,
    dns: Option<String>,
}

async fn camera_settings_write(
    State(st): State<Arc<AppState>>,
    Json(req): Json<CameraSettingsRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let mut changes = serde_json::Map::new();
    for (key, value) in [
        ("ip", req.ip),
        ("mask", req.mask),
        ("gw", req.gw),
        ("dns", req.dns),
    ] {
        let Some(value) = value else { continue };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !crate::camera::validate_ipv4(value) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{key} is not a valid IPv4 address"),
            ));
        }
        changes.insert(key.to_string(), serde_json::Value::String(value.to_string()));
    }
    if changes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "supply at least one of ip, mask, gw or dns".into(),
        ));
    }

    let out = st
        .driver
        .write_settings(serde_json::Value::Object(changes))
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let applied = out
        .get("applied")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok((
        if applied {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        },
        Json(out),
    ))
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
    if req.points.is_none() && req.scale.is_none() && req.offset.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "calibration needs points, scale or offset".into(),
        ));
    }
    let next = if let Some(points) = req.points {
        Calibration::fit([
            (points[0][0] as u16, points[0][1]),
            (points[1][0] as u16, points[1][1]),
        ])
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    } else {
        // Fill an omitted half from the current calibration instead of
        // silently reverting it to the factory default.
        let current = *st.calibration.read().await;
        Calibration {
            scale: req.scale.unwrap_or(current.scale),
            offset: req.offset.unwrap_or(current.offset),
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
    let regions = st.regions.read().await.clone();
    let mut rstats: Vec<serde_json::Value> = regions.iter().filter_map(|r| region_stats(&img, r)).collect();
    attach_deltas(&img, &regions, &mut rstats);
    let reference_at = {
        let active = st.active_ref.read().await.clone();
        match active {
            Some(id) => st.references.read().await.iter().find(|r| r.id == id).map(|r| r.at),
            None => None,
        }
    };
    let meta = json!({
        "name": name,
        "at": now.as_secs(),
        "stats": stats,
        "calibration": img.calibration,
        "display": display,
        "regions": regions,
        "region_stats": rstats,
        "reference_at": reference_at,
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

#[derive(Deserialize)]
struct ReanalyzeQuery {
    name: String,
    #[serde(default)]
    palette: Option<String>,
    #[serde(default)]
    clip_lo: Option<f32>,
    #[serde(default)]
    clip_hi: Option<f32>,
    #[serde(default)]
    span_lo: Option<f32>,
    #[serde(default)]
    span_hi: Option<f32>,
}

fn reanalyze_display(base: &DisplaySettings, q: &ReanalyzeQuery) -> Result<DisplaySettings, (StatusCode, String)> {
    let mut display = base.clone();
    if let Some(palette) = &q.palette {
        if !PALETTES.contains(&palette.as_str()) {
            return Err((StatusCode::BAD_REQUEST, "unknown palette".into()));
        }
        display.palette = palette.clone();
    }
    if let Some(v) = q.clip_lo { display.clip_lo = Some(v); }
    if let Some(v) = q.clip_hi { display.clip_hi = Some(v); }
    if let Some(v) = q.span_lo { display.span_lo = Some(v); }
    if let Some(v) = q.span_hi { display.span_hi = Some(v); }
    Ok(display)
}

async fn reanalyze_jpg(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ReanalyzeQuery>,
) -> Response {
    let img = match load_snapshot(&st, &q.name) {
        Ok(img) => img,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let base = st.display.read().await.clone();
    let display = match reanalyze_display(&base, &q) {
        Ok(d) => d,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    match render_jpeg(&img, &display, 85) {
        Ok(jpeg) => jpeg_response(jpeg),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn reanalyze_json(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ReanalyzeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let img = load_snapshot(&st, &q.name)?;
    let base = st.display.read().await.clone();
    let display = reanalyze_display(&base, &q)?;
    let stats = img.stats();
    let regions: Vec<_> = st
        .regions
        .read()
        .await
        .iter()
        .filter_map(|r| region_stats(&img, r))
        .collect();
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
        "display": display,
        "regions": regions,
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
) -> Result<Json<PrerecordConfig>, (StatusCode, String)> {
    let cfg = PrerecordConfig {
        enabled: next.enabled,
        before_s: next.before_s.min(60),
        after_s: next.after_s.min(60),
    };
    *st.prerecord.write().await = cfg.clone();
    st.persist_prerecord().await.map_err(persist_error)?;
    Ok(Json(cfg))
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
    use std::io::Write;
    let (base, want_zip) = match name.strip_suffix(".zip") {
        Some(base) => (base.to_string(), true),
        None => (name.clone(), false),
    };
    if !base.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    if want_zip {
        let dir = st.captures_dir();
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for ext in ["mjpg", "json"] {
                let path = dir.join(format!("{base}.{ext}"));
                if let Ok(bytes) = std::fs::read(&path) {
                    if writer.start_file(format!("{base}.{ext}"), options).is_ok() {
                        let _ = writer.write_all(&bytes);
                    }
                }
            }
            let _ = writer.finish();
        }
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/zip")
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{base}.zip\""),
            )
            .body(Body::from(cursor.into_inner()))
            .unwrap();
    }
    match std::fs::read(st.captures_dir().join(format!("{base}.mjpg"))) {
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
) -> Result<Json<Align>, (StatusCode, String)> {
    let clamped = Align {
        x: next.x.clamp(-50.0, 50.0),
        y: next.y.clamp(-50.0, 50.0),
        scale: next.scale.clamp(0.25, 4.0),
        rot: next.rot.rem_euclid(360.0),
        flip_h: next.flip_h,
        flip_v: next.flip_v,
    };
    *st.align.write().await = clamped.clone();
    st.persist_align().await.map_err(persist_error)?;
    Ok(Json(clamped))
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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
    st.persist_notify().await.map_err(persist_error)?;
    Ok(Json(status))
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
    Json(json!({
        "ok": true,
        "name": "thermolink",
        "version": env!("CARGO_PKG_VERSION"),
        "repo": crate::update::repo(),
    }))
}

fn upstream_error(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_GATEWAY, e.to_string())
}

/// `GET /api/update` — compare the running build with the latest release.
/// A private repository with no published release is an honest 502, not a
/// fabricated "up to date".
async fn update_check() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let repo = crate::update::repo();
    let release = crate::update::latest(&repo).await.map_err(upstream_error)?;
    let current = env!("CARGO_PKG_VERSION");
    let asset =
        crate::update::pick_asset(&release.assets, std::env::consts::OS, std::env::consts::ARCH);
    Ok(Json(json!({
        "ok": true,
        "current": current,
        "latest": release.tag,
        "name": release.name,
        "url": release.url,
        "notes": release.notes,
        "available": crate::update::is_newer(&release.tag, current),
        "asset": asset.map(|a| json!({ "name": a.name, "size": a.size })),
        "repo": repo,
    })))
}

/// `POST /api/update` — install the latest release asset over this binary.
/// The running image is untouched until `POST /api/restart` execs again.
async fn update_apply(
    State(st): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let repo = crate::update::repo();
    let release = crate::update::latest(&repo).await.map_err(upstream_error)?;
    if release.tag.is_empty() {
        return Err((StatusCode::BAD_GATEWAY, "release has no tag".into()));
    }
    let asset =
        crate::update::pick_asset(&release.assets, std::env::consts::OS, std::env::consts::ARCH)
            .ok_or_else(|| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!(
                        "release {} has no binary asset for {} {}",
                        release.tag,
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ),
                )
            })?;
    let bytes = crate::update::download(&asset.url)
        .await
        .map_err(upstream_error)?;
    let exe = st.exe.clone();
    let installed = tokio::task::spawn_blocking(move || crate::update::install(&bytes, &exe))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "from": env!("CARGO_PKG_VERSION"),
        "to": release.tag,
        "restart_required": true,
        "detail": {
            "path": installed.path,
            "backup": installed.backup,
            "bytes": installed.bytes,
        }
    })))
}

/// `POST /api/restart` — respond first, then exec this binary in place.
/// In-memory sessions are replaced, so the caller signs in again.
async fn restart_relay(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let exe = st.exe.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if let Err(e) = crate::update::restart(&exe) {
            tracing::error!(error = %e, "relay restart failed");
        }
    });
    Json(json!({ "ok": true, "restarting": true, "login_required": true }))
}

async fn set_region_ref(
    State(st): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    {
        let mut regions = st.regions.write().await;
        for r in regions.iter_mut() {
            r.is_ref = r.id == id;
        }
    }
    st.persist_regions().await.map_err(persist_error)?;
    Ok(Json(json!({ "ok": true })))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isotherm_defaults_and_validation() {
        let state = IsothermState::default();
        assert_eq!(state.current, "pcba 25-60");
        assert!(state.profiles.contains_key("pcba 25-60"));
        assert!(state.profiles.contains_key("fine 1C"));
        for bands in state.profiles.values() {
            validate_iso_bands(bands).expect("built-in bands are valid");
        }

        let flat = vec![IsoBand { lo: 5.0, hi: 5.0, colour: "#ffffff".into() }];
        assert!(validate_iso_bands(&flat).is_err(), "hi must exceed lo");
        let colour = vec![IsoBand { lo: 1.0, hi: 2.0, colour: "red".into() }];
        assert!(validate_iso_bands(&colour).is_err(), "colour must be #rrggbb");
        let too_many = vec![
            IsoBand { lo: 0.0, hi: 1.0, colour: "#ffffff".into() };
            crate::render::ISO_BAND_LIMIT + 1
        ];
        assert!(validate_iso_bands(&too_many).is_err(), "band cap");
        assert!(validate_iso_bands(&[]).is_err(), "empty table");
    }

    #[test]
    fn isotherm_state_round_trips() {
        let mut state = IsothermState::default();
        state.current = "mine".into();
        state.profiles.insert(
            "mine".into(),
            vec![IsoBand { lo: 10.0, hi: 20.0, colour: "#00ff00".into() }],
        );
        let text = serde_json::to_string(&state).expect("encode");
        let back: IsothermState = serde_json::from_str(&text).expect("decode");
        assert_eq!(back.current, "mine");
        assert_eq!(back.profiles["mine"][0].hi, 20.0);
    }

    #[test]
    fn guest_guard_maps_writes_to_capabilities() {
        assert_eq!(
            guard_for(&Method::GET, "/api/thermal.json", None),
            Guard::Read
        );
        assert_eq!(
            guard_for(&Method::POST, "/api/regions", None),
            Guard::Cap("measure")
        );
        assert_eq!(
            guard_for(&Method::DELETE, "/api/snapshots/x.raw", None),
            Guard::Cap("snapshot")
        );
        assert_eq!(
            guard_for(&Method::POST, "/api/references", None),
            Guard::Cap("reference")
        );
        assert_eq!(
            guard_for(&Method::POST, "/api/display", None),
            Guard::Cap("display")
        );
        assert_eq!(
            guard_for(&Method::GET, "/api/history.json", Some("clear=1")),
            Guard::Cap("measure")
        );
        // Notification credentials, the updater and account management stay
        // admin-only even for reads.
        assert_eq!(guard_for(&Method::GET, "/api/notify", None), Guard::Admin);
        assert_eq!(guard_for(&Method::GET, "/api/update", None), Guard::Admin);
        assert_eq!(guard_for(&Method::POST, "/api/update", None), Guard::Admin);
        assert_eq!(guard_for(&Method::POST, "/api/restart", None), Guard::Admin);
        assert_eq!(
            guard_for(&Method::POST, "/api/camera/reboot", None),
            Guard::Admin
        );
        assert_eq!(
            guard_for(&Method::POST, "/api/camera/settings", None),
            Guard::Admin
        );
        assert_eq!(
            guard_for(&Method::POST, "/api/account", None),
            Guard::Admin
        );
    }

    #[test]
    fn old_auth_files_default_to_a_closed_guest() {
        let old = r#"{"username":"admin","hash":"x"}"#;
        let auth: AuthFile = serde_json::from_str(old).expect("legacy auth.json");
        assert!(!auth.guest.enabled);
        let perms = auth.perms();
        assert_eq!(perms["measure"], true);
        assert_eq!(perms["snapshot"], true);
        assert_eq!(perms["calibration"], false);
        assert!(!perms.contains_key("camera"));
    }

    #[test]
    fn stored_guest_permissions_are_restricted_to_known_caps() {
        let text = r#"{"username":"admin","hash":"x","guest":{"enabled":true,"hash":"y"},
            "guest_perms":{"measure":false,"alarms":true,"camera":true}}"#;
        let auth: AuthFile = serde_json::from_str(text).expect("auth.json");
        let perms = auth.perms();
        assert_eq!(perms["measure"], false);
        assert_eq!(perms["alarms"], true);
        assert!(!perms.contains_key("camera"), "unknown caps are never granted");
    }

    #[test]
    fn password_hash_round_trips() {
        let hash = hash_password("correct horse battery").expect("hash");
        assert!(verify_hash("correct horse battery", &hash));
        assert!(!verify_hash("wrong password", &hash));
        assert!(!verify_hash("anything", "not-a-hash"));
    }

    #[test]
    fn secure_request_recognises_proxy_header() {
        let mut headers = HeaderMap::new();
        assert!(!is_secure_request(&headers));
        headers.insert("x-forwarded-proto", "https, http".parse().unwrap());
        assert!(is_secure_request(&headers));
        headers.insert("x-forwarded-proto", "http".parse().unwrap());
        assert!(!is_secure_request(&headers));
    }

    #[test]
    fn secure_cookie_only_when_tls_is_used() {
        let plain = session_cookie("abc", false);
        assert!(!plain.contains("Secure"));
        let tls = session_cookie("abc", true);
        assert!(tls.contains("; Secure"));
        assert!(tls.contains("HttpOnly"));
    }

    #[tokio::test]
    async fn embedded_translations_are_served_and_complete() {
        let res = i18n_file(AxumPath("de.json".into())).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["tab.alarms"], "Alarme");

        // Every language parses and carries exactly the English key set, so the
        // client never has to guess which strings exist.
        let mut en_keys: Vec<String> = Vec::new();
        for (lang, text) in I18N_LANGS {
            let value: serde_json::Value = serde_json::from_str(text).expect(lang);
            let mut keys: Vec<String> =
                value.as_object().expect("object").keys().cloned().collect();
            keys.sort();
            if *lang == "en" {
                en_keys = keys;
            } else {
                assert_eq!(keys.len(), en_keys.len(), "{lang} key count");
                assert_eq!(keys, en_keys, "{lang} key set");
            }
        }
        assert!(en_keys.iter().any(|k| k == "auth.signIn"));

        let missing = i18n_file(AxumPath("xx.json".into())).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn atomic_write_replaces_the_previous_file() {
        let dir = std::env::temp_dir().join(format!(
            "thermolink-persist-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        write_atomic(&dir, "state.json", b"{\"v\":1}").await.expect("first write");
        write_atomic(&dir, "state.json", b"{\"v\":2}").await.expect("replace");
        let body = tokio::fs::read_to_string(dir.join("state.json")).await.unwrap();
        assert_eq!(body, "{\"v\":2}");
        // No temporary file is left behind.
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec!["state.json".to_string()]);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn atomic_write_reports_a_failed_directory() {
        let base = std::env::temp_dir().join(format!(
            "thermolink-persist-file-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::write(&base, b"not a directory").await.unwrap();
        let err = write_atomic(&base, "state.json", b"{}").await;
        assert!(err.is_err(), "writing under a regular file must fail");
        let _ = tokio::fs::remove_file(&base).await;
    }
}
