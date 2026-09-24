//! Phone access: a small web server that serves the phone app and its API.
//!
//! It listens on 127.0.0.1 only; `tailscale serve` puts it on your tailnet
//! over HTTPS. Every API call needs a device token, which a phone gets by
//! pairing: it shows a code, you approve it in Codebench. Tokens are kept
//! hashed. The GTK side owns the terminals, so the server turns requests
//! into `Command`s and serves snapshots the GTK side publishes.

use crate::store::{config_dir, now};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub const DEFAULT_PORT: u16 = 47823;

const INDEX: &str = include_str!("phone/index.html");
const APP_JS: &str = include_str!("phone/app.js");
const APP_CSS: &str = include_str!("phone/app.css");
const ICON: &str = include_str!("phone/icon.svg");
const MANIFEST: &str = include_str!("phone/manifest.webmanifest");

/// What the phone asks Codebench to do.
#[derive(Debug)]
pub enum Command {
    /// Type a message into a task (queued if busy, starts it if stopped).
    Send { task: String, text: String },
    /// Press keys in a task: "enter", "esc", "up", "down", "tab", digits...
    Keys { task: String, keys: Vec<String> },
    /// Start or resume a task so its screen can be shown.
    Open { task: String },
    NewTask { project: String, agent: String, title: String, prompt: String },
    /// A phone wants to pair and shows `code`; approve it on the desktop.
    PairRequest { id: String, code: String, name: String },
}

#[derive(Clone, Debug)]
enum Pairing {
    Pending { name: String, at: u64 },
    Approved { token: String },
    Denied,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Device {
    pub name: String,
    pub token_sha256: String,
    pub paired: u64,
    #[serde(default)]
    pub last_seen: u64,
}

#[derive(Default)]
struct Shared {
    state_json: String,
    theme_css: String,
    screens: HashMap<String, String>,
    watched: HashSet<String>,
    pairings: HashMap<String, Pairing>,
    devices: Vec<Device>,
}

#[derive(Clone)]
struct Ctx {
    shared: Arc<Mutex<Shared>>,
    events: broadcast::Sender<String>,
    commands: async_channel::Sender<Command>,
    /// The only Tailscale login allowed, when requests come via tailscale.
    owner: Option<String>,
}

/// The GTK side's handle on the running server.
pub struct Remote {
    shared: Arc<Mutex<Shared>>,
    events: broadcast::Sender<String>,
    pub commands: async_channel::Receiver<Command>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

fn devices_file() -> std::path::PathBuf {
    config_dir().join("devices.json")
}

pub fn load_devices() -> Vec<Device> {
    std::fs::read(devices_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_devices(devices: &[Device]) {
    use std::os::unix::fs::PermissionsExt;
    let file = devices_file();
    let tmp = file.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_vec_pretty(devices)
        && std::fs::write(&tmp, json).is_ok()
    {
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::rename(tmp, file);
    }
}

/// Revokes one phone while the server is off.
pub fn revoke_device(token_sha256: &str) {
    let rest: Vec<Device> = load_devices().into_iter().filter(|d| d.token_sha256 != token_sha256).collect();
    save_devices(&rest);
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random source");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_code() -> String {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("system random source");
    format!("{:06}", u32::from_le_bytes(buf) % 1_000_000)
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The Tailscale login of this machine, from `tailscale status --json`.
fn tailscale_owner() -> Option<String> {
    let out = std::process::Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let uid = v["Self"]["UserID"].to_string();
    v["User"][&uid]["LoginName"].as_str().map(str::to_string)
}

impl Remote {
    pub fn start(port: u16) -> Result<Remote, String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let shared = Arc::new(Mutex::new(Shared { devices: load_devices(), ..Default::default() }));
        let (events, _) = broadcast::channel(256);
        let (tx, rx) = async_channel::unbounded();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let ctx = Ctx { shared: shared.clone(), events: events.clone(), commands: tx, owner: tailscale_owner() };

        std::thread::Builder::new()
            .name("codebench-remote".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
                let Ok(rt) = rt else { return };
                rt.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                    let app = router(ctx);
                    let _ = axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            let _ = stop_rx.await;
                        })
                        .await;
                });
            })
            .map_err(|e| e.to_string())?;

