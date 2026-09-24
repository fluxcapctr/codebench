//! Artifacts: visuals an agent makes for you (HTML pages, SVG, Mermaid
//! diagrams, markdown documents, images), kept in the project's notes
//! folder under `artifacts/` and shown in a viewer window.
//!
//! They are served by a small local server on their own port, so they are a
//! different origin from the phone app, and every page is sandboxed: an
//! artifact's scripts can run, but cannot read Codebench's storage or call
//! its API. Viewer pages reload themselves when the file changes.

use crate::store::Project;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

pub const DEFAULT_PORT: u16 = 47824;

/// The artifact server's port; CODEBENCH_ARTIFACT_PORT overrides it.
pub fn port() -> u16 {
    std::env::var("CODEBENCH_ARTIFACT_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// The kinds an artifact can be, by file extension.
pub const KINDS: &[(&str, &str)] = &[
    ("html", "html"),
    ("svg", "svg"),
    ("mermaid", "mmd"),
    ("markdown", "md"),
    ("png", "png"),
    ("jpg", "jpg"),
    ("gif", "gif"),
    ("webp", "webp"),
    ("pdf", "pdf"),
];

pub fn dir(project: &Project) -> PathBuf {
    project.notes_dir().join("artifacts")
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub file: String,
    pub title: String,
    pub kind: String,
    pub modified: u64,
}

fn kind_of(file: &str) -> Option<&'static str> {
    let ext = file.rsplit_once('.')?.1.to_lowercase();
    let ext = if ext == "jpeg" { "jpg".to_string() } else { ext };
    KINDS.iter().find(|(_, e)| *e == ext).map(|(k, _)| *k)
}

/// A plain file name inside the artifacts folder, never a path.
fn safe_name(file: &str) -> Option<&str> {
    let ok = !file.is_empty()
        && !file.starts_with('.')
        && file.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (ok && kind_of(file).is_some()).then_some(file)
}

/// Newest first.
pub fn list(project: &Project) -> Vec<Artifact> {
    let mut out: Vec<Artifact> = std::fs::read_dir(dir(project))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let kind = kind_of(safe_name(&file)?)?;
            let modified = e.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            let title = file.rsplit_once('.').map_or(file.as_str(), |(stem, _)| stem).replace(['-', '_'], " ");
            Some(Artifact { file, title, kind: kind.to_string(), modified })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// Saves an artifact from content or by copying a file, named after its
/// title so showing the same title again updates it. Returns the file name.
pub fn save(project: &Project, title: &str, kind: Option<&str>, content: Option<&str>, from: Option<&std::path::Path>) -> Result<String, String> {
    let stem = crate::workflow::slug(title);
    if stem.is_empty() {
        return Err("give the artifact a title".into());
    }
    let ext = match (kind, from) {
        (Some(k), _) => KINDS.iter().find(|(name, ext)| *name == k || *ext == k).map(|(_, e)| *e).ok_or(format!("unknown kind {k}"))?,
        (None, Some(path)) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            KINDS.iter().find(|(k, _)| Some(*k) == kind_of(&name)).map(|(_, e)| *e).ok_or("unsupported file type")?
        }
        (None, None) => "html",
    };
    let file = format!("{stem}.{ext}");
    let dest = dir(project);
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    match (content, from) {
        (Some(text), _) => std::fs::write(dest.join(&file), text).map_err(|e| e.to_string())?,
        (None, Some(path)) => {
            let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if meta.len() > 50 * 1024 * 1024 {
                return Err("that file is over 50 MB".into());
            }
            std::fs::copy(path, dest.join(&file)).map_err(|e| e.to_string())?;
        }
        (None, None) => return Err("give content or a path".into()),
    }
    Ok(file)
}

// ── server ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Ctx {
    key: String,
    owner: Option<String>,
}

/// A random key every artifact URL carries; without it nothing is served.
pub struct Server {
    pub key: String,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl Server {
    pub fn start() -> Result<Server, String> {
        let port = port();
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 16];
        getrandom::fill(&mut buf).map_err(|e| e.to_string())?;
        let key: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        let ctx = Ctx { key: key.clone(), owner: crate::remote::tailscale_owner() };
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("codebench-artifacts".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
                rt.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                    let app = Router::new()
                        .route("/v/{project}/{file}", get(view))
                        .route("/raw/{project}/{file}", get(raw))
                        .route("/mtime/{project}/{file}", get(mtime))
                        .with_state(ctx);
                    let _ = axum::serve(listener, app).with_graceful_shutdown(async { let _ = stop_rx.await; }).await;
                });
            })
            .map_err(|e| e.to_string())?;
        Ok(Server { key, _shutdown: stop_tx })
    }

    pub fn local_url(&self, project: &str, file: &str) -> String {
        format!("http://127.0.0.1:{}/v/{project}/{file}?k={}", port(), self.key)
    }
}

#[derive(Deserialize)]
struct Key {
    #[serde(default)]
    k: String,
}