        Ok(Remote { shared, events, commands: rx, shutdown: Some(stop_tx) })
    }

    /// The task list the phone shows; pushed to live phones when it changes.
    pub fn publish_state(&self, json: String) {
        let mut s = self.shared.lock().unwrap();
        if s.state_json == json {
            return;
        }
        s.state_json = json.clone();
        drop(s);
        let _ = self.events.send(format!(r#"{{"type":"state","data":{json}}}"#));
    }

    pub fn publish_theme(&self, css: String) {
        self.shared.lock().unwrap().theme_css = css;
        let _ = self.events.send(r#"{"type":"theme"}"#.to_string());
    }

    pub fn is_watched(&self, key: &str) -> bool {
        self.shared.lock().unwrap().watched.contains(key)
    }

    pub fn publish_screen(&self, key: &str, html: String) {
        let mut s = self.shared.lock().unwrap();
        if s.screens.get(key) == Some(&html) {
            return;
        }
        s.screens.insert(key.to_string(), html.clone());
        drop(s);
        let msg = serde_json::json!({ "type": "screen", "task": key, "html": html });
        let _ = self.events.send(msg.to_string());
    }

    pub fn decide_pairing(&self, id: &str, approve: bool) {
        let mut s = self.shared.lock().unwrap();
        let Some(Pairing::Pending { name, .. }) = s.pairings.get(id).cloned() else { return };
        if approve {
            let token = random_hex(32);
            s.devices.push(Device { name, token_sha256: sha256_hex(&token), paired: now(), last_seen: now() });
            save_devices(&s.devices);
            s.pairings.insert(id.to_string(), Pairing::Approved { token });
        } else {
            s.pairings.insert(id.to_string(), Pairing::Denied);
        }
    }

    pub fn devices(&self) -> Vec<Device> {
        self.shared.lock().unwrap().devices.clone()
    }

    pub fn revoke(&self, token_sha256: &str) {
        let mut s = self.shared.lock().unwrap();
        s.devices.retain(|d| d.token_sha256 != token_sha256);
        save_devices(&s.devices);
        let _ = self.events.send(r#"{"type":"revoked"}"#.to_string());
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route("/", get(|| async { page(INDEX, "text/html; charset=utf-8") }))
        .route("/app.js", get(|| async { page(APP_JS, "text/javascript; charset=utf-8") }))
        .route("/app.css", get(|| async { page(APP_CSS, "text/css; charset=utf-8") }))
        .route("/icon.svg", get(|| async { page(ICON, "image/svg+xml") }))
        .route("/manifest.webmanifest", get(|| async { page(MANIFEST, "application/manifest+json") }))
        .route("/theme.css", get(theme_css))
        .route("/api/pair", post(pair_start))
        .route("/api/pair/{id}", get(pair_status))
        .route("/api/state", get(state))
        .route("/api/task/{id}/screen", get(screen))
        .route("/api/task/{id}/send", post(send))
        .route("/api/task/{id}/keys", post(keys))
        .route("/api/task/{id}/open", post(open))
        .route("/api/tasks", post(new_task))
        .route("/api/live", get(live))
        .with_state(ctx)
}

fn page(body: &'static str, kind: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
            ),
        ],
        body,
    )
        .into_response()
}

async fn theme_css(State(ctx): State<Ctx>) -> Response {
    let css = ctx.shared.lock().unwrap().theme_css.clone();
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], css).into_response()
}