fn allowed(ctx: &Ctx, headers: &HeaderMap, key: &str) -> bool {
    let tailnet_ok = match (headers.get("tailscale-user-login").and_then(|v| v.to_str().ok()), &ctx.owner) {
        (Some(login), Some(owner)) => login.eq_ignore_ascii_case(owner),
        (Some(_), None) => false,
        (None, _) => true,
    };
    tailnet_ok && key == ctx.key
}

fn find_file(project: &str, file: &str) -> Option<PathBuf> {
    let file = safe_name(file)?;
    let state = crate::store::State::load();
    let p = state.project(project)?;
    let path = dir(p).join(file);
    path.is_file().then_some(path)
}

/// Every artifact page runs sandboxed: scripts yes, same-origin no.
const SANDBOX: &str = "sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads";

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Polls the file's modified time and reloads when it changes.
fn reload_script(project: &str, file: &str, key: &str) -> String {
    format!(
        r#"<script>(function(){{var last=null;setInterval(function(){{fetch("/mtime/{project}/{file}?k={key}").then(function(r){{return r.text()}}).then(function(t){{if(last!==null&&t!==last)location.reload();last=t}}).catch(function(){{}})}},1000)}})()</script>"#
    )
}

async fn view(State(ctx): State<Ctx>, headers: HeaderMap, Path((project, file)): Path<(String, String)>, Query(q): Query<Key>) -> Response {
    if !allowed(&ctx, &headers, &q.k) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(path) = find_file(&project, &file) else { return StatusCode::NOT_FOUND.into_response() };
    let kind = kind_of(&file).unwrap_or("html");
    let reload = reload_script(&project, &file, &q.k);
    let title = esc(&file);
    let raw_url = format!("/raw/{project}/{file}?k={}", q.k);
    let page = match kind {
        "html" => {
            let mut html = std::fs::read_to_string(&path).unwrap_or_default();
            match html.rfind("</body>") {
                Some(i) => html.insert_str(i, &reload),
                None => html.push_str(&reload),
            }
            html
        }
        "markdown" => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let mut body = String::new();
            pulldown_cmark::html::push_html(&mut body, pulldown_cmark::Parser::new_ext(&text, pulldown_cmark::Options::all()));
            doc(&title, &body, &reload)
        }
        "mermaid" => {
            let text = esc(&std::fs::read_to_string(&path).unwrap_or_default());
            let body = format!(
                r#"<pre class="mermaid">{text}</pre><script type="module">import m from "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs";m.initialize({{startOnLoad:true,theme:"dark"}});</script>"#
            );
            doc(&title, &body, &reload)
        }
        "pdf" => format!(r#"<!doctype html><title>{title}</title><style>html,body{{margin:0;height:100%}}</style><embed src="{raw_url}" type="application/pdf" width="100%" height="100%">{reload}"#),
        _ => doc(&title, &format!(r#"<img src="{raw_url}" alt="{title}">"#), &reload),
    };
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, SANDBOX),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        page,
    )
        .into_response()
}

fn doc(title: &str, body: &str, reload: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title}</title>
<style>body{{margin:0;padding:24px;background:#111;color:#eee;font:15px/1.6 system-ui,sans-serif;max-width:900px;margin:auto}}
img{{max-width:100%;height:auto;display:block;margin:auto}} pre,code{{font-family:ui-monospace,monospace;background:#1b1b1b}} pre{{padding:12px;overflow:auto}}
a{{color:#8ab4f8}} table{{border-collapse:collapse}} td,th{{border:1px solid #444;padding:4px 8px}}</style></head><body>{body}{reload}</body></html>"#
    )
}

async fn raw(State(ctx): State<Ctx>, headers: HeaderMap, Path((project, file)): Path<(String, String)>, Query(q): Query<Key>) -> Response {
    if !allowed(&ctx, &headers, &q.k) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(path) = find_file(&project, &file) else { return StatusCode::NOT_FOUND.into_response() };
    let kind = match kind_of(&file) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        _ => "text/plain; charset=utf-8",
    };
    let bytes = std::fs::read(path).unwrap_or_default();
    ([(header::CONTENT_TYPE, kind), (header::CONTENT_SECURITY_POLICY, SANDBOX), (header::CACHE_CONTROL, "no-cache")], bytes).into_response()
}

async fn mtime(State(ctx): State<Ctx>, headers: HeaderMap, Path((project, file)): Path<(String, String)>, Query(q): Query<Key>) -> Response {
    if !allowed(&ctx, &headers, &q.k) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let t = find_file(&project, &file)
        .and_then(|p| std::fs::metadata(p).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis());
    // The sandboxed page has an opaque origin, so this needs CORS.
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"), (header::CACHE_CONTROL, "no-cache")], t.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(safe_name("chart.html"), Some("chart.html"));
        assert_eq!(safe_name("../x.html"), None);
        assert_eq!(safe_name("a/b.svg"), None);
        assert_eq!(safe_name(".hidden.html"), None);
        assert_eq!(safe_name("notes.txt"), None);
        assert_eq!(kind_of("Diagram.MMD"), Some("mermaid"));
    }
}