/// Requests through `tailscale serve` carry the caller's login; only yours
/// gets in. Local requests without the header are allowed (they come from
/// this machine).
fn tailnet_ok(ctx: &Ctx, headers: &HeaderMap) -> bool {
    match (headers.get("tailscale-user-login").and_then(|v| v.to_str().ok()), &ctx.owner) {
        (Some(login), Some(owner)) => login.eq_ignore_ascii_case(owner),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn token_ok(ctx: &Ctx, token: &str) -> bool {
    if token.len() < 32 {
        return false;
    }
    let hash = sha256_hex(token);
    let mut s = ctx.shared.lock().unwrap();
    let Some(d) = s.devices.iter_mut().find(|d| d.token_sha256 == hash) else { return false };
    d.last_seen = now();
    true
}

fn authorized(ctx: &Ctx, headers: &HeaderMap) -> Result<(), StatusCode> {
    if !tailnet_ok(ctx, headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if token_ok(ctx, token) { Ok(()) } else { Err(StatusCode::UNAUTHORIZED) }
}

#[derive(Deserialize)]
struct PairStart {
    name: String,
}

async fn pair_start(State(ctx): State<Ctx>, headers: HeaderMap, Json(req): Json<PairStart>) -> Response {
    if !tailnet_ok(&ctx, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let name: String = req.name.chars().filter(|c| !c.is_control()).take(40).collect();
    let (id, code) = {
        let mut s = ctx.shared.lock().unwrap();
        // Old requests expire, and only a few may wait at once.
        let cutoff = now().saturating_sub(180);
        s.pairings.retain(|_, p| !matches!(p, Pairing::Pending { at, .. } if *at < cutoff));
        let pending = s.pairings.values().filter(|p| matches!(p, Pairing::Pending { .. })).count();
        if pending >= 3 {
            return (StatusCode::TOO_MANY_REQUESTS, "too many pairing requests; try again in a few minutes").into_response();
        }
        let (id, code) = (random_hex(16), random_code());
        s.pairings.insert(id.clone(), Pairing::Pending { name: name.clone(), at: now() });
        (id, code)
    };
    let _ = ctx.commands.send(Command::PairRequest { id: id.clone(), code: code.clone(), name }).await;
    Json(serde_json::json!({ "id": id, "code": code })).into_response()
}

async fn pair_status(State(ctx): State<Ctx>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !tailnet_ok(&ctx, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut s = ctx.shared.lock().unwrap();
    let reply = match s.pairings.get(&id).cloned() {
        Some(Pairing::Pending { .. }) => serde_json::json!({ "state": "pending" }),
        Some(Pairing::Approved { token }) => {
            // Handed out once.
            s.pairings.remove(&id);
            serde_json::json!({ "state": "approved", "token": token })
        }
        Some(Pairing::Denied) => {
            s.pairings.remove(&id);
            serde_json::json!({ "state": "denied" })
        }
        None => serde_json::json!({ "state": "expired" }),
    };
    Json(reply).into_response()
}

async fn state(State(ctx): State<Ctx>, headers: HeaderMap) -> Response {
    if let Err(code) = authorized(&ctx, &headers) {
        return code.into_response();
    }
    let json = ctx.shared.lock().unwrap().state_json.clone();
    ([(header::CONTENT_TYPE, "application/json")], json).into_response()
}

async fn screen(State(ctx): State<Ctx>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(code) = authorized(&ctx, &headers) {
        return code.into_response();
    }
    let html = {
        let mut s = ctx.shared.lock().unwrap();
        s.watched.insert(id.clone());
        s.screens.get(&id).cloned().unwrap_or_default()
    };
    Json(serde_json::json!({ "html": html })).into_response()
}

#[derive(Deserialize)]
struct SendBody {
    text: String,
}

async fn send(State(ctx): State<Ctx>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<SendBody>) -> Response {
    if let Err(code) = authorized(&ctx, &headers) {
        return code.into_response();
    }
    if b.text.trim().is_empty() || b.text.len() > 20_000 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let _ = ctx.commands.send(Command::Send { task: id, text: b.text }).await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct KeysBody {
    keys: Vec<String>,
}

async fn keys(State(ctx): State<Ctx>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<KeysBody>) -> Response {
    if let Err(code) = authorized(&ctx, &headers) {
        return code.into_response();
    }
    if b.keys.is_empty() || b.keys.len() > 20 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let _ = ctx.commands.send(Command::Keys { task: id, keys: b.keys }).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn open(State(ctx): State<Ctx>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(code) = authorized(&ctx, &headers) {
        return code.into_response();
    }
    ctx.shared.lock().unwrap().watched.insert(id.clone());
    let _ = ctx.commands.send(Command::Open { task: id }).await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct NewTaskBody {
    project: String,
    agent: String,
    #[serde(default)]
    title: String,
    prompt: String,
}

async fn new_task(State(ctx): State<Ctx>, headers: HeaderMap, Json(b): Json<NewTaskBody>) -> Response {
    if let Err(code) = authorized(&ctx, &headers) {
        return code.into_response();
    }
    if b.prompt.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let _ = ctx
        .commands
        .send(Command::NewTask { project: b.project, agent: b.agent, title: b.title, prompt: b.prompt })
        .await;
    StatusCode::NO_CONTENT.into_response()
}

/// Live updates. The first message must be `{"auth": "<token>"}`, since a
/// browser cannot set headers on a WebSocket; `{"watch": "<task>"}` asks for
/// that task's screen.
async fn live(State(ctx): State<Ctx>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !tailnet_ok(&ctx, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| live_socket(ctx, socket))
}

async fn live_socket(ctx: Ctx, mut socket: WebSocket) {
    let authed = match tokio::time::timeout(std::time::Duration::from_secs(10), socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<serde_json::Value>(&t)
            .ok()
            .and_then(|v| v["auth"].as_str().map(|s| token_ok(&ctx, s)))
            .unwrap_or(false),
        _ => false,
    };
    if !authed {
        let _ = socket.send(Message::Text(r#"{"type":"unauthorized"}"#.into())).await;
        return;
    }
    let initial = ctx.shared.lock().unwrap().state_json.clone();
    if !initial.is_empty() {
        let _ = socket.send(Message::Text(format!(r#"{{"type":"state","data":{initial}}}"#).into())).await;
    }
    let mut events = ctx.events.subscribe();
    let mut watching: Option<String> = None;
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                if let Message::Text(t) = msg
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
                    && let Some(task) = v["watch"].as_str()
                {
                    watching = Some(task.to_string());
                    let html = {
                        let mut s = ctx.shared.lock().unwrap();
                        s.watched.insert(task.to_string());
                        s.screens.get(task).cloned()
                    };
                    if let Some(html) = html {
                        let msg = serde_json::json!({ "type": "screen", "task": task, "html": html });
                        let _ = socket.send(Message::Text(msg.to_string().into())).await;
                    }
                }
            }
            event = events.recv() => {
                let Ok(event) = event else { continue };
                // Screens go only to the phone looking at that task.
                if event.starts_with(r#"{"html""#) || event.contains(r#""type":"screen""#) {
                    let for_me = watching.as_deref().is_some_and(|w| event.contains(&format!(r#""task":"{w}""#)));
                    if !for_me {
                        continue;
                    }
                }
                if socket.send(Message::Text(event.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_codes_look_right() {
        assert_eq!(random_hex(32).len(), 64);
        assert_eq!(random_code().len(), 6);
        assert_ne!(random_hex(16), random_hex(16));
        assert_eq!(sha256_hex("abc").len(), 64);
    }
}
