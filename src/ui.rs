//! The window: a project and task sidebar on the left, the selected task's
//! terminal on the right, a header line above and a key-hint line below.

use crate::accounts::{self, Account, Login};
use crate::agents;
use crate::artifacts;
use crate::history::{self, Past};
use crate::bus::{self, Request};
use crate::git;
use crate::notes;
use crate::store::{self, Session, State};
use crate::theme::{self, Theme};
use crate::progress;
use crate::usage;
use crate::limits::{self, Usage};
use crate::remote::{self, Remote};
use crate::workflow::{self, Workflow};
use crate::headless;
use gtk::{gdk, gio, glib, pango, prelude::*};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use vte::prelude::*;

pub const APP_ID: &str = "co.ericstevens.codebench";

const HINTS: &str = "^1-9 projects   alt 1-7 views   alt ↑↓ tasks   ^⇧N new task   ^⇧H handoff   F1 all commands";

/// The views of a project, in the bar under the project tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Tasks,
    Files,
    Notes,
    Workflows,
    Artifacts,
    Git,
    Processes,
}

impl View {
    const ALL: [View; 7] = [View::Tasks, View::Files, View::Notes, View::Workflows, View::Artifacts, View::Git, View::Processes];

    fn glyph(self) -> &'static str {
        match self {
            View::Tasks => "●",
            View::Files => "▤",
            View::Notes => "≡",
            View::Workflows => "▶",
            View::Artifacts => "◆",
            View::Git => "±",
            View::Processes => "⚙\u{fe0e}",
        }
    }

    fn name(self) -> &'static str {
        match self {
            View::Tasks => "tasks",
            View::Files => "files",
            View::Notes => "notes",
            View::Workflows => "workflows",
            View::Artifacts => "artifacts",
            View::Git => "git",
            View::Processes => "processes",
        }
    }
}

/// What a row on a list page does when you pick it.
#[derive(Clone)]
enum PageAct {
    OpenArtifact(String, String),
    Edit(PathBuf),
    NewNote,
    RunWorkflow(Box<Workflow>),
    NewWorkflow,
    OpenUrl(String),
    StopProcess(i32),
    Obsidian,
    LinkNotes,
    Refresh,
}

/// A row on a list page: what it shows, what Enter does, and what the
/// second key (e or x) does.
struct PageItem {
    markup: String,
    act: Option<PageAct>,
    alt: Option<PageAct>,
}

/// Everything Codebench can do, with its key. F1 lists these and runs the
/// one you pick, so a key another program grabs never locks you out.
#[derive(Clone, Copy)]
enum Action {
    NewTask,
    Workflows,
    Notes,
    Handoff,
    AddProject,
    Import,
    Accounts,
    Phone,
    LinkNotes,
    Obsidian,
    Rename,
    Git,
    Merge,
    Browser,
    Split,
    Stop,
    Delete,
    Archived,
    Sidebar,
    Files,
    TopBars,
    Search,
}

const ACTIONS: &[(&str, &str, Action)] = &[
    ("Ctrl+Shift+N", "new task in this project", Action::NewTask),
    ("Ctrl+Shift+P", "workflows: run, edit (^E) or create saved prompts", Action::Workflows),
    ("Ctrl+Shift+E", "notes: brief, handoffs, your own notes (Alt+3)", Action::Notes),
    ("Ctrl+Shift+H", "hand off: write a note, continue in a fresh session", Action::Handoff),
    ("Ctrl+Shift+O", "add a project: new (with git), one of your folders, or browse", Action::AddProject),
    ("Ctrl+Shift+I", "import past Claude and Codex chats for this project", Action::Import),
    ("F2", "accounts: logins, limits, sign in, update agents (also Ctrl+Shift+U)", Action::Accounts),
    ("Ctrl+Shift+Y", "phone access: on or off, address, paired phones", Action::Phone),
    ("Ctrl+Shift+L", "link notes to a folder, e.g. in your Obsidian vault", Action::LinkNotes),
    ("Ctrl+Shift+J", "open the brief in Obsidian", Action::Obsidian),
    ("Ctrl+Shift+R", "rename the open task, or the project if none (right-click a tab to rename it)", Action::Rename),
    ("Ctrl+Shift+G", "git (lazygit) for this task or project", Action::Git),
    ("Ctrl+Shift+F", "browse files (yazi, or your editor) in this task's folder", Action::Files),
    ("Ctrl+Shift+M", "merge a worktree task back into its branch (press twice)", Action::Merge),
    ("Ctrl+Shift+K", "give this project's agents a browser (on or off)", Action::Browser),
    ("Ctrl+Shift+S", "split: pin this task on the right, pick another for the left", Action::Split),
    ("Ctrl+Shift+W", "stop task (select it again to resume)", Action::Stop),
    ("Ctrl+Shift+D", "delete task or remove project", Action::Delete),
    ("Ctrl+Shift+A", "show or hide handed-off tasks", Action::Archived),
    ("Ctrl+Shift+B", "show or hide the sidebar", Action::Sidebar),
    ("Alt+/", "filter this project's tasks by name (Enter opens the first, Esc clears)", Action::Search),
    ("Ctrl+Shift+T", "show or hide the project tabs and view bar", Action::TopBars),
];

/// Keys with no action of their own, listed after the actions.
const OTHER_KEYS: &[(&str, &str)] = &[
    ("Tab", "in the new task box: give the task its own git worktree"),
    ("Ctrl+1 … 9", "switch to project tab 1 to 9 (Ctrl+PgUp / PgDn: previous / next)"),
    ("Alt+1 … 7", "views: tasks, files, notes, workflows, artifacts, git, processes"),
    ("Ctrl+Shift+← / →", "focus the left or right side of a split"),
    ("Ctrl+Shift+C / V", "copy / paste"),
    ("Alt+Up / Down", "previous / next row"),
];

/// Context size at which the sidebar starts warning, in tokens.
const CONTEXT_WARN: u64 = 100_000;
const CONTEXT_HIGH: u64 = 200_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// Not running. Selecting it starts or resumes the agent.
    Dormant,
    Idle,
    Working,
    Waiting,
    /// Waiting, but you have already looked at it.
    WaitingSeen,
    Done,
    Exited,
}

impl Status {
    fn parse(s: &str) -> Option<Status> {
        match s.trim() {
            "working" => Some(Status::Working),
            "waiting" => Some(Status::Waiting),
            "done" => Some(Status::Done),
            "idle" => Some(Status::Idle),
            _ => None,
        }
    }

    /// Glyph, glyph color and trailing word for the sidebar.
    fn look<'a>(self, t: &'a Theme) -> (&'static str, &'a str, &'static str) {
        match self {
            Status::Dormant => ("○", &t.muted, ""),
            Status::Idle => ("●", &t.muted, ""),
            Status::Working => ("●", &t.yellow, "working"),
            Status::Waiting => ("●", &t.red, "needs you"),
            Status::WaitingSeen => ("●", &t.muted, "waiting"),
            Status::Done => ("✓", &t.green, "done"),
            Status::Exited => ("×", &t.muted, "stopped"),
        }
    }

    fn wants_attention(self) -> bool {
        matches!(self, Status::Waiting | Status::Done)
    }

    fn running(self) -> bool {
        !matches!(self, Status::Dormant | Status::Exited)
    }

    /// Whether the agent can take a new message right now.
    fn free(self) -> bool {
        matches!(self, Status::Idle | Status::Done)
    }

    fn name(self) -> &'static str {
        match self {
            Status::Dormant => "not running",
            Status::Idle => "idle",
            Status::Working => "working",
            Status::Waiting => "waiting on the user",
            Status::WaitingSeen => "waiting, seen",
            Status::Done => "done",
            Status::Exited => "stopped",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Row {
    Project(String),
    Notes(String),
    /// The "+ new task" row at the end of the task list.
    NewTask(String),
    Session(String),
}

/// Where new projects are created: ~/code if it exists, else home.
fn projects_root() -> PathBuf {
    let code = store::home().join("code");
    if code.is_dir() { code } else { store::home() }
}

/// A folder name from what was typed: letters, digits, '.', '_' and '-',
/// with spaces turned into '-'.
fn project_folder_name(typed: &str) -> String {
    let name: String = typed
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    name.trim_matches(['.', '-']).to_string()
}

/// A fresh worktree and branch for a task, under Codebench's data folder.
fn make_worktree(project: &store::Project, id: &str, title: &str) -> Result<store::Worktree, String> {
    let short: String = id.chars().take(8).collect();
    let slug = workflow::slug(title);
    let branch = format!("cb/{slug}-{short}");
    let path = store::data_dir().join("worktrees").join(format!("{}-{slug}-{short}", project.name));
    let base = git::add_worktree(&project.path, &path, &branch)?;
    Ok(store::Worktree { path, branch, base })
}

#[derive(Serialize, Deserialize, Clone)]
struct UiSettings {
    sidebar_width: i32,
}

fn ui_settings() -> UiSettings {
    std::fs::read(store::config_dir().join("ui.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(UiSettings { sidebar_width: 270 })
}

fn save_ui_settings(s: &UiSettings) {
    if let Ok(json) = serde_json::to_vec(s) {
        let _ = std::fs::write(store::config_dir().join("ui.json"), json);
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct PhoneSettings {
    enabled: bool,
    port: u16,
}

fn phone_settings() -> PhoneSettings {
    std::fs::read(store::config_dir().join("phone.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(PhoneSettings { enabled: false, port: remote::DEFAULT_PORT })
}

fn save_phone_settings(s: &PhoneSettings) {
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(store::config_dir().join("phone.json"), json);
    }
}

/// The https address `tailscale serve` gives this machine, if it is set up.
fn tailnet_address() -> Option<String> {
    let out = std::process::Command::new("tailscale").args(["serve", "status", "--json"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v["Web"].as_object()?.keys().next().map(|host| format!("https://{}", host.trim_end_matches(":443")))
}

/// Drops blank lines around a screen capture.
fn trim_screen(html: &str) -> String {
    let body = html.trim().strip_prefix("<pre>").unwrap_or(html).strip_suffix("</pre>").unwrap_or(html);
    let body = body.trim_end().trim_start_matches(['\n', '\r']);
    format!("<pre>{body}\n</pre>")
}

/// Where phones reach artifacts: the tailnet https address on port 8443,
/// if `tailscale serve --https=8443` points at the artifact server.
fn artifact_address() -> Option<String> {
    let out = std::process::Command::new("tailscale").args(["serve", "status", "--json"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v["Web"].as_object()?.keys().find(|h| h.ends_with(":8443")).map(|h| format!("https://{h}"))
}

/// Terminal input for a key the phone sends.
fn key_bytes(key: &str) -> Option<&'static [u8]> {
    Some(match key {
        "enter" => b"\r",
        "esc" => b"\x1b",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "tab" => b"\t",
        "shift-tab" => b"\x1b[Z",
        "ctrl-c" => b"\x03",
        "y" => b"y",
        "n" => b"n",
        "1" => b"1",
        "2" => b"2",
        "3" => b"3",
        "4" => b"4",
        _ => return None,
    })
}

/// The phone app's colors and font, from the Omarchy theme.
fn phone_css(t: &Theme) -> String {
    format!(
        ":root {{ --bg: {}; --fg: {}; --accent: {}; --muted: {}; --dark: {}; --light: {}; --selection: {}; --red: {}; --green: {}; --yellow: {}; --font: \"{}\"; }}",
        t.background, t.foreground, t.accent, t.muted, t.dark_background, t.light_background, t.selection, t.red, t.green, t.yellow, t.font_family
    )
}

/// "week (all models)" -> "week", "week (Fable)" -> "Fable week".
fn short_limit(name: &str) -> String {
    match name.split_once(" (") {
        Some((window, rest)) => {
            let what = rest.trim_end_matches(')');
            if what == "all models" { window.to_string() } else { format!("{what} {window}") }
        }
        None => name.to_string(),
    }
}

/// Opens the brief in Obsidian.
fn open_obsidian(notes_dir: &Path) {
    let uri = notes::obsidian_uri(&notes_dir.join("brief.md"));
    let _ = std::process::Command::new("xdg-open").arg(uri).spawn();
}

/// Panes that close themselves when their program exits.
fn transient(key: &str) -> bool {
    key.starts_with("edit:") || key.starts_with("git:") || key.starts_with("auth:")
}

fn notes_key(pid: &str) -> String {
    format!("notes:{pid}")
}

enum PickerMode {
    Closed,
    NewTask,
    Rename(String),
    RenameProject(String),
    NewNote(String),
    Help,
    Workflows(String),
    Accounts,
    Import(String),
    AddProject,
    Phone,
    /// A phone asks to pair: (pairing id).
    Pair(String),
}

#[derive(Clone)]
enum AddChoice {
    /// Create this folder under the projects folder, with git.
    New(String),
    Browse,
    Existing(PathBuf),
}

struct Picker {
    root: gtk::Box,
    title: gtk::Label,
    entry: gtk::Entry,
    list: gtk::ListBox,
    mode: RefCell<PickerMode>,
    /// Agent ids, one per list row, in NewTask mode.
    items: RefCell<Vec<&'static str>>,
    /// Workflow rows in Workflows mode; None is the "new workflow" row.
    workflows: RefCell<Vec<Option<Workflow>>>,
    /// NewTask mode: give the task its own worktree (None if not a repo).
    isolate: Cell<Option<bool>>,
    /// Import mode rows.
    past: RefCell<Vec<Past>>,
    /// AddProject mode rows.
    add_choices: RefCell<Vec<AddChoice>>,
    /// Help mode rows: indexes into ACTIONS.
    actions: RefCell<Vec<usize>>,
}

struct Running {
    term: vte::Terminal,
    pid: Cell<Option<i32>>,
    /// For agents with no status hooks: set when you send a prompt, and
    /// the last time the screen changed after that.
    armed: Cell<bool>,
    last_output: Cell<Instant>,
}

/// Agents whose status Codebench infers from screen activity: working
/// while the screen keeps changing after a prompt, done once it settles.
fn watches_activity(agent: &str) -> bool {
    matches!(agent, "gemini" | "grok" | "opencode" | "local" | "cursor" | "agy")
}

/// Width a task gets when opened from the phone and not on the desktop.
const PHONE_COLUMNS: libc::c_long = 64;

/// How long the screen must stay still before a task counts as done.
const SETTLE: Duration = Duration::from_secs(5);

struct App {
    window: gtk::ApplicationWindow,
    state: RefCell<State>,
    theme: RefCell<Theme>,
    css: gtk::CssProvider,
    sidebar_box: gtk::Box,
    sidebar: gtk::ListBox,
    /// Narrows the task list by name; hidden until used.
    search: gtk::SearchEntry,
    rows: RefCell<Vec<Row>>,
    rebuilding: Cell<bool>,
    show_archived: Cell<bool>,
    /// Tasks whose name is being worked out right now.
    naming: RefCell<std::collections::HashSet<String>>,
    stack: gtk::Stack,
    empty: gtk::Label,
    header_left: gtk::Label,
    header_right: gtk::Label,
    footer: gtk::Label,
    flash_gen: Cell<u32>,
    picker: Picker,
    /// Terminals by key: a session id, or `notes:<project id>`.
    running: RefCell<HashMap<String, Rc<Running>>>,
    status: RefCell<HashMap<String, Status>>,
    /// Holds off sleep and idle while any agent is working.
    awake: RefCell<crate::awake::Awake>,
    context: RefCell<HashMap<String, u64>>,
    /// Sessions asked to write a handoff note, and where.
    handoffs: RefCell<HashMap<String, PathBuf>>,
    /// Messages from other tasks waiting for the agent to be free.
    inbox: RefCell<HashMap<String, Vec<String>>>,
    /// Where to go back to when an editor or git pane closes.
    return_to: RefCell<Option<Row>>,
    /// Recent git answers by query, so redraws do not run git every time.
    git_cache: RefCell<HashMap<String, (Instant, Option<String>)>>,
    pending_merge: RefCell<Option<(String, Instant)>>,
    accounts: RefCell<Vec<Account>>,
    usage: RefCell<Vec<Usage>>,
    agents_label: gtk::Label,
    paned: gtk::Paned,
    side_box: gtk::Box,
    side_header: gtk::Label,
    side_holder: gtk::Box,
    /// The task shown on the right of a split.
    pinned: RefCell<Option<String>>,
    /// A board refresh is queued; later changes ride along with it.
    board_pending: Cell<bool>,
    /// Bumped per board read, so a slow older read never lands last.
    board_gen: Cell<u32>,
    /// The task whose terminal last took keyboard focus.
    focused: RefCell<Option<String>>,
    board: gtk::Label,
    /// The phone server, while phone access is on.
    remote: RefCell<Option<Rc<Remote>>>,
    /// Terminals with a screen capture for the phone already scheduled.
    screen_pending: RefCell<std::collections::HashSet<String>>,
    artifact_server: Option<artifacts::Server>,
    /// Viewer windows by artifact, so showing it again just reloads.
    viewers: RefCell<HashMap<String, std::process::Child>>,
    tabs_box: gtk::Box,
    tabbar: gtk::Box,
    viewbar: gtk::Box,
    view_buttons: Vec<(View, gtk::Button)>,
    view: Cell<View>,
    page_title: gtk::Label,
    page_list: gtk::ListBox,
    page_items: RefCell<Vec<PageItem>>,
    editor: gtk::TextView,
    /// The file open in the notes editor.
    editor_path: RefCell<Option<PathBuf>>,
    /// Bumped on every edit; a save runs once typing pauses.
    editor_gen: Cell<u32>,
    /// Set while loading a file, so loading is not taken as an edit.
    editor_loading: Cell<bool>,
    /// Typed into since the last load or save.
    editor_dirty: Cell<bool>,
    /// The file's text as last loaded or saved, to notice outside edits.
    editor_disk: RefCell<Option<String>>,
    /// The task you last looked at in each project, reopened with its tab.
    last_task: RefCell<HashMap<String, String>>,
    /// A handle on the app itself, for widgets built outside Rc methods.
    me: RefCell<std::rc::Weak<App>>,
    current_project: RefCell<Option<String>>,
    /// Terminal key of the row on screen, if it has one.
    current_key: RefCell<Option<String>>,
    status_dir: PathBuf,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    reload_pending: Cell<bool>,
}

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    let bench: Rc<RefCell<Option<Rc<App>>>> = Rc::default();
    app.connect_command_line(move |app, cmd| {
        let b = bench.borrow().clone();
        let b = b.unwrap_or_else(|| {
            let b = App::build(app);
            *bench.borrow_mut() = Some(b.clone());
            b.watch_state_errors();
            b
        });
        // `codebench <dir>` adds the folder as a project and opens it, also
        // when Codebench is already running.
        let args = cmd.arguments();
        if args.get(1).is_some_and(|a| a == "--task") {
            // `codebench --task <id>`: jump to a task (used by the bar widget).
            if let Some(sid) = args.get(2).map(|a| a.to_string_lossy().into_owned()) {
                b.select(&Row::Session(sid));
            }
        } else if let Some(arg) = args.get(1) {
            let path = PathBuf::from(arg);
            let path = match cmd.cwd() {
                Some(cwd) if path.is_relative() => cwd.join(path),
                _ => path,
            };
            if path.is_dir() {
                b.open_path(&path);
            }
        }
        b.window.present();
        glib::ExitCode::SUCCESS
    });
    app.run()
}

fn esc(s: &str) -> String {
    glib::markup_escape_text(s).to_string()
}

fn rgba(hex: &str) -> gdk::RGBA {
    gdk::RGBA::parse(hex).unwrap_or(gdk::RGBA::new(0.0, 0.0, 0.0, 1.0))
}

fn git_branch(path: &Path) -> Option<String> {
    let head = std::fs::read_to_string(path.join(".git/HEAD")).ok()?;
    match head.trim().strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(branch.to_string()),
        None => Some(head.trim().chars().take(8).collect()),
    }
}

fn tilde(path: &Path) -> String {
    let path = path.display().to_string();
    let home = store::home().display().to_string();
    path.strip_prefix(&home).map(|r| format!("~{r}")).unwrap_or(path)
}

/// Types text into an agent's prompt as a paste, then presses Enter
/// separately so the newline is not taken as part of the paste.
fn type_into(term: &vte::Terminal, text: &str) {
    term.paste_text(text);
    let term = term.clone();
    glib::timeout_add_local_once(Duration::from_millis(250), move || term.feed_child(b"\r"));
}

fn label(class: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.add_css_class(class);
    l
}

/// Claude Code's little pixel mascot, drawn in the theme's colors (FG and BG).
const CLAUDE_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 11 11" shape-rendering="crispEdges"><g fill="FG"><rect x="2" y="1" width="7" height="6"/><rect x="0" y="3" width="11" height="2"/><rect x="2" y="7" width="1" height="2"/><rect x="4" y="7" width="1" height="2"/><rect x="6" y="7" width="1" height="2"/><rect x="8" y="7" width="1" height="2"/></g><g fill="BG"><rect x="3" y="2" width="1" height="2"/><rect x="7" y="2" width="1" height="2"/></g></svg>"##;

/// Codex's cloud with a prompt, drawn in the theme's colors (FG and BG).
const CODEX_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24"><g fill="FG"><circle cx="8" cy="13" r="6"/><circle cx="13" cy="9" r="6.5"/><circle cx="17" cy="14" r="5"/><rect x="8" y="13" width="9" height="6"/></g><g fill="none" stroke="BG" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8 11l3 2.5-3 2.5"/><path d="M13 16h4"/></g></svg>"##;

/// Antigravity's arch, drawn in the theme's colors (FG).
const AGY_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24"><path fill="FG" d="M12 2C7.6 2 5.5 9 3 17c-.8 2.6-.4 4.2 1.2 4.6 1.8.4 2.9-1.2 3.9-3.6C9.3 15 10.4 13 12 13s2.7 2 3.9 5c1 2.4 2.1 4 3.9 3.6 1.6-.4 2-2 1.2-4.6C18.5 9 16.4 2 12 2z"/></svg>"##;

/// A computer monitor for the local model, drawn in the theme's colors (FG and BG).
const LOCAL_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24"><g fill="FG"><rect x="1" y="3" width="22" height="15" rx="2"/><rect x="10" y="18" width="4" height="3"/><rect x="6" y="20" width="12" height="2" rx="1"/></g><rect x="3" y="5" width="18" height="11" fill="BG"/></svg>"##;

/// A small logo for an agent, or None to show its name instead.
fn agent_icon(agent: &str, theme: &Theme) -> Option<gdk::Texture> {
    thread_local! {
        static CACHE: RefCell<HashMap<String, gdk::Texture>> = RefCell::default();
    }
    let svg = match agent {
        "claude" => CLAUDE_ICON,
        "codex" => CODEX_ICON,
        "agy" => AGY_ICON,
        "local" => LOCAL_ICON,
        _ => return None,
    };
    // Both in the text color, so they follow the Omarchy theme.
    let svg = svg.replace("FG", &theme.foreground).replace("BG", &theme.background);
    CACHE.with_borrow_mut(|cache| {
        if let Some(t) = cache.get(&svg) {
            return Some(t.clone());
        }
        let t = gdk::Texture::from_bytes(&glib::Bytes::from(svg.as_bytes())).ok()?;
        cache.insert(svg, t.clone());
        Some(t)
    })
}

/// A task row: status glyph, the agent's logo (or name), then the rest.
fn task_row(glyph: &str, status: Status, agent: &str, rest: &str, theme: &Theme) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let g = gtk::Label::new(None);
    g.set_markup(glyph);
    if status == Status::Working {
        g.add_css_class("cb-pulse");
    }
    line.append(&g);
    let rest = match agent_icon(agent, theme) {
        Some(icon) => {
            let image = gtk::Image::from_paintable(Some(&icon));
            image.set_pixel_size(14);
            image.set_tooltip_text(agents::get(agent).map(|a| a.label));
            line.append(&image);
            rest.to_string()
        }
        None => format!("<span foreground='{}'>{:<8}</span>{rest}", theme.muted, esc(agent)),
    };
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.set_ellipsize(pango::EllipsizeMode::End);
    l.set_markup(&rest);
    line.append(&l);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&line));
    // Finished or asking, and not looked at yet: mark the whole row.
    match status {
        Status::Done => row.add_css_class("cb-done"),
        Status::Waiting => row.add_css_class("cb-needs"),
        _ => {}
    }
    row
}

fn row_with(markup: &str) -> gtk::ListBoxRow {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.set_ellipsize(pango::EllipsizeMode::End);
    l.set_markup(markup);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&l));
    row
}

impl App {
    fn build(app: &gtk::Application) -> Rc<App> {
        let theme = Theme::load();
        let css = gtk::CssProvider::new();
        css.load_from_string(&theme.css());
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_USER,
            );
        }
        // GTK's automatic font rendering places text at fractional pixels, which
        // smears the one-pixel top bar of capitals like E and T across two dim
        // rows. Snap font metrics to whole pixels so strokes land crisply.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
            settings.set_gtk_hint_font_metrics(true);
        }

        let window = gtk::ApplicationWindow::new(app);
        window.set_title(Some("codebench"));
        window.set_default_size(1400, 900);
        // No client-side titlebar: Hyprland draws the border, like a terminal.
        window.set_decorated(false);

        // The tasks list and the main area, with a draggable divider.
        let root = gtk::Paned::new(gtk::Orientation::Horizontal);
        root.add_css_class("cb-root");
        root.set_wide_handle(false);
        root.set_shrink_start_child(false);
        root.set_resize_start_child(false);
        root.set_vexpand(true);
        // Dragging still works; keyboard focus on the divider only drew a
        // highlight bar.
        root.set_focusable(false);

        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.add_css_class("cb-sidebar");
        sidebar_box.set_width_request(160);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("filter tasks"));
        search.add_css_class("cb-search");
        search.set_visible(false);
        sidebar_box.append(&search);
        let sidebar = gtk::ListBox::new();
        sidebar.set_selection_mode(gtk::SelectionMode::Single);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&sidebar));
        sidebar_box.append(&scroller);

        // Project tabs across the top, like tmux windows, with the agents'
        // logins and limits on the right.
        let tabbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabbar.add_css_class("cb-tabs");
        let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        // Many projects scroll sideways instead of widening the window.
        let tabs_scroll = gtk::ScrolledWindow::new();
        tabs_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        tabs_scroll.set_propagate_natural_height(true);
        tabs_scroll.set_hexpand(true);
        tabs_scroll.set_child(Some(&tabs_box));
        let subs = label("cb-dim");
        subs.set_margin_end(10);
        subs.set_ellipsize(pango::EllipsizeMode::End);
        subs.set_max_width_chars(40);
        subs.set_text(&format!(
            "agents: {}",
            agents::installed()
                .iter()
                .filter(|a| a.id != "shell")
                .map(|a| a.id)
                .collect::<Vec<_>>()
                .join(" ")
        ));
        tabbar.append(&tabs_scroll);
        tabbar.append(&subs.clone());

        // The current project's views.
        let viewbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        viewbar.add_css_class("cb-views");
        let mut view_buttons = Vec::new();
        for (i, v) in View::ALL.iter().enumerate() {
            let l = gtk::Label::new(None);
            l.set_markup(&format!("{} {}", v.glyph(), v.name()));
            let b = gtk::Button::new();
            b.set_tooltip_text(Some(&format!("Alt+{}", i + 1)));
            b.set_child(Some(&l));
            b.set_has_frame(false);
            b.set_focus_on_click(false);
            b.add_css_class("cb-view");
            viewbar.append(&b);
            view_buttons.push((*v, b));
        }

        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.set_hexpand(true);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.add_css_class("cb-header");
        let header_left = gtk::Label::new(None);
        header_left.set_xalign(0.0);
        header_left.set_hexpand(true);
        header_left.set_ellipsize(pango::EllipsizeMode::End);
        let header_right = label("cb-dim");
        header_right.set_ellipsize(pango::EllipsizeMode::Start);
        header.append(&header_left);
        header.append(&header_right);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        let empty = gtk::Label::new(None);
        empty.add_css_class("cb-empty");
        empty.set_justify(gtk::Justification::Center);
        stack.add_named(&empty, Some("empty"));

        // The project page: a left-aligned board that scrolls.
        let board = gtk::Label::new(None);
        board.set_xalign(0.0);
        board.set_yalign(0.0);
        board.set_wrap(true);
        board.set_wrap_mode(pango::WrapMode::WordChar);
        board.set_selectable(false);
        board.add_css_class("cb-board");
        let board_scroll = gtk::ScrolledWindow::new();
        board_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        board_scroll.set_child(Some(&board));
        stack.add_named(&board_scroll, Some("project"));

        // List pages (notes, workflows, artifacts, processes): a title line
        // and rows; Enter or a click picks one.
        let page_title = label("cb-page-title");
        page_title.set_wrap(true);
        let page_list = gtk::ListBox::new();
        page_list.set_selection_mode(gtk::SelectionMode::Single);
        page_list.add_css_class("cb-page");
        let page_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page_box.append(&page_title);
        page_box.append(&page_list);
        let page_scroll = gtk::ScrolledWindow::new();
        page_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        page_scroll.set_child(Some(&page_box));
        stack.add_named(&page_scroll, Some("page"));

        // The notes editor: plain text, saved as you type.
        let editor = gtk::TextView::new();
        editor.set_wrap_mode(gtk::WrapMode::WordChar);
        editor.set_monospace(true);
        editor.add_css_class("cb-editor");
        editor.set_left_margin(24);
        editor.set_right_margin(24);
        editor.set_top_margin(16);
        editor.set_bottom_margin(16);
        let buffer = editor.buffer();
        buffer.create_tag(Some("heading"), &[("weight", &700i32)]);
        buffer.create_tag(Some("mark"), &[]);
        let editor_scroll = gtk::ScrolledWindow::new();
        editor_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        editor_scroll.set_child(Some(&editor));
        stack.add_named(&editor_scroll, Some("editor"));

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));

        // Right side of a split: one pinned task with its own title line.
        let side_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side_box.add_css_class("cb-split");
        let side_header = label("cb-header");
        side_header.set_ellipsize(pango::EllipsizeMode::End);
        let side_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side_holder.set_vexpand(true);
        side_box.append(&side_header);
        side_box.append(&side_holder);
        side_box.set_visible(false);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_wide_handle(false);
        paned.set_start_child(Some(&overlay));
        paned.set_end_child(Some(&side_box));
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_vexpand(true);
        paned.set_focusable(false);
        let picker = Picker::build();
        overlay.add_overlay(&picker.root);

        let footer = label("cb-footer");
        footer.set_text(HINTS);
        footer.set_ellipsize(pango::EllipsizeMode::End);

        main.append(&header);
        main.append(&paned);
        main.append(&footer);
        root.set_start_child(Some(&sidebar_box));
        root.set_end_child(Some(&main));
        let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shell.append(&tabbar.clone());
        shell.append(&viewbar.clone());
        shell.append(&root);
        root.set_position(ui_settings().sidebar_width);
        root.connect_position_notify(|paned| {
            let width = paned.position();
            if width >= 160 {
                save_ui_settings(&UiSettings { sidebar_width: width });
            }
        });
        window.set_child(Some(&shell));

        let status_dir = store::cache_dir().join("status");
        let _ = std::fs::remove_dir_all(&status_dir);
        let _ = std::fs::create_dir_all(&status_dir);

        let bench = Rc::new(App {
            window,
            state: RefCell::new(State::load()),
            theme: RefCell::new(theme),
            css,
            sidebar_box,
            sidebar,
            search,
            rows: RefCell::default(),
            rebuilding: Cell::new(false),
            show_archived: Cell::new(false),
            naming: RefCell::default(),
            stack,
            empty,
            header_left,
            header_right,
            footer,
            flash_gen: Cell::new(0),
            picker,
            running: RefCell::default(),
            status: RefCell::default(),
            awake: RefCell::new(crate::awake::Awake::new()),
            context: RefCell::default(),
            handoffs: RefCell::default(),
            inbox: RefCell::default(),
            return_to: RefCell::default(),
            git_cache: RefCell::default(),
            pending_merge: RefCell::default(),
            accounts: RefCell::default(),
            usage: RefCell::new(limits::load()),
            agents_label: subs,
            paned,
            side_box,
            side_header,
            side_holder,
            pinned: RefCell::default(),
            focused: RefCell::default(),
            board_pending: Cell::new(false),
            board_gen: Cell::new(0),
            artifact_server: artifacts::Server::start().ok(),
            viewers: RefCell::default(),
            tabs_box,
            tabbar,
            viewbar,
            view_buttons,
            view: Cell::new(View::Tasks),
            page_title,
            page_list,
            page_items: RefCell::default(),
            editor,
            editor_path: RefCell::default(),
            editor_gen: Cell::new(0),
            editor_loading: Cell::new(false),
            editor_dirty: Cell::new(false),
            editor_disk: RefCell::default(),
            last_task: RefCell::default(),
            me: RefCell::default(),
            board,
            remote: RefCell::default(),
            screen_pending: RefCell::default(),
            current_project: RefCell::default(),
            current_key: RefCell::default(),
            status_dir,
            monitors: RefCell::default(),
            reload_pending: Cell::new(false),
        });
        *bench.me.borrow_mut() = Rc::downgrade(&bench);
        bench.connect();
        bench.watch();

        // Show how full each resumable Claude task already is.
        let claude_ids: Vec<String> = bench
            .state
            .borrow()
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| s.agent == "claude")
            .map(|s| s.id.clone())
            .collect();
        for sid in claude_ids {
            bench.refresh_context(&sid);
        }

        let first = bench.state.borrow().projects.first().map(|p| p.id.clone());
        bench.rebuild_sidebar();
        match first {
            Some(pid) => bench.select(&Row::Project(pid)),
            None => bench.show_empty(),
        }
        bench.process_requests();
        bench.refresh_accounts();
        if phone_settings().enabled {
            bench.start_remote();
        }

        // Tasks named "task N", or by the old first-words rule, from before.
        let b = bench.clone();
        glib::timeout_add_local_once(Duration::from_secs(2), move || {
            let ids: Vec<String> = b.state.borrow().projects.iter().flat_map(|p| &p.sessions).map(|s| s.id.clone()).collect();
            for id in ids {
                b.name_task(&id);
            }
        });

        // Subscription limits: soon after start, then every ten minutes.
        let b = bench.clone();
        glib::timeout_add_local_once(Duration::from_secs(3), move || b.refresh_usage());
        let b = bench.clone();
        glib::timeout_add_seconds_local(600, move || {
            b.refresh_usage();
            glib::ControlFlow::Continue
        });

        // Tasks whose status comes from screen activity settle into "done".
        let b = bench.clone();
        glib::timeout_add_seconds_local(1, move || {
            let settled: Vec<String> = b
                .running
                .borrow()
                .iter()
                .filter(|(_, r)| r.armed.get() && r.last_output.get().elapsed() >= SETTLE)
                .map(|(k, r)| {
                    r.armed.set(false);
                    k.clone()
                })
                .collect();
            for key in settled {
                if b.status_of(&key) == Status::Working {
                    b.set_status(&key, Status::Done);
                }
            }
            b.update_keep_awake();
            glib::ControlFlow::Continue
        });

        // Scheduled workflows run inside the app while it is open.
        headless::write_pid();
        let b = bench.clone();
        glib::timeout_add_seconds_local(60, move || {
            b.run_due_workflows();
            glib::ControlFlow::Continue
        });
        let b = bench.clone();
        glib::timeout_add_local_once(Duration::from_secs(5), move || b.run_due_workflows());
        bench
    }

    fn connect(self: &Rc<Self>) {
        let b = self.clone();
        self.sidebar.connect_row_selected(move |_, row| {
            if b.rebuilding.get() {
                return;
            }
            let Some(row) = row else { return };
            let kind = b.rows.borrow().get(row.index() as usize).cloned();
            if let Some(Row::Session(sid)) = kind {
                b.show_session(&sid);
            }
        });
        let b = self.clone();
        self.sidebar.connect_row_activated(move |_, row| {
            let kind = b.rows.borrow().get(row.index() as usize).cloned();
            if let Some(Row::NewTask(_)) = kind {
                b.open_new_task();
            }
        });

        for (view, button) in &self.view_buttons {
            let b = self.clone();
            let view = *view;
            button.connect_clicked(move |_| b.show_view(view));
        }

        let b = self.clone();
        self.page_list.connect_row_activated(move |_, row| {
            let act = b.page_items.borrow().get(row.index() as usize).and_then(|i| i.act.clone());
            if let Some(act) = act {
                b.run_page_act(act);
            }
        });
        let page_keys = gtk::EventControllerKey::new();
        let b = self.clone();
        page_keys.connect_key_pressed(move |_, key, _, mods| {
            if mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            let idx = b.page_list.selected_row().map(|r| r.index()).unwrap_or(-1);
            let alt = b.page_items.borrow().get(idx as usize).and_then(|i| i.alt.clone());
            // Each alternate action answers only to its own documented key,
            // so an edit key can never stop a process.
            let fits = match &alt {
                Some(PageAct::Edit(_)) => key.to_lower() == gdk::Key::e,
                Some(PageAct::StopProcess(_)) => matches!(key.to_lower(), gdk::Key::x | gdk::Key::Delete),
                _ => false,
            };
            match alt.filter(|_| fits) {
                Some(act) => {
                    b.run_page_act(act);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        self.page_list.add_controller(page_keys);

        let b = self.clone();
        self.editor.buffer().connect_changed(move |buf| {
            b.highlight_markdown(buf);
            if b.editor_loading.get() {
                return;
            }
            b.editor_dirty.set(true);
            let generation = b.editor_gen.get().wrapping_add(1);
            b.editor_gen.set(generation);
            let b2 = b.clone();
            glib::timeout_add_local_once(Duration::from_millis(600), move || {
                if b2.editor_gen.get() == generation {
                    b2.save_editor();
                }
            });
        });

        // Drop files on the notes or artifacts view to copy them in.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let b = self.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            b.drop_into_view(files)
        });
        self.stack.add_controller(drop);

        let b = self.clone();
        self.search.connect_search_changed(move |_| b.rebuild_sidebar());
        let b = self.clone();
        self.search.connect_activate(move |_| b.open_first_match());
        let b = self.clone();
        self.search.connect_stop_search(move |_| b.close_search());
        // Down from the box walks the filtered list.
        let search_keys = gtk::EventControllerKey::new();
        let b = self.clone();
        search_keys.connect_key_pressed(move |_, key, _, _| {
            if key != gdk::Key::Down {
                return glib::Propagation::Proceed;
            }
            if let Some(row) = b.sidebar.selected_row().or_else(|| b.sidebar.row_at_index(0)) {
                row.grab_focus();
            }
            glib::Propagation::Stop
        });
        self.search.add_controller(search_keys);
        // "/" in the task list starts filtering too.
        let list_keys = gtk::EventControllerKey::new();
        let b = self.clone();
        list_keys.connect_key_pressed(move |_, key, _, mods| {
            if key == gdk::Key::slash && !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                b.open_search();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.sidebar.add_controller(list_keys);

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let b = self.clone();
        keys.connect_key_pressed(move |_, key, _, mods| b.on_key(key, mods));
        self.window.add_controller(keys);

        // Leaving the editor any way at all saves it and lets go of the file.
        let b = self.clone();
        self.stack.connect_visible_child_name_notify(move |st| {
            if st.visible_child_name().as_deref() != Some("editor") {
                b.close_editor();
            }
        });
        let b = self.clone();
        self.window.connect_close_request(move |_| {
            b.save_editor();
            // Closing stops every task, so nothing is left to stay awake for.
            b.awake.borrow_mut().release();
            glib::Propagation::Proceed
        });

        let b = self.clone();
        self.window.connect_is_active_notify(move |w| {
            if w.is_active() {
                b.mark_seen();
            }
        });

        let b = self.clone();
        self.picker.entry.connect_activate(move |_| b.picker_accept());
        let b = self.clone();
        self.picker.entry.connect_changed(move |_| {
            match &*b.picker.mode.borrow() {
                PickerMode::Workflows(pid) => b.fill_workflows(pid),
                PickerMode::Import(_) => b.fill_import(),
                PickerMode::AddProject => b.fill_add_project(),
                PickerMode::Help => b.fill_help(),
                _ => {}
            }
        });
        let b = self.clone();
        self.picker.list.connect_row_activated(move |_, _| b.picker_accept());
        let picker_keys = gtk::EventControllerKey::new();
        picker_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let b = self.clone();
        picker_keys.connect_key_pressed(move |_, key, _, mods| {
            let list = &b.picker.list;
            if mods.contains(gdk::ModifierType::CONTROL_MASK) && key.to_lower() == gdk::Key::e {
                b.edit_picked_workflow();
                return glib::Propagation::Stop;
            }
            if matches!(*b.picker.mode.borrow(), PickerMode::Accounts) {
                match key.to_lower() {
                    gdk::Key::s => {
                        b.sign_in_picked(true);
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::r => {
                        b.refresh_accounts();
                        b.refresh_usage();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::u => {
                        b.run_account_command(accounts::update_command, "update");
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::i => {
                        b.run_account_command(accounts::install_command, "install");
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            if key == gdk::Key::Tab && matches!(*b.picker.mode.borrow(), PickerMode::NewTask) {
                if let Some(on) = b.picker.isolate.get() {
                    b.picker.isolate.set(Some(!on));
                    b.new_task_title();
                }
                return glib::Propagation::Stop;
            }
            let step = match key {
                gdk::Key::Escape => {
                    b.close_picker();
                    return glib::Propagation::Stop;
                }
                gdk::Key::Up => -1,
                gdk::Key::Down => 1,
                _ => return glib::Propagation::Proceed,
            };
            let idx = list.selected_row().map(|r| r.index()).unwrap_or(0) + step;
            if let Some(row) = list.row_at_index(idx) {
                list.select_row(Some(&row));
            }
            glib::Propagation::Stop
        });
        self.picker.root.add_controller(picker_keys);
    }

    /// Watches the agents' status files and the Omarchy theme.
    fn watch(self: &Rc<Self>) {
        let mut monitors = self.monitors.borrow_mut();

        let dir = gio::File::for_path(&self.status_dir);
        if let Ok(m) = dir.monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            let b = self.clone();
            m.connect_changed(move |_, file, _, event| {
                if !matches!(
                    event,
                    gio::FileMonitorEvent::Created
                        | gio::FileMonitorEvent::Changed
                        | gio::FileMonitorEvent::ChangesDoneHint
                ) {
                    return;
                }
                let (Some(name), Some(path)) = (file.basename(), file.path()) else { return };
                let sid = name.to_string_lossy().into_owned();
                if let Some(status) = std::fs::read_to_string(path).ok().as_deref().and_then(Status::parse) {
                    b.set_status(&sid, status);
                }
            });
            monitors.push(m);
        }

        let requests = bus::dir();
        let _ = std::fs::create_dir_all(&requests);
        if let Ok(m) = gio::File::for_path(&requests).monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            let b = self.clone();
            m.connect_changed(move |_, file, _, event| {
                let is_request = file.path().is_some_and(|p| p.extension().is_some_and(|e| e == "json"));
                if is_request && event == gio::FileMonitorEvent::Created {
                    b.process_requests();
                }
            });
            monitors.push(m);
        }

        for path in [theme::theme_marker(), theme::font_config()] {
            if let Ok(m) = gio::File::for_path(path).monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
                let b = self.clone();
                m.connect_changed(move |_, _, _, _| b.schedule_theme_reload());
                monitors.push(m);
            }
        }
    }

    /// Omarchy writes several theme files in a row, so wait for it to settle.
    fn schedule_theme_reload(self: &Rc<Self>) {
        if self.reload_pending.replace(true) {
            return;
        }
        let b = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            b.reload_pending.set(false);
            *b.theme.borrow_mut() = Theme::load();
            b.css.load_from_string(&b.theme.borrow().css());
            if let Some(r) = b.remote.borrow().as_ref() {
                r.publish_theme(phone_css(&b.theme.borrow()));
            }
            for r in b.running.borrow().values() {
                b.style_terminal(&r.term);
            }
            b.rebuild_sidebar();
            b.update_header();
        });
    }

    /// Gives a task still called "task N", or just the start of its first
    /// prompt, a name that says what it is about. Works in the background.
    fn name_task(self: &Rc<Self>, key: &str) {
        let Some(s) = self.state.borrow().session(key).map(|(_, s)| s.clone()) else { return };
        if !s.launched || !self.naming.borrow_mut().insert(key.to_string()) {
            return;
        }
        let b = self.clone();
        let key = key.to_string();
        glib::spawn_future_local(async move {
            let before = s.title.clone();
            let found = gio::spawn_blocking(move || history::auto_name(&s)).await.ok().flatten();
            b.naming.borrow_mut().remove(&key);
            let Some(title) = found else { return };
            {
                let mut state = b.state.borrow_mut();
                // Renamed by hand meanwhile: leave it.
                let Some(s) = state.session_mut(&key).filter(|s| s.title == before) else { return };
                s.title = title;
                state.save();
            }
            b.rebuild_sidebar();
            if b.current_key.borrow().as_deref() == Some(key.as_str()) {
                b.update_header();
            }
        });
    }

    // ── sidebar ────────────────────────────────────────────────────────────

    fn status_of(&self, key: &str) -> Status {
        self.status.borrow().get(key).copied().unwrap_or(Status::Dormant)
    }

    fn selected_row(&self) -> Option<Row> {
        if let Some(key) = self.current_key.borrow().clone() {
            if transient(&key) {
                return self.current_project.borrow().clone().map(Row::Project);
            }
            if key.starts_with("page:") || key.starts_with("note:") {
                return self.current_project.borrow().clone().map(Row::Project);
            }
            return Some(match key.strip_prefix("notes:") {
                Some(pid) => Row::Notes(pid.to_string()),
                None => Row::Session(key),
            });
        }
        self.current_project.borrow().clone().map(Row::Project)
    }

    /// Redraws the project tabs, the view bar and the current project's
    /// task list, and publishes task status for the widget and the phone.
    fn rebuild_sidebar(&self) {
        self.rebuilding.set(true);
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        while let Some(child) = self.tabs_box.first_child() {
            self.tabs_box.remove(&child);
        }
        let theme = self.theme.borrow();
        let state = self.state.borrow();
        let context = self.context.borrow();
        let show_archived = self.show_archived.get();
        let query = self.search.text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let current = self.current_project.borrow().clone();
        let me = self.me.borrow().upgrade();
        let mut rows = Vec::new();

        // Tabs: number, name, browser mark, and how many tasks need you.
        for (i, p) in state.projects.iter().enumerate() {
            let attention = p.sessions.iter().filter(|s| self.status_of(&s.id).wants_attention()).count();
            let working = p.sessions.iter().filter(|s| self.status_of(&s.id) == Status::Working).count();
            let badge = match (attention, working) {
                (0, 0) => String::new(),
                (0, _) => format!(" <span foreground='{}'>●</span>", theme.yellow),
                (n, _) => format!(" <span foreground='{}'>●{n}</span>", theme.red),
            };
            let web = if p.browser { format!(" <span foreground='{}'>◍</span>", theme.muted) } else { String::new() };
            let number = if i < 9 { format!("<span alpha='50%'>{}</span> ", i + 1) } else { String::new() };
            let l = gtk::Label::new(None);
            l.set_markup(&format!("{number}{}{web}{badge}", esc(&p.name)));
            l.set_ellipsize(pango::EllipsizeMode::End);
            l.set_max_width_chars(24);
            let tab = gtk::Button::new();
            tab.set_child(Some(&l));
            tab.set_tooltip_text(Some(&p.name));
            tab.set_has_frame(false);
            tab.set_focus_on_click(false);
            tab.add_css_class("cb-tab");
            if current.as_deref() == Some(p.id.as_str()) {
                tab.add_css_class("active");
                // Scroll the active tab into view once it is laid out.
                let (t, bx) = (tab.clone(), self.tabs_box.clone());
                glib::idle_add_local_once(move || {
                    let Some(scroll) = bx.parent().and_then(|v| v.parent()).and_downcast::<gtk::ScrolledWindow>() else { return };
                    let Some(r) = t.compute_bounds(&bx) else { return };
                    let adj = scroll.hadjustment();
                    let (x, w) = (r.x() as f64, r.width() as f64);
                    if x < adj.value() {
                        adj.set_value(x);
                    } else if x + w > adj.value() + adj.page_size() {
                        adj.set_value(x + w - adj.page_size());
                    }
                });
            }
            if let Some(me) = me.clone() {
                let (m, pid) = (me.clone(), p.id.clone());
                tab.connect_clicked(move |_| m.switch_project(&pid));
                // Right-click renames the project, whatever task is open.
                let right = gtk::GestureClick::new();
                right.set_button(3);
                let pid = p.id.clone();
                right.connect_pressed(move |_, _, _, _| me.open_rename_project(&pid));
                tab.add_controller(right);
            }
            self.tabs_box.append(&tab);
        }
        let add = gtk::Button::with_label("+");
        add.set_has_frame(false);
        add.set_focus_on_click(false);
        add.add_css_class("cb-tab");
        add.set_tooltip_text(Some("add a project (Ctrl+Shift+O)"));
        if let Some(me) = me.clone() {
            add.connect_clicked(move |_| me.open_add_project());
        }
        self.tabs_box.append(&add);

        let here = current.as_deref().and_then(|id| state.project(id));
        for (i, (view, button)) in self.view_buttons.iter().enumerate() {
            if *view == self.view.get() && current.is_some() {
                button.add_css_class("active");
            } else {
                button.remove_css_class("active");
            }
            // The git and artifacts buttons carry a little status.
            let extra = match (view, here) {
                (View::Git, Some(p)) => match self.git_branch_of(&p.path) {
                    Some(branch) => match self.git_changes(&p.path) {
                        Some(n) if n > 0 => format!(" <span foreground='{}'>{} +{n}</span>", theme.muted, esc(&branch)),
                        _ => format!(" <span foreground='{}'>{}</span>", theme.muted, esc(&branch)),
                    },
                    None => String::new(),
                },
                (View::Artifacts, Some(p)) => match artifacts::list(p).len() {
                    0 => String::new(),
                    n => format!(" <span foreground='{}'>{n}</span>", theme.muted),
                },
                _ => String::new(),
            };
            if let Some(l) = button.child().and_downcast::<gtk::Label>() {
                let _ = i;
                l.set_markup(&format!("{} {}{extra}", view.glyph(), view.name()));
            }
        }

        // The current project's tasks.
        if let Some(p) = current.as_deref().and_then(|id| state.project(id)) {
            // While filtering, handed-off tasks count too: an old task is
            // often the one you are looking for.
            let shown = |s: &&Session| {
                if words.is_empty() {
                    return show_archived || !s.archived;
                }
                let hay = format!("{} {}", s.title, s.agent).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            };
            for s in p.sessions.iter().filter(shown) {
                let status = self.status_of(&s.id);
                let (glyph, color, word) = status.look(&theme);
                let word = if word.is_empty() {
                    String::new()
                } else if status.wants_attention() {
                    format!("  <span foreground='{color}'><b>{word}</b></span>")
                } else {
                    format!("  <span foreground='{color}'>{word}</span>")
                };
                let tokens = context
                    .get(&s.id)
                    .map(|&n| {
                        let c = match n {
                            n if n >= CONTEXT_HIGH => &theme.red,
                            n if n >= CONTEXT_WARN => &theme.yellow,
                            _ => &theme.muted,
                        };
                        format!("  <span foreground='{c}'>{}</span>", usage::short(n))
                    })
                    .unwrap_or_default();
                let waiting_mail = self.inbox.borrow().get(&s.id).map_or(0, Vec::len);
                let tokens = if waiting_mail > 0 {
                    format!("{tokens}  <span foreground='{}'>✉{waiting_mail}</span>", theme.accent)
                } else {
                    tokens
                };
                let title = if s.archived {
                    format!("<span foreground='{}'>{} ↳</span>", theme.muted, esc(&s.title))
                } else {
                    esc(&s.title)
                };
                let title = match &s.worktree {
                    Some(w) => {
                        let dirty = self.git_changes(&w.path).unwrap_or(0);
                        let committed = self.git_ahead(&p.path, w).1;
                        let n = dirty + committed;
                        let count = if n > 0 { format!("{n}") } else { String::new() };
                        format!("{title} <span foreground='{}'>⑂{count}</span>", theme.accent)
                    }
                    None => title,
                };
                self.sidebar.append(&task_row(
                    &format!("<span foreground='{color}'>{glyph}</span>"),
                    status,
                    &s.agent,
                    &format!("{title}{tokens}{word}"),
                    &theme,
                ));
                rows.push(Row::Session(s.id.clone()));
            }
            if words.is_empty() {
                let new = row_with(&format!("<span foreground='{}'>+ new task</span>   <span alpha='50%'>^⇧N</span>", theme.muted));
                new.add_css_class("cb-new");
                self.sidebar.append(&new);
                rows.push(Row::NewTask(p.id.clone()));
            } else if rows.is_empty() {
                let none = row_with(&format!("<span foreground='{}'>no tasks match</span>", theme.muted));
                none.set_selectable(false);
                none.set_activatable(false);
                self.sidebar.append(&none);
                rows.push(Row::NewTask(p.id.clone()));
            }
        }

        let snapshot = state
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .map(|s| (s.id.clone(), self.status_of(&s.id).name()))
            .collect();
        bus::write_snapshot(&snapshot);
        self.publish_phone_state(&state);
        drop((state, theme, context));

        let idx = self.selected_row().and_then(|sel| rows.iter().position(|r| *r == sel));
        *self.rows.borrow_mut() = rows;
        match idx.and_then(|i| self.sidebar.row_at_index(i as i32)) {
            Some(row) => self.sidebar.select_row(Some(&row)),
            None => self.sidebar.unselect_all(),
        }
        self.rebuilding.set(false);
    }

    /// Shows a row: a task opens its terminal (switching tabs if needed);
    /// the other kinds switch to their project's view.
    fn select(&self, target: &Row) {
        let Some(me) = self.me.borrow().upgrade() else { return };
        match target {
            Row::Session(sid) => {
                let Some(pid) = self.state.borrow().session(sid).map(|(p, _)| p.id.clone()) else { return };
                if self.current_project.borrow().as_deref() != Some(pid.as_str()) {
                    *self.current_project.borrow_mut() = Some(pid);
                    *self.current_key.borrow_mut() = Some(sid.clone());
                    self.rebuild_sidebar();
                }
                let idx = self.rows.borrow().iter().position(|r| r == target);
                let already = self.sidebar.selected_row().map(|r| r.index() as usize) == idx;
                match idx.and_then(|i| self.sidebar.row_at_index(i as i32)) {
                    Some(row) if !already => self.sidebar.select_row(Some(&row)),
                    _ => me.show_session(sid),
                }
            }
            Row::Project(pid) => me.switch_project(pid),
            Row::Notes(pid) => {
                *self.current_project.borrow_mut() = Some(pid.clone());
                me.show_view(View::Notes);
            }
            Row::NewTask(_) => {}
        }
    }

    fn open_search(&self) {
        self.sidebar_box.set_visible(true);
        self.search.set_visible(true);
        self.search.grab_focus();
    }

    /// Clears and hides the filter, back to the task you were in.
    fn close_search(&self) {
        let had_text = !self.search.text().is_empty();
        self.search.set_text("");
        self.search.set_visible(false);
        if had_text {
            self.rebuild_sidebar();
        }
        if let Some(t) = self.current_term() {
            t.grab_focus();
        }
    }

    fn open_first_match(self: &Rc<Self>) {
        let first = self.rows.borrow().iter().find_map(|r| match r {
            Row::Session(sid) => Some(sid.clone()),
            _ => None,
        });
        let Some(sid) = first else { return };
        self.search.set_text("");
        self.search.set_visible(false);
        self.rebuild_sidebar();
        self.select(&Row::Session(sid.clone()));
        if let Some(t) = self.running.borrow().get(&sid).map(|r| r.term.clone()) {
            t.grab_focus();
        }
    }

    fn move_selection(&self, delta: i32) {
        let idx = self.sidebar.selected_row().map(|r| r.index()).unwrap_or(-1) + delta;
        if let Some(row) = self.sidebar.row_at_index(idx) {
            let is_task = matches!(self.rows.borrow().get(idx as usize), Some(Row::Session(_)));
            if is_task {
                self.sidebar.select_row(Some(&row));
            }
        }
    }

    /// Switches to a project's tab, back to the task you last had open
    /// there, or its board.
    fn switch_project(self: &Rc<Self>, pid: &str) {
        let last = self.last_task.borrow().get(pid).cloned();
        let last = last.filter(|sid| self.state.borrow().session(sid).is_some_and(|(p, s)| p.id == pid && !s.archived));
        *self.current_project.borrow_mut() = Some(pid.to_string());
        self.view.set(View::Tasks);
        match last {
            Some(sid) => {
                *self.current_key.borrow_mut() = Some(sid.clone());
                self.rebuild_sidebar();
                self.show_session(&sid);
            }
            None => {
                *self.current_key.borrow_mut() = None;
                self.rebuild_sidebar();
                self.show_project(pid);
            }
        }
    }

    fn toggle_top_bars(&self) {
        let show = !self.tabbar.is_visible();
        self.tabbar.set_visible(show);
        self.viewbar.set_visible(show);
    }

    /// Copies files dropped on the notes or artifacts view into that folder.
    fn drop_into_view(self: &Rc<Self>, files: Vec<PathBuf>) -> bool {
        let Some(pid) = self.current_project.borrow().clone() else { return false };
        let Some(p) = self.state.borrow().project(&pid).cloned() else { return false };
        let dest = match self.view.get() {
            View::Notes => notes::ensure(&p),
            View::Artifacts => artifacts::dir(&p),
            _ => return false,
        };
        let _ = std::fs::create_dir_all(&dest);
        let mut copied = 0;
        let mut failed = Vec::new();
        for f in files.iter().filter(|f| f.is_file()) {
            let Some(name) = f.file_name() else { continue };
            // Never over an existing file: a same-named one gets a new name,
            // and the file itself (or an identical copy) is left alone.
            let want = dest.join(name);
            if artifacts::same_file(f, &want) || std::fs::read(&want).ok() == std::fs::read(f).ok() {
                continue;
            }
            match artifacts::copy_file_safely(f, &artifacts::free_name(&want)) {
                Ok(_) => copied += 1,
                Err(e) => failed.push(format!("{}: {e}", name.to_string_lossy())),
            }
        }
        let mut msg = format!("copied {copied} file{} into {}", if copied == 1 { "" } else { "s" }, tilde(&dest));
        if !failed.is_empty() {
            msg.push_str(&format!(" · failed: {}", failed.join(", ")));
        }
        self.flash(&msg);
        let view = self.view.get();
        self.show_view(view);
        copied > 0
    }

    /// Moves to the tab `delta` away from the current one, wrapping.
    fn cycle_project(self: &Rc<Self>, delta: i32) {
        let ids: Vec<String> = self.state.borrow().projects.iter().map(|p| p.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let cur = self.current_project.borrow().clone();
        let i = cur.and_then(|c| ids.iter().position(|x| *x == c)).unwrap_or(0) as i32;
        let n = ids.len() as i32;
        let next = ((i + delta) % n + n) % n;
        self.switch_project(&ids[next as usize]);
    }

    fn project_tab(self: &Rc<Self>, index: usize) {
        let id = self.state.borrow().projects.get(index).map(|p| p.id.clone());
        if let Some(id) = id {
            self.switch_project(&id);
        }
    }

    fn show_view(self: &Rc<Self>, view: View) {
        let Some(pid) = self.current_project.borrow().clone() else {
            self.flash("add a project first (Ctrl+Shift+O)");
            return;
        };
        self.view.set(view);
        match view {
            View::Tasks => {
                *self.current_key.borrow_mut() = None;
                self.show_project(&pid);
            }
            View::Files => {
                *self.current_key.borrow_mut() = None;
                self.open_files();
            }
            View::Git => {
                *self.current_key.borrow_mut() = None;
                self.open_git_here();
            }
            View::Notes => self.show_notes_page(&pid),
            View::Workflows => self.show_workflows_page(&pid),
            View::Artifacts => self.show_artifacts(&pid),
            View::Processes => self.show_processes_page(&pid),
        }
        self.rebuild_sidebar();
    }

    // ── list pages ─────────────────────────────────────────────────────────

    fn show_page(&self, pid: &str, view: View, hint: &str, items: Vec<PageItem>) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        *self.current_project.borrow_mut() = Some(pid.to_string());
        *self.current_key.borrow_mut() = Some(format!("page:{}:{pid}", view.name()));
        let theme = self.theme.borrow();
        self.page_title.set_markup(&format!(
            "<span foreground='{}'><b>{}</b></span>   <span foreground='{}'>{}</span>",
            theme.accent,
            view.name(),
            theme.muted,
            esc(hint)
        ));
        while let Some(child) = self.page_list.first_child() {
            self.page_list.remove(&child);
        }
        for item in &items {
            let l = gtk::Label::new(None);
            l.set_xalign(0.0);
            l.set_ellipsize(pango::EllipsizeMode::End);
            l.set_markup(&item.markup);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&l));
            row.set_activatable(item.act.is_some());
            row.set_selectable(item.act.is_some());
            self.page_list.append(&row);
        }
        *self.page_items.borrow_mut() = items;
        self.stack.set_visible_child_name("page");
        self.header_left.set_markup(&format!(
            "<span foreground='{}'><b>{}</b></span>  <span foreground='{}'>›</span>  {}",
            theme.accent,
            esc(&p.name),
            theme.muted,
            view.name()
        ));
        drop(theme);
        self.header_right.set_text(&tilde(&p.path));
        if let Some(first) = (0..self.page_items.borrow().len() as i32)
            .find(|&i| self.page_items.borrow()[i as usize].act.is_some())
            .and_then(|i| self.page_list.row_at_index(i))
        {
            self.page_list.select_row(Some(&first));
            first.grab_focus();
        }
    }

    fn section(&self, title: &str) -> PageItem {
        PageItem {
            markup: format!("\n<span foreground='{}'><b>{}</b></span>", self.theme.borrow().muted, esc(&title.to_uppercase())),
            act: None,
            alt: None,
        }
    }

    fn run_page_act(self: &Rc<Self>, act: PageAct) {
        let pid = self.current_project.borrow().clone().unwrap_or_default();
        let project = self.state.borrow().project(&pid).cloned();
        match act {
            PageAct::OpenArtifact(pid, file) => self.open_viewer(&pid, &file),
            PageAct::Edit(path) => {
                if let Some(p) = project {
                    self.edit_file(&p, &path);
                }
            }
            PageAct::NewNote => {
                self.picker.fill("new note", Some(("note name", "")), &[]);
                *self.picker.mode.borrow_mut() = PickerMode::NewNote(pid);
                self.picker.show();
            }
            PageAct::RunWorkflow(wf) => self.run_workflow(&pid, &wf, true),
            PageAct::NewWorkflow => self.open_workflows(),
            PageAct::OpenUrl(url) => {
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
            }
            PageAct::StopProcess(p) => {
                crate::processes::stop(p);
                self.flash(&format!("stopped process {p}"));
                let b = self.clone();
                glib::timeout_add_local_once(Duration::from_millis(600), move || {
                    let pid = b.current_project.borrow().clone();
                    if let Some(pid) = pid {
                        b.show_processes_page(&pid);
                    }
                });
            }
            PageAct::Obsidian => self.open_in_obsidian(),
            PageAct::LinkNotes => self.link_notes_dialog(false),
            PageAct::Refresh => {
                let view = self.view.get();
                self.show_view(view);
            }
        }
    }

    fn show_notes_page(self: &Rc<Self>, pid: &str) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        let dir = notes::ensure(&p);
        let theme = self.theme.borrow().clone();
        let md_files = |sub: &Path| -> Vec<(u64, PathBuf)> {
            let mut out: Vec<(u64, PathBuf)> = std::fs::read_dir(sub)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "md"))
                .map(|p| {
                    let t = std::fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                    (t, p)
                })
                .collect();
            out.sort_by(|a, b| b.0.cmp(&a.0));
            out
        };
        let row = |path: &Path, when: u64| PageItem {
            markup: format!(
                "{}   <span foreground='{}'>{}</span>",
                esc(&path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                theme.muted,
                workflow::stamp(when)
            ),
            act: Some(PageAct::Edit(path.to_path_buf())),
            alt: None,
        };
        let mut items = vec![
            PageItem {
                markup: format!("<span foreground='{}'>★</span> brief   <span foreground='{}'>every task reads this</span>", theme.accent, theme.muted),
                act: Some(PageAct::Edit(dir.join("brief.md"))),
                alt: None,
            },
            PageItem { markup: format!("<span foreground='{}'>+ new note</span>", theme.accent), act: Some(PageAct::NewNote), alt: None },
        ];
        let own: Vec<_> = md_files(&dir).into_iter().filter(|(_, p)| p.file_name().is_some_and(|n| n != "brief.md")).collect();
        if !own.is_empty() {
            items.push(self.section("your notes"));
            items.extend(own.iter().map(|(t, p)| row(p, *t)));
        }
        let handoffs = md_files(&dir.join("handoffs"));
        if !handoffs.is_empty() {
            items.push(self.section("handoffs"));
            items.extend(handoffs.iter().map(|(t, p)| row(p, *t)));
        }
        let runs = md_files(&dir.join("runs"));
        if !runs.is_empty() {
            items.push(self.section("workflow runs"));
            items.extend(runs.iter().take(20).map(|(t, p)| row(p, *t)));
        }
        items.push(self.section("obsidian"));
        items.push(if notes::vault_root(&dir).is_some() {
            PageItem { markup: "open the brief in Obsidian".into(), act: Some(PageAct::Obsidian), alt: None }
        } else {
            PageItem { markup: "link these notes into your Obsidian vault".into(), act: Some(PageAct::LinkNotes), alt: None }
        });
        self.show_page(pid, View::Notes, &format!("enter opens it · saves as you type · {}", tilde(&dir)), items);
    }

    fn show_workflows_page(self: &Rc<Self>, pid: &str) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        let theme = self.theme.borrow().clone();
        let mut items = vec![PageItem {
            markup: format!("<span foreground='{}'>+ new workflow</span>", theme.accent),
            act: Some(PageAct::NewWorkflow),
            alt: None,
        }];
        for wf in workflow::list(&p) {
            let when = match (&wf.schedule, &wf.schedule_text) {
                (Some(_), Some(t)) => format!(" · {t}"),
                (None, Some(t)) => format!(" · can't read schedule \"{t}\""),
                _ => String::new(),
            };
            let from = match (&wf.collection, wf.global) {
                (Some(c), _) => format!(" · {c}"),
                (None, true) => " · global".into(),
                _ => String::new(),
            };
            items.push(PageItem {
                markup: format!(
                    "<span foreground='{}'>▶</span> {}   <span foreground='{}'>{}{when}{from}</span>",
                    theme.accent,
                    esc(&wf.name),
                    theme.muted,
                    esc(&wf.agent)
                ),
                act: Some(PageAct::RunWorkflow(Box::new(wf.clone()))),
                alt: Some(PageAct::Edit(wf.path.clone())),
            });
        }
        self.show_page(pid, View::Workflows, "enter runs · e edits", items);
    }

    fn show_processes_page(self: &Rc<Self>, pid: &str) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        let theme = self.theme.borrow().clone();
        let mut roots = vec![p.path.clone()];
        roots.extend(p.sessions.iter().filter_map(|s| s.worktree.as_ref().map(|w| w.path.clone())));
        let servers = crate::processes::in_folders(&roots);
        let mut items = Vec::new();
        if servers.is_empty() {
            items.push(PageItem {
                markup: format!("<span foreground='{}'>nothing from this project is listening on a port</span>", theme.muted),
                act: None,
                alt: None,
            });
        }
        for sv in &servers {
            let ports: Vec<String> = sv.ports.iter().map(|p| format!(":{p}")).collect();
            items.push(PageItem {
                markup: format!(
                    "<span foreground='{}'>●</span> <b>{}</b>   {}   <span foreground='{}'>pid {}</span>",
                    theme.green,
                    ports.join(" "),
                    esc(&crate::processes::short_command(&sv.command, &sv.cwd)),
                    theme.muted,
                    sv.pid
                ),
                act: sv.ports.first().map(|port| PageAct::OpenUrl(format!("http://localhost:{port}"))),
                alt: Some(PageAct::StopProcess(sv.pid)),
            });
        }
        items.push(PageItem { markup: format!("<span foreground='{}'>↻ refresh</span>", theme.muted), act: Some(PageAct::Refresh), alt: None });
        self.show_page(pid, View::Processes, "servers started in this project's folders · enter opens in the browser · x stops", items);
    }

    // ── main area ──────────────────────────────────────────────────────────

    fn show_empty(&self) {
        *self.current_project.borrow_mut() = None;
        *self.current_key.borrow_mut() = None;
        self.empty.set_text(
            "no projects yet\n\n^⇧O  add a project folder\n\nor run  codebench ~/code/<project>  from a terminal",
        );
        self.stack.set_visible_child_name("empty");
        self.update_header();
    }

    /// The project page: a progress board of its tasks (status, plan and
    /// latest message), then its workflows and keys. Plans are read from
    /// transcripts in the background and filled in when ready.
    fn show_project(self: &Rc<Self>, pid: &str) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        *self.current_project.borrow_mut() = Some(pid.to_string());
        *self.current_key.borrow_mut() = None;
        let generation = self.board_gen.get().wrapping_add(1);
        self.board_gen.set(generation);
        // Keep what the board shows until the fresh read lands, so a
        // refresh does not blank plans for a moment.
        if self.stack.visible_child_name().as_deref() != Some("project") {
            self.board.set_markup(&self.board_markup(&p, &HashMap::new()));
        }
        self.stack.set_visible_child_name("project");
        self.update_header();

        let live: Vec<Session> = p.sessions.iter().filter(|s| !s.archived).cloned().collect();
        let b = self.clone();
        let pid = pid.to_string();
        glib::spawn_future_local(async move {
            let Ok(found) = gio::spawn_blocking(move || {
                live.iter()
                    .filter_map(|s| {
                        let progress = match s.agent.as_str() {
                            "claude" => progress::claude(&agents::claude_transcript(&s.id)?),
                            "codex" => progress::codex(&agents::codex_rollout(s)?),
                            _ => return None,
                        };
                        Some((s.id.clone(), progress))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .await
            else {
                return;
            };
            // Only if the page is still showing this project, and no newer
            // read has started since.
            let showing = b.board_gen.get() == generation
                && b.current_key.borrow().is_none()
                && b.current_project.borrow().as_deref() == Some(pid.as_str());
            let project = b.state.borrow().project(&pid).cloned();
            if let (true, Some(project)) = (showing, project) {
                b.board.set_markup(&b.board_markup(&project, &found));
            }
        });
    }

    fn board_markup(&self, p: &store::Project, progress: &HashMap<String, progress::Progress>) -> String {
        let t = self.theme.borrow();
        let dim = |s: &str| format!("<span foreground='{}'>{}</span>", t.muted, esc(s));
        let mut out = format!("<span foreground='{}' size='large'><b>{}</b></span>   {}\n", t.accent, esc(&p.name), dim(&tilde(&p.path)));
        let brief = if notes::brief(p).is_some() { "brief written" } else { "no brief yet (^⇧E)" };
        let brief = if p.browser { format!("{brief}   ·   browser on (^⇧K)") } else { brief.to_string() };
        out.push_str(&format!("{}\n\n", dim(&format!("{brief}   ·   notes in {}", tilde(&p.notes_dir())))));

        let live: Vec<&Session> = p.sessions.iter().filter(|s| !s.archived).collect();
        if live.is_empty() {
            out.push_str(&dim("no tasks yet. ^⇧N starts one, ^⇧I imports past chats"));
            out.push_str("\n");
        }
        for s in live {
            let status = self.status_of(&s.id);
            let (glyph, color, word) = status.look(&t);
            let context = self.context.borrow().get(&s.id).map(|&n| format!("  {}", usage::short(n))).unwrap_or_default();
            out.push_str(&format!(
                "<span foreground='{color}'>{glyph}</span> {} <b>{}</b>  <span foreground='{color}'>{word}</span>{}\n",
                dim(&format!("{:<8}", s.agent)),
                esc(&s.title),
                dim(&context)
            ));
            if let Some(pr) = progress.get(&s.id) {
                const SHOWN: usize = 8;
                let done = pr.steps.iter().filter(|x| x.state == progress::StepState::Done).count();
                if !pr.steps.is_empty() {
                    out.push_str(&format!("   {}\n", dim(&format!("plan {done}/{}", pr.steps.len()))));
                }
                // Keep the unfinished steps in view when the plan is long.
                let skip = pr.steps.len().saturating_sub(SHOWN).min(done);
                for step in pr.steps.iter().skip(skip).take(SHOWN) {
                    let (mark, c) = match step.state {
                        progress::StepState::Done => ("✓", &t.green),
                        progress::StepState::Active => ("◐", &t.yellow),
                        progress::StepState::Pending => ("○", &t.muted),
                    };
                    out.push_str(&format!("   <span foreground='{c}'>{mark}</span> {}\n", esc(&step.text)));
                }
                if let Some(last) = &pr.last {
                    let line = last.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                    let line: String = if line.chars().count() > 140 { line.chars().take(139).chain(['…']).collect() } else { line.to_string() };
                    out.push_str(&format!("   {} {}\n", dim("›"), dim(&line)));
                }
            }
            out.push('\n');
        }

        let flows = workflow::list(p);
        if !flows.is_empty() {
            out.push_str(&format!("<b>workflows</b>  {}\n", dim("^⇧P")));
            for wf in &flows {
                let when = match (&wf.schedule, &wf.schedule_text) {
                    (Some(_), Some(text)) => format!("  ·  {text}"),
                    (None, Some(text)) => format!("  ·  can't read schedule \"{text}\""),
                    _ => String::new(),
                };
                let scope = match (&wf.collection, wf.global) {
                    (Some(c), _) => format!("  ({c})"),
                    (None, true) => "  (global)".to_string(),
                    _ => String::new(),
                };
                out.push_str(&format!("  {}{}\n", esc(&wf.name), dim(&format!("{when}{scope}"))));
            }
            if flows.iter().any(|w| w.schedule.is_some() && w.scheduled_for(p)) && !headless::timer_installed() {
                out.push_str(&dim("  scheduled runs happen while codebench is open; codebench schedule on also runs them while it is closed"));
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&dim("^⇧N new task   ^⇧P workflows   ^⇧E notes   ^⇧I import past chats   ^⇧G git   F1 all commands"));
        out
    }

    /// Shows the terminal for `key`, starting it with `start` if it is not
    /// running.
    /// Shows the terminal for `key`. If it is not running it starts after a
    /// short pause, so arrowing through the sidebar does not launch every
    /// agent on the way.
    fn show_terminal(self: &Rc<Self>, key: &str, start: impl FnOnce(&Rc<Self>) + 'static) {
        self.mark_seen();
        self.update_header();
        if self.status_of(key).running() {
            self.focus_terminal(key);
            return;
        }
        self.empty.set_text("starting…");
        self.stack.set_visible_child_name("empty");
        let b = self.clone();
        let key = key.to_string();
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if b.current_key.borrow().as_deref() != Some(&key) {
                return;
            }
            if !b.status_of(&key).running() {
                start(&b);
            }
            b.focus_terminal(&key);
        });
    }

    fn focus_terminal(&self, key: &str) {
        let term = self.running.borrow().get(key).map(|r| r.term.clone());
        if let Some(term) = term {
            // A pinned task lives on the right; the left keeps its view.
            if self.pinned.borrow().as_deref() != Some(key) {
                self.stack.set_visible_child_name(key);
            }
            term.grab_focus();
        }
    }

    /// Takes a terminal out of whichever side holds it.
    fn detach(&self, key: &str, term: &vte::Terminal) {
        if self.pinned.borrow().as_deref() == Some(key) {
            self.side_holder.remove(term);
            *self.pinned.borrow_mut() = None;
            self.side_box.set_visible(false);
        } else {
            self.stack.remove(term);
        }
    }

    /// Pins the current task to the right half, or unpins it.
    fn toggle_split(self: &Rc<Self>) {
        let pinned = self.pinned.borrow().clone();
        if let Some(key) = pinned {
            let term = self.running.borrow().get(&key).map(|r| r.term.clone());
            *self.pinned.borrow_mut() = None;
            self.side_box.set_visible(false);
            if let Some(term) = term {
                self.side_holder.remove(&term);
                self.stack.add_named(&term, Some(&key));
                if self.current_key.borrow().as_deref() == Some(key.as_str()) {
                    self.focus_terminal(&key);
                }
            }
            self.flash("split closed");
            return;
        }
        let Some(key) = self.current_key.borrow().clone().filter(|k| !transient(k)) else {
            self.flash("select a running task to pin it on the right");
            return;
        };
        let Some(term) = self.running.borrow().get(&key).map(|r| r.term.clone()) else { return };
        self.stack.remove(&term);
        self.side_holder.append(&term);
        *self.pinned.borrow_mut() = Some(key.clone());
        let title = self.state.borrow().session(&key).map_or_else(
            || "notes".to_string(),
            |(p, s)| format!("{} › {}", p.name, s.title),
        );
        self.side_header.set_text(&format!("{title}      ^⇧S unpin"));
        self.side_box.set_visible(true);
        let width = self.paned.width();
        if width > 0 {
            self.paned.set_position(width / 2);
        }
        let pid = self.current_project.borrow().clone();
        if let Some(pid) = pid {
            self.select(&Row::Project(pid.clone()));
            self.show_project(&pid);
        }
        term.grab_focus();
        self.flash("pinned on the right. pick another task for the left side");
    }

    fn show_session(self: &Rc<Self>, sid: &str) {
        let Some(pid) = self.state.borrow().session(sid).map(|(p, _)| p.id.clone()) else { return };
        self.last_task.borrow_mut().insert(pid.clone(), sid.to_string());
        if self.view.get() != View::Tasks {
            self.view.set(View::Tasks);
            for (view, button) in &self.view_buttons {
                if *view == View::Tasks { button.add_css_class("active") } else { button.remove_css_class("active") }
            }
        }
        *self.current_project.borrow_mut() = Some(pid);
        *self.current_key.borrow_mut() = Some(sid.to_string());
        let id = sid.to_string();
        self.show_terminal(sid, move |b| b.launch(&id, None));
    }

    /// The project's notes folder, opened in your editor in a terminal pane.
    fn show_notes(self: &Rc<Self>, pid: &str) {
        let Some(project) = self.state.borrow().project(pid).cloned() else { return };
        *self.current_project.borrow_mut() = Some(pid.to_string());
        let key = notes_key(pid);
        *self.current_key.borrow_mut() = Some(key.clone());
        let k = key.clone();
        self.show_terminal(&key, move |b| {
            let key = k;
            let dir = notes::ensure(&project);
            // $EDITOR may carry arguments (Omarchy sets
            // `omarchy-launch-editor --inline`), so let the shell split it.
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let argv = vec!["sh".to_string(), "-c".to_string(), format!("exec {editor} brief.md")];
            b.spawn(&key, "editor", &argv, &dir, &[]);
        });
    }

    /// Clears "done" and "needs you" on the task you are looking at.
    fn mark_seen(&self) {
        let Some(key) = self.current_key.borrow().clone() else { return };
        if !self.window.is_active() {
            return;
        }
        let seen = match self.status_of(&key) {
            Status::Done => Status::Idle,
            Status::Waiting => Status::WaitingSeen,
            _ => return,
        };
        self.status.borrow_mut().insert(key, seen);
        self.rebuild_sidebar();
    }

    fn update_header(&self) {
        let theme = self.theme.borrow();
        let state = self.state.borrow();
        let row = self.selected_row();
        let project = self.current_project.borrow().clone();
        let project = project.as_deref().and_then(|id| state.project(id));

        let mut left = match project {
            Some(p) => format!("<span foreground='{}'><b>{}</b></span>", theme.accent, esc(&p.name)),
            None => format!("<span foreground='{}'><b>codebench</b></span>", theme.accent),
        };
        let sep = format!("  <span foreground='{}'>›</span>  ", theme.muted);
        match &row {
            Some(Row::Session(sid)) => {
                if let Some((_, s)) = state.session(sid) {
                    let label = agents::get(&s.agent).map(|a| a.label).unwrap_or(&s.agent);
                    left.push_str(&format!(
                        "{sep}{}  <span foreground='{}'>{}</span>",
                        esc(&s.title),
                        theme.muted,
                        esc(label)
                    ));
                }
            }
            Some(Row::Notes(_)) => left.push_str(&format!("{sep}notes")),
            _ => {}
        }
        self.header_left.set_markup(&left);

        let worktree = match &row {
            Some(Row::Session(sid)) => state.session(sid).and_then(|(_, s)| s.worktree.clone()),
            _ => None,
        };
        let right = match (&row, project) {
            (Some(Row::Notes(_)), Some(p)) => tilde(&p.notes_dir()),
            (_, Some(p)) if worktree.is_some() => {
                let w = worktree.unwrap();
                let (commits, files) = self.git_ahead(&p.path, &w);
                let dirty = self.git_changes(&w.path).unwrap_or(0);
                format!(
                    "⑂ {} → {}   {commits} commits, {files} files{}",
                    w.branch,
                    w.base,
                    if dirty > 0 { format!(", {dirty} uncommitted") } else { String::new() }
                )
            }
            (_, Some(p)) => match git_branch(&p.path) {
                Some(branch) => match self.git_changes(&p.path) {
                    Some(n) if n > 0 => format!("{}   {branch}  +{n}", tilde(&p.path)),
                    _ => format!("{}   {branch}", tilde(&p.path)),
                },
                None => tilde(&p.path),
            },
            _ => String::new(),
        };
        self.header_right.set_text(&right);
    }

    fn flash(self: &Rc<Self>, msg: &str) {
        let generation = self.flash_gen.get().wrapping_add(1);
        self.flash_gen.set(generation);
        self.footer.set_text(msg);
        self.footer.add_css_class("cb-flash");
        let b = self.clone();
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            if b.flash_gen.get() == generation {
                b.footer.set_text(HINTS);
                b.footer.remove_css_class("cb-flash");
            }
        });
    }

    // ── terminals ──────────────────────────────────────────────────────────

    fn style_terminal(&self, term: &vte::Terminal) {
        let t = self.theme.borrow();
        let palette: Vec<gdk::RGBA> = t.palette.iter().map(|c| rgba(c)).collect();
        let refs: Vec<&gdk::RGBA> = palette.iter().collect();
        term.set_colors(Some(&rgba(&t.foreground)), Some(&rgba(&t.background)), &refs);
        term.set_color_cursor(Some(&rgba(&t.cursor)));
        term.set_color_highlight(Some(&rgba(&t.selection)));
        term.set_color_highlight_foreground(Some(&rgba(&t.foreground)));
        let mut font = pango::FontDescription::from_string(&t.font_family);
        font.set_size((t.font_size * pango::SCALE as f64) as i32);
        term.set_font(Some(&font));
    }

    fn new_terminal(self: &Rc<Self>, key: &str, agent: &str) -> vte::Terminal {
        let term = vte::Terminal::new();
        term.set_hexpand(true);
        term.set_vexpand(true);
        term.set_scrollback_lines(10_000);
        term.set_cursor_blink_mode(vte::CursorBlinkMode::Off);
        term.set_bold_is_bright(false);
        term.set_mouse_autohide(true);
        term.set_allow_hyperlink(true);
        term.set_scroll_on_keystroke(true);
        let focus = gtk::EventControllerFocus::new();
        let b = self.clone();
        let k = key.to_string();
        focus.connect_enter(move |_| *b.focused.borrow_mut() = Some(k.clone()));
        term.add_controller(focus);

        // Dropping files types their paths in, quoted, like a terminal does.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let t = term.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let paths: Vec<String> = list
                .files()
                .iter()
                .filter_map(|f| f.path())
                .map(|p| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''")))
                .collect();
            if paths.is_empty() {
                return false;
            }
            t.paste_text(&format!("{} ", paths.join(" ")));
            t.grab_focus();
            true
        });
        term.add_controller(drop);
        self.style_terminal(&term);

        let b = self.clone();
        let id = key.to_string();
        term.connect_child_exited(move |_, _| {
            if let Some(r) = b.running.borrow().get(&id) {
                r.pid.set(None);
            }
            // Editor and git panes close themselves and return to where
            // you were.
            if transient(&id) {
                let removed = b.running.borrow_mut().remove(&id);
                if let Some(r) = removed {
                    b.detach(&id, &r.term);
                }
                b.status.borrow_mut().remove(&id);
                b.git_cache.borrow_mut().clear();
                if id.starts_with("auth:") {
                    b.refresh_accounts();
                }
                if b.current_key.borrow().as_deref() == Some(id.as_str()) {
                    b.go_back();
                }
                return;
            }
            b.set_status(&id, Status::Exited);
        });

        // Agents without prompt hooks: a bell means they want you, and for
        // agents that report "done" on their own, sending a line means working.
        if !matches!(agent, "claude" | "shell" | "editor") {
            let b = self.clone();
            let id = key.to_string();
            term.connect_bell(move |_| b.set_status(&id, Status::Waiting));
        }
        if agents::reports_done(agent) {
            let b = self.clone();
            let id = key.to_string();
            term.connect_commit(move |_, text, _| {
                if text.contains('\r') {
                    b.set_status(&id, Status::Working);
                }
            });
        }
        // Feed the screen to a phone that is looking at this task.
        let b = self.clone();
        let id = key.to_string();
        term.connect_contents_changed(move |_| {
            b.schedule_screen(&id);
            b.refresh_board(Duration::from_secs(5));
        });
        term
    }

    /// Runs `argv` in the terminal for `key`, creating the terminal if needed.
    fn spawn(self: &Rc<Self>, key: &str, agent: &str, argv: &[String], cwd: &Path, env: &[String]) {
        let existing = self.running.borrow().get(key).cloned();
        let running = match existing {
            Some(r) => {
                r.term.reset(true, true);
                r
            }
            None => {
                let term = self.new_terminal(key, agent);
                // A terminal that has never been on screen has no size yet,
                // and TUIs draw nothing at zero columns.
                term.set_size(100, 36);
                self.stack.add_named(&term, Some(key));
                let r = Rc::new(Running {
                    term,
                    pid: Cell::new(None),
                    armed: Cell::new(false),
                    last_output: Cell::new(Instant::now()),
                });
                if watches_activity(agent) {
                    let b = self.clone();
                    let id = key.to_string();
                    let run = r.clone();
                    r.term.connect_commit(move |_, text, _| {
                        if text.contains('\r') {
                            run.armed.set(true);
                            run.last_output.set(Instant::now());
                            b.set_status(&id, Status::Working);
                        }
                    });
                    let run = r.clone();
                    r.term.connect_contents_changed(move |_| {
                        if run.armed.get() {
                            run.last_output.set(Instant::now());
                        }
                    });
                }
                self.running.borrow_mut().insert(key.to_string(), r.clone());
                r
            }
        };

        let mut env = env.to_vec();
        env.extend(["TERM=xterm-256color".to_string(), "COLORTERM=truecolor".to_string()]);
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let env_refs: Vec<&str> = env.iter().map(String::as_str).collect();

        let b = self.clone();
        let id = key.to_string();
        let r = running.clone();
        let program = argv[0].clone();
        let cwd = cwd.to_string_lossy().into_owned();
        running.term.spawn_async(
            vte::PtyFlags::DEFAULT,
            Some(&cwd),
            &argv_refs,
            &env_refs,
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            None::<&gio::Cancellable>,
            move |res| match res {
                Ok(pid) => r.pid.set(Some(pid.0)),
                Err(e) => {
                    b.set_status(&id, Status::Exited);
                    b.flash(&format!("could not start {program}: {}", e.message()));
                }
            },
        );
        self.status.borrow_mut().insert(key.to_string(), Status::Idle);
        self.rebuild_sidebar();
    }

    /// Starts the task's agent, resuming its previous conversation if it has
    /// one. `prompt` is sent as the opening message where the agent allows.
    fn launch(self: &Rc<Self>, sid: &str, prompt: Option<&str>) {
        let Some((project, session)) = self
            .state
            .borrow()
            .session(sid)
            .map(|(p, s)| (p.clone(), s.clone()))
        else {
            return;
        };
        notes::ensure(&project);

        let status_file = agents::status_file(&self.status_dir, sid);
        let _ = std::fs::remove_file(&status_file);
        let env = [
            format!("CODEBENCH_STATUS={}", status_file.display()),
            format!("CODEBENCH_SESSION={sid}"),
            format!("CODEBENCH_PROJECT={}", project.path.display()),
            format!("CODEBENCH_NOTES={}", project.notes_dir().display()),
        ];
        let argv = agents::argv(&session, &project, prompt);
        let dir = session.dir(&project);
        if !dir.is_dir() {
            self.flash(&format!("the task's folder is gone: {}", tilde(&dir)));
            return;
        }
        self.spawn(sid, &session.agent, &argv, &dir, &env);

        if let Some(s) = self.state.borrow_mut().session_mut(sid) {
            s.launched = true;
        }
        self.state.borrow().save();
    }

    fn refresh_context(&self, sid: &str) {
        let is_claude = self.state.borrow().session(sid).is_some_and(|(_, s)| s.agent == "claude");
        if is_claude && let Some(n) = usage::claude_context_tokens(sid) {
            self.context.borrow_mut().insert(sid.to_string(), n);
        }
    }

    fn set_status(self: &Rc<Self>, key: &str, status: Status) {
        if status == Status::Done {
            if self.finish_handoff(key) {
                return;
            }
            // The Stop hook can fire before Claude flushes the final message
            // to its transcript, so read the usage again a moment later.
            self.refresh_context(key);
            let b = self.clone();
            let id = key.to_string();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                b.refresh_context(&id);
                b.name_task(&id);
                b.rebuild_sidebar();
            });
        }
        let visible = self.window.is_active() && self.current_key.borrow().as_deref() == Some(key);
        let status = match status {
            Status::Done if visible => Status::Idle,
            Status::Waiting if visible => Status::WaitingSeen,
            s => s,
        };
        let prev = self.status.borrow_mut().insert(key.to_string(), status);
        if prev == Some(status) && status != Status::Idle {
            return;
        }
        if status.wants_attention() && !visible {
            self.notify(key, status);
        }
        if status.wants_attention()
            && let Some(remote) = self.remote.borrow().as_ref()
            && let Some((p, s)) = self.state.borrow().session(key)
        {
            let body = if status == Status::Waiting { "needs you" } else { "finished" };
            remote.notify(&format!("{} · {}", p.name, s.title), body, key);
        }
        if status.free() {
            self.deliver_inbox(key);
        }
        if status == Status::Done || status == Status::Idle {
            // The agent may have changed files; let git counts refresh.
            self.git_cache.borrow_mut().clear();
        }
        self.rebuild_sidebar();
        if self.current_key.borrow().as_deref() == Some(key) {
            self.update_header();
        }
        self.refresh_board(Duration::from_millis(500));
        self.update_keep_awake();
    }

    fn board_showing(&self) -> Option<String> {
        let on_board = self.current_key.borrow().is_none() && self.stack.visible_child_name().as_deref() == Some("project");
        if on_board { self.current_project.borrow().clone() } else { None }
    }

    /// Redraws the project board, if it is showing, once `after` passes.
    /// Changes in the meantime share that one redraw, so a busy terminal
    /// does not rescan transcripts on every line.
    fn refresh_board(self: &Rc<Self>, after: Duration) {
        if self.board_pending.get() || self.board_showing().is_none() {
            return;
        }
        self.board_pending.set(true);
        let b = self.clone();
        glib::timeout_add_local_once(after, move || {
            b.board_pending.set(false);
            if let Some(pid) = b.board_showing() {
                b.show_project(&pid);
            }
        });
    }

    /// While any agent is working, and for a grace period after, the
    /// machine does not sleep or idle-lock. See `awake`.
    fn update_keep_awake(&self) {
        let working = self.status.borrow().values().any(|s| *s == Status::Working);
        self.awake.borrow_mut().update(working);
    }

    fn notify(&self, sid: &str, status: Status) {
        let state = self.state.borrow();
        let Some((p, s)) = state.session(sid) else { return };
        let body = if status == Status::Waiting { "needs you" } else { "finished" };
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "Codebench", &format!("{} · {}", p.name, s.title), body])
            .spawn();
    }

    fn stop(&self, key: &str) {
        if let Some(pid) = self.running.borrow().get(key).and_then(|r| r.pid.get()) {
            unsafe {
                libc::kill(pid, libc::SIGHUP);
            }
        }
    }

    /// The task keys act on: the pinned right side when its terminal has
    /// focus, otherwise the left selection.
    fn focused_key(&self) -> Option<String> {
        let focused = self.focused.borrow().clone();
        if let Some(k) = focused
            && self.pinned.borrow().as_deref() == Some(k.as_str())
        {
            return Some(k);
        }
        self.current_key.borrow().clone()
    }

    fn current_term(&self) -> Option<vte::Terminal> {
        let key = self.focused_key()?;
        self.running.borrow().get(&key).map(|r| r.term.clone())
    }

    // ── handoff ────────────────────────────────────────────────────────────

    /// Asks the current task's agent to write a handoff note. When it
    /// finishes, `finish_handoff` moves the task to a fresh session.
    fn start_handoff(self: &Rc<Self>) {
        let pinned = self.focused_key().filter(|k| self.pinned.borrow().as_deref() == Some(k.as_str()));
        let Some(Row::Session(sid)) = pinned.map(Row::Session).or_else(|| self.selected_row()) else {
            self.flash("select a task to hand off");
            return;
        };
        let Some((project, session)) = self.state.borrow().session(&sid).map(|(p, s)| (p.clone(), s.clone())) else {
            return;
        };
        if !agents::supports_handoff(&session.agent) {
            self.flash(&format!("handoff works with claude and codex tasks, not {}", session.agent));
            return;
        }
        match self.status_of(&sid) {
            Status::Dormant | Status::Exited => {
                self.flash("start the task first (select it), then hand off");
                return;
            }
            Status::Working => {
                self.flash("the agent is still working. hand off when it is done");
                return;
            }
            _ => {}
        }
        let Some(term) = self.current_term() else { return };

        notes::ensure(&project);
        let path = notes::handoff_path(&project, &session);
        let _ = std::fs::remove_file(&path);
        self.handoffs.borrow_mut().insert(sid.clone(), path.clone());

        type_into(&term, &notes::handoff_request(&path));
        self.flash("writing a handoff note. a fresh session opens when it is done");
    }

    /// Returns true if `sid` just finished writing its handoff note, in which
    /// case the task continues in a new session.
    fn finish_handoff(self: &Rc<Self>, sid: &str) -> bool {
        let Some(path) = self.handoffs.borrow().get(sid).cloned() else { return false };
        if !path.is_file() {
            return false;
        }
        self.handoffs.borrow_mut().remove(sid);
        self.stop(sid);

        let new_id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.state.borrow_mut();
            let Some(pid) = state.session(sid).map(|(p, _)| p.id.clone()) else { return false };
            let Some(project) = state.project_mut(&pid) else { return false };
            let Some(idx) = project.sessions.iter().position(|s| s.id == sid) else { return false };
            let old = &mut project.sessions[idx];
            old.archived = true;
            let next = Session {
                id: new_id.clone(),
                agent: old.agent.clone(),
                title: notes::next_title(&old.title),
                created: store::now(),
                launched: false,
                prompt: Some(notes::handoff_resume(&path)),
                archived: false,
                workflow: None,
                worktree: old.worktree.clone(),
                codex_id: None,
            };
            project.sessions.insert(idx + 1, next);
            state.save();
        }
        self.status.borrow_mut().insert(sid.to_string(), Status::Exited);
        self.rebuild_sidebar();
        self.select(&Row::Session(new_id));
        self.flash(&format!("handed off. note saved to {}", tilde(&path)));
        true
    }

    // ── accounts ───────────────────────────────────────────────────────────

    /// Checks every agent's login in the background, then updates the
    /// sidebar footer and the accounts box if it is open.
    fn refresh_accounts(self: &Rc<Self>) {
        let b = self.clone();
        glib::spawn_future_local(async move {
            let Ok(list) = gio::spawn_blocking(accounts::check_all).await else { return };
            *b.accounts.borrow_mut() = list;
            b.show_accounts_summary();
            if matches!(*b.picker.mode.borrow(), PickerMode::Accounts) {
                b.fill_accounts();
            }
        });
    }

    /// Checks subscription limits in the background.
    fn refresh_usage(self: &Rc<Self>) {
        let b = self.clone();
        glib::spawn_future_local(async move {
            let Ok(found) = gio::spawn_blocking(limits::check_all).await else { return };
            if found.is_empty() {
                return;
            }
            // Keep the last known numbers for an agent that did not answer.
            let mut all = b.usage.borrow().clone();
            for u in found {
                all.retain(|x| x.agent != u.agent);
                all.push(u);
            }
            limits::save(&all);
            *b.usage.borrow_mut() = all;
            b.show_accounts_summary();
            if matches!(*b.picker.mode.borrow(), PickerMode::Accounts) {
                b.fill_accounts();
            }
        });
    }

    fn usage_of(&self, agent: &str) -> Option<Usage> {
        self.usage.borrow().iter().find(|u| u.agent == agent).cloned()
    }

    fn show_accounts_summary(&self) {
        let theme = self.theme.borrow();
        let tint = |pct: u32| match pct {
            p if p >= 80 => theme.red.clone(),
            p if p >= 60 => theme.yellow.clone(),
            _ => theme.muted.clone(),
        };
        let parts: Vec<String> = self
            .accounts
            .borrow()
            .iter()
            // Signed-out agents are not in use, and local has no limits.
            .filter(|a| a.installed && a.agent != "local" && !matches!(a.login, Login::Out))
            .map(|a| match a.login {
                Login::In(_) => {
                    let used = self
                        .usage_of(a.agent)
                        .and_then(|u| u.worst().map(|l| l.percent))
                        .map(|p| format!(" <span foreground='{}'>{p}%</span>", tint(p)))
                        .unwrap_or_default();
                    format!("{} <span foreground='{}'>✓</span>{used}", a.agent, theme.green)
                }
                _ => format!("{} ?", a.agent),
            })
            .collect();
        self.agents_label.set_markup(&format!("{}   F2", parts.join("  ")));
    }

    fn open_accounts(self: &Rc<Self>) {
        self.picker.fill(
            "accounts   enter sign in · s switch · u update · i install · r refresh",
            None,
            &["checking…".to_string()],
        );
        *self.picker.mode.borrow_mut() = PickerMode::Accounts;
        self.fill_accounts();
        self.picker.show();
        self.refresh_accounts();
    }

    fn fill_accounts(&self) {
        let rows: Vec<String> = self
            .accounts
            .borrow()
            .iter()
            .map(|a| {
                let login = match &a.login {
                    _ if !a.installed => "- not installed (i to install)".to_string(),
                    Login::In(detail) => format!("✓ {detail}"),
                    Login::Out => "✗ signed out".to_string(),
                    Login::Unknown(why) => format!("? {why}"),
                };
                let update = a.update.as_ref().map(|(cur, new)| format!("   ↑ {cur} → {new}")).unwrap_or_default();
                let used = self
                    .usage_of(a.agent)
                    .map(|u| {
                        let parts: Vec<String> = u.limits.iter().map(|l| format!("{} {}%", short_limit(&l.name), l.percent)).collect();
                        format!("   ·  {}", parts.join(", "))
                    })
                    .unwrap_or_default();
                format!("{:<10}{login}{used}{update}", a.agent)
            })
            .collect();
        if !rows.is_empty() {
            let selected = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0);
            self.picker.set_options(&rows);
            if let Some(row) = self.picker.list.row_at_index(selected) {
                self.picker.list.select_row(Some(&row));
                row.grab_focus();
            }
        }
    }

    /// Runs an update or install command for the picked agent in a pane.
    fn run_account_command(self: &Rc<Self>, command: fn(&str) -> Option<String>, what: &str) {
        let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
        let Some(agent) = self.accounts.borrow().get(idx).map(|a| a.agent) else { return };
        match command(agent) {
            Some(cmd) => self.run_in_auth_pane(agent, &format!("{cmd}; echo; echo 'done. press enter to close'; read _"), what),
            None => self.flash(&format!("codebench does not know how to {what} {agent} here")),
        }
    }

    /// Runs the picked agent's sign-in (or sign-out and sign-in) in a pane.
    fn sign_in_picked(self: &Rc<Self>, switch: bool) {
        let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
        let Some(agent) = self.accounts.borrow().get(idx).map(|a| a.agent) else { return };
        if !self.accounts.borrow().get(idx).is_some_and(|a| a.installed) {
            self.flash(&format!("{agent} is not installed. press i to install it"));
            return;
        }
        let Some(cmd) = accounts::login_command(agent, switch) else { return };
        self.run_in_auth_pane(agent, &cmd, if switch { "switch account" } else { "sign in" });
    }

    /// A self-closing pane for account and install commands.
    fn run_in_auth_pane(self: &Rc<Self>, agent: &str, cmd: &str, what: &str) {
        *self.picker.mode.borrow_mut() = PickerMode::Closed;
        self.picker.root.set_visible(false);
        let key = format!("auth:{agent}");
        *self.return_to.borrow_mut() = self.selected_row();
        *self.current_key.borrow_mut() = Some(key.clone());
        let argv = vec!["sh".to_string(), "-c".to_string(), cmd.to_string()];
        self.spawn(&key, "auth", &argv, &store::home(), &[]);
        self.focus_terminal(&key);
        self.header_left.set_markup(&format!(
            "<span foreground='{}'><b>{agent}</b></span>  {what}",
            self.theme.borrow().accent,
        ));
        self.header_right.set_text("returns when done");
    }

    // ── import ─────────────────────────────────────────────────────────────

    fn open_import(self: &Rc<Self>) {
        let Some(pid) = self.current_project.borrow().clone() else {
            self.flash("select a project first");
            return;
        };
        let Some(project) = self.state.borrow().project(&pid).cloned() else { return };
        self.picker.fill(
            &format!("import past chats into {}   enter import · esc close", project.name),
            Some(("filter", "")),
            &["looking…".to_string()],
        );
        self.picker.past.borrow_mut().clear();
        *self.picker.mode.borrow_mut() = PickerMode::Import(pid);
        self.picker.show();

        let known: Vec<String> = project
            .sessions
            .iter()
            .flat_map(|s| [Some(s.id.clone()), s.codex_id.clone()])
            .flatten()
            .collect();
        let path = project.path.clone();
        let b = self.clone();
        glib::spawn_future_local(async move {
            let Ok(found) = gio::spawn_blocking(move || history::find(&path, &known)).await else { return };
            *b.picker.past.borrow_mut() = found;
            if matches!(*b.picker.mode.borrow(), PickerMode::Import(_)) {
                b.fill_import();
            }
        });
    }

    /// The import rows matching the filter, in the order they are shown.
    fn import_rows(&self) -> Vec<Past> {
        let filter = self.picker.entry.text().trim().to_lowercase();
        self.picker
            .past
            .borrow()
            .iter()
            .filter(|p| filter.is_empty() || p.title.to_lowercase().contains(&filter))
            .cloned()
            .collect()
    }

    fn fill_import(&self) {
        let rows: Vec<String> = self
            .import_rows()
            .iter()
            .map(|p| format!("{:<8}{}  {}", p.agent, workflow::stamp(p.updated), p.title))
            .collect();
        if rows.is_empty() {
            self.picker.set_options(&["no past chats found for this folder".to_string()]);
        } else {
            self.picker.set_options(&rows);
        }
    }

    fn import_picked(self: &Rc<Self>, pid: &str) {
        let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
        let Some(past) = self.import_rows().get(idx).cloned() else { return };
        let session = Session {
            id: if past.agent == "claude" { past.id.clone() } else { uuid::Uuid::new_v4().to_string() },
            agent: past.agent.to_string(),
            title: past.title.clone(),
            created: past.updated,
            launched: true,
            prompt: None,
            archived: false,
            workflow: None,
            worktree: None,
            codex_id: (past.agent == "codex").then(|| past.id.clone()),
        };
        let sid = session.id.clone();
        {
            let mut state = self.state.borrow_mut();
            let Some(project) = state.project_mut(pid) else { return };
            project.sessions.push(session);
            state.save();
        }
        self.picker.past.borrow_mut().retain(|p| p.id != past.id);
        self.refresh_context(&sid);
        self.rebuild_sidebar();
        self.fill_import();
        self.flash(&format!("imported \"{}\". pick more, or esc", past.title));
    }

    // ── artifacts ──────────────────────────────────────────────────────────

    fn show_artifacts(self: &Rc<Self>, pid: &str) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        let theme = self.theme.borrow().clone();
        let list = artifacts::list(&p);
        let mut items: Vec<PageItem> = list
            .iter()
            .map(|a| PageItem {
                markup: format!(
                    "<span foreground='{}'>◆</span> {}   <span foreground='{}'>{} · {}</span>",
                    theme.accent,
                    esc(&a.title),
                    theme.muted,
                    a.kind,
                    workflow::stamp(a.modified)
                ),
                act: Some(PageAct::OpenArtifact(pid.to_string(), a.file.clone())),
                alt: None,
            })
            .collect();
        if items.is_empty() {
            items.push(PageItem {
                markup: format!(
                    "<span foreground='{}'>no artifacts yet. ask an agent to show you something (a chart, a page, a diagram) and it appears here.</span>",
                    theme.muted
                ),
                act: None,
                alt: None,
            });
        }
        self.show_page(pid, View::Artifacts, &format!("enter opens the viewer · {}", tilde(&artifacts::dir(&p))), items);
    }

    /// Opens an artifact in its own window next to Codebench (a Chromium
    /// app window when available). An open viewer reloads by itself, so
    /// showing the same artifact again does not open a second one.
    fn open_viewer(self: &Rc<Self>, pid: &str, file: &str) {
        let Some(server) = self.artifact_server.as_ref() else {
            self.flash("the artifact viewer could not start (port in use?)");
            return;
        };
        let url = server.local_url(pid, file);
        let key = format!("{pid}/{file}");
        let open = self.viewers.borrow_mut().get_mut(&key).is_some_and(|c| c.try_wait().ok().flatten().is_none());
        if open {
            return;
        }
        let child = match agents::chromium() {
            Some(chrome) => std::process::Command::new(chrome)
                .arg(format!("--app={url}"))
                .arg(format!("--user-data-dir={}", store::cache_dir().join("viewer").display()))
                .args(["--no-first-run", "--no-default-browser-check", "--new-window"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn(),
            None => std::process::Command::new("xdg-open").arg(&url).spawn(),
        };
        match child {
            Ok(c) => {
                self.viewers.borrow_mut().insert(key, c);
            }
            Err(e) => self.flash(&format!("could not open the viewer: {e}")),
        }
        if self.current_key.borrow().as_deref() == Some(&format!("page:artifacts:{pid}")) {
            self.show_artifacts(pid);
        }
    }

    // ── phone ──────────────────────────────────────────────────────────────

    fn start_remote(self: &Rc<Self>) {
        let port = phone_settings().port;
        let remote = match Remote::start(port) {
            Ok(r) => Rc::new(r),
            Err(e) => {
                self.flash(&format!("phone access could not start: {e}"));
                return;
            }
        };
        remote.publish_theme(phone_css(&self.theme.borrow()));
        if let Some(server) = self.artifact_server.as_ref() {
            remote.set_artifacts(artifact_address(), server.key.clone());
        }
        let commands = remote.commands.clone();
        *self.remote.borrow_mut() = Some(remote);
        self.rebuild_sidebar();
        let b = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(cmd) = commands.recv().await {
                b.handle_phone(cmd);
            }
        });
    }

    fn stop_remote(&self) {
        // Dropping the handle shuts the server down.
        self.remote.borrow_mut().take();
    }

    fn publish_phone_state(&self, state: &State) {
        let Some(remote) = self.remote.borrow().clone() else { return };
        let kind = |sid: &str| match self.status_of(sid) {
            Status::Waiting | Status::Done => "needs",
            Status::Working | Status::WaitingSeen => "working",
            Status::Idle => "idle",
            Status::Dormant | Status::Exited => "stopped",
        };
        let context = self.context.borrow();
        let projects: Vec<serde_json::Value> = state
            .projects
            .iter()
            .map(|p| {
                let tasks: Vec<serde_json::Value> = p
                    .sessions
                    .iter()
                    // The phone never gets a plain shell.
                    .filter(|s| !s.archived && s.agent != "shell")
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id, "title": s.title, "agent": s.agent, "kind": kind(&s.id),
                            "context": context.get(&s.id).map(|&n| usage::short(n)),
                        })
                    })
                    .collect();
                serde_json::json!({ "id": p.id, "name": p.name, "tasks": tasks })
            })
            .collect();
        let usage: Vec<serde_json::Value> = self
            .usage
            .borrow()
            .iter()
            .map(|u| {
                let limits: Vec<serde_json::Value> = u
                    .limits
                    .iter()
                    .map(|l| serde_json::json!({ "name": short_limit(&l.name), "percent": l.percent }))
                    .collect();
                serde_json::json!({ "agent": u.agent, "limits": limits })
            })
            .collect();
        let agents: Vec<&str> = agents::installed().iter().filter(|a| a.id != "shell").map(|a| a.id).collect();
        let json = serde_json::json!({ "running": true, "projects": projects, "usage": usage, "agents": agents });
        remote.publish_state(json.to_string());
    }

    /// Sends a terminal's screen to the phone at most five times a second.
    fn schedule_screen(self: &Rc<Self>, key: &str) {
        let watched = self.remote.borrow().as_ref().is_some_and(|r| r.is_watched(key));
        if !watched || !self.screen_pending.borrow_mut().insert(key.to_string()) {
            return;
        }
        let b = self.clone();
        let key = key.to_string();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            b.screen_pending.borrow_mut().remove(&key);
            b.send_screen(&key);
        });
    }

    fn send_screen(&self, key: &str) {
        let term = self.running.borrow().get(key).map(|r| r.term.clone());
        let (Some(term), Some(remote)) = (term, self.remote.borrow().clone()) else { return };
        // The screen plus two screens of history, by row number, so it works
        // for tasks that were never shown on the desktop too.
        // Row numbers count the whole scrollback. The scroll position is not
        // kept for a terminal never shown, so anchor on the cursor, which
        // is always on the live screen.
        let rows = term.row_count();
        let cols = term.column_count();
        let (_, cursor_row) = term.cursor_position();
        let end = (cursor_row + rows).max(rows);
        let start = (end - rows * 3).max(0);
        let (html, _) = term.text_range_format(vte::Format::Html, start, 0, end - 1, cols);
        let html = trim_screen(&html.map(|h| h.to_string()).unwrap_or_default());
        remote.publish_screen(key, html);
    }

    /// Only agent tasks take input from the phone: never a shell, editor,
    /// git or sign-in pane.
    fn phone_task(&self, sid: &str) -> Option<Session> {
        self.state.borrow().session(sid).map(|(_, s)| s.clone()).filter(|s| s.agent != "shell")
    }

    fn handle_phone(self: &Rc<Self>, cmd: remote::Command) {
        match cmd {
            remote::Command::PairRequest { id, code, name } => {
                self.picker.fill(
                    &format!("pair phone \"{name}\"?   it shows {} {}", &code[..3], &code[3..]),
                    None,
                    &["enter: approve   ·   esc: deny".to_string()],
                );
                *self.picker.mode.borrow_mut() = PickerMode::Pair(id);
                self.picker.show();
                self.window.present();
                let _ = std::process::Command::new("notify-send")
                    .args(["-a", "Codebench", "Phone wants to pair", &format!("{name} shows {code}. Approve it in Codebench.")])
                    .spawn();
            }
            remote::Command::Send { task, text } => {
                if self.phone_task(&task).is_some() {
                    self.deliver_text(&task, &text);
                }
            }
            remote::Command::Keys { task, keys } => {
                if self.phone_task(&task).is_none() || !self.status_of(&task).running() {
                    return;
                }
                let term = self.running.borrow().get(&task).map(|r| r.term.clone());
                if let Some(term) = term {
                    for key in keys {
                        if let Some(bytes) = key_bytes(&key) {
                            term.feed_child(bytes);
                        }
                    }
                }
            }
            remote::Command::Open { task } => {
                if self.phone_task(&task).is_none() {
                    return;
                }
                if !self.status_of(&task).running() {
                    self.launch(&task, None);
                }
                // Not on the desktop screen: lay it out for a phone.
                let shown = self.stack.visible_child_name().as_deref() == Some(task.as_str())
                    || self.pinned.borrow().as_deref() == Some(task.as_str());
                if !shown && let Some(r) = self.running.borrow().get(&task) {
                    r.term.set_size(PHONE_COLUMNS, 40);
                }
                let b = self.clone();
                glib::timeout_add_local_once(Duration::from_millis(600), move || b.send_screen(&task));
            }
            remote::Command::RunWorkflow { project, path } => {
                // Only a workflow the project actually lists, never an
                // arbitrary file.
                let Some(p) = self.state.borrow().project(&project).cloned() else { return };
                let wf = workflow::list(&p).into_iter().find(|w| w.path == path);
                if let Some(wf) = wf {
                    self.run_workflow(&project, &wf, false);
                    self.flash(&format!("phone started workflow: {}", wf.name));
                }
            }
            remote::Command::NewTask { project, agent, title, prompt } => {
                if agents::get(&agent).is_none_or(|a| a.id == "shell") || !agents::installed().iter().any(|a| a.id == agent) {
                    return;
                }
                let id = uuid::Uuid::new_v4().to_string();
                {
                    let mut state = self.state.borrow_mut();
                    let Some(p) = state.project_mut(&project) else { return };
                    let title = if title.is_empty() { format!("task {}", p.sessions.len() + 1) } else { title };
                    p.sessions.push(Session {
                        id: id.clone(),
                        agent,
                        title,
                        created: store::now(),
                        launched: false,
                        prompt: Some(prompt),
                        archived: false,
                        workflow: None,
                        worktree: None,
                        codex_id: None,
                    });
                    state.save();
                }
                self.launch(&id, None);
                self.rebuild_sidebar();
            }
        }
    }

    fn open_phone_panel(self: &Rc<Self>) {
        self.picker.fill("phone access   enter toggles or revokes · esc close", None, &[]);
        *self.picker.mode.borrow_mut() = PickerMode::Phone;
        self.fill_phone_panel();
        self.picker.show();
    }

    fn fill_phone_panel(&self) {
        let on = self.remote.borrow().is_some();
        let port = phone_settings().port;
        let mut rows = vec![
            format!("phone access: {}   (enter to turn {})", if on { "on" } else { "off" }, if on { "off" } else { "on" }),
            format!("address: {}", tailnet_address().unwrap_or_else(|| format!("run  tailscale serve --bg {port}  once"))),
        ];
        for d in self.phone_devices() {
            rows.push(format!("revoke  {}   paired {}", d.name, workflow::stamp(d.paired)));
        }
        self.picker.set_options(&rows);
    }

    fn phone_devices(&self) -> Vec<remote::Device> {
        match self.remote.borrow().as_ref() {
            Some(r) => r.devices(),
            None => remote::load_devices(),
        }
    }

    fn phone_panel_pick(self: &Rc<Self>, idx: usize) {
        match idx {
            0 => {
                let on = self.remote.borrow().is_none();
                if on {
                    self.start_remote();
                } else {
                    self.stop_remote();
                }
                save_phone_settings(&PhoneSettings { enabled: on, ..phone_settings() });
                self.flash(if on { "phone access on" } else { "phone access off" });
            }
            1 => {}
            n => {
                if let Some(d) = self.phone_devices().get(n - 2) {
                    match self.remote.borrow().as_ref() {
                        Some(r) => r.revoke(&d.token_sha256),
                        None => remote::revoke_device(&d.token_sha256),
                    }
                    self.flash(&format!("revoked {}", d.name));
                }
            }
        }
        self.fill_phone_panel();
    }

    // ── git ────────────────────────────────────────────────────────────────

    /// Runs a git query at most every few seconds per key.
    fn git_cached(&self, key: String, query: impl FnOnce() -> Option<String>) -> Option<String> {
        if let Some((at, value)) = self.git_cache.borrow().get(&key)
            && at.elapsed() < Duration::from_secs(3)
        {
            return value.clone();
        }
        let value = query();
        self.git_cache.borrow_mut().insert(key, (Instant::now(), value.clone()));
        value
    }

    fn git_changes(&self, dir: &Path) -> Option<usize> {
        self.git_cached(format!("changes:{}", dir.display()), || git::changed_files(dir).map(|n| n.to_string()))
            .and_then(|n| n.parse().ok())
    }

    fn git_branch_of(&self, dir: &Path) -> Option<String> {
        self.git_cached(format!("branch:{}", dir.display()), || {
            if git::is_repo(dir) { Some(git::branch(dir).unwrap_or_default()) } else { None }
        })
    }

    /// (commits, files) a worktree branch is ahead of its base.
    fn git_ahead(&self, repo: &Path, w: &store::Worktree) -> (usize, usize) {
        self.git_cached(format!("ahead:{}:{}", w.base, w.branch), || {
            git::ahead(repo, &w.base, &w.branch).map(|(c, f)| format!("{c} {f}"))
        })
        .and_then(|v| {
            let (c, f) = v.split_once(' ')?;
            Some((c.parse().ok()?, f.parse().ok()?))
        })
        .unwrap_or((0, 0))
    }

    /// The directory the selected task works in, or the project's.
    fn current_dir(&self) -> Option<PathBuf> {
        let state = self.state.borrow();
        if let Some(Row::Session(sid)) = self.selected_row()
            && let Some((p, s)) = state.session(&sid)
        {
            return Some(s.dir(p));
        }
        let pid = self.current_project.borrow().clone()?;
        state.project(&pid).map(|p| p.path.clone())
    }

    fn open_git_here(self: &Rc<Self>) {
        let dir = self.current_dir();
        match dir {
            Some(dir) if git::is_repo(&dir) => {
                *self.return_to.borrow_mut() = self.selected_row();
                self.open_git(&dir);
            }
            Some(_) => self.flash("this project is not a git repository"),
            None => self.flash("select a project first"),
        }
    }

    /// A file manager (yazi) or your editor at the task's folder, in a pane
    /// that closes when you quit it.
    fn open_files(self: &Rc<Self>) {
        let Some(dir) = self.current_dir() else {
            self.flash("select a project first");
            return;
        };
        let key = format!("edit:files:{}", dir.display());
        *self.return_to.borrow_mut() = self.selected_row();
        *self.current_key.borrow_mut() = Some(key.clone());
        if !self.status_of(&key).running() {
            let argv: Vec<String> = if agents::on_path("yazi") {
                vec!["yazi".into()]
            } else {
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
                vec!["sh".into(), "-c".into(), format!("exec {editor} .")]
            };
            self.spawn(&key, "editor", &argv, &dir, &[]);
        }
        self.focus_terminal(&key);
        self.header_left.set_markup(&format!(
            "<span foreground='{}'><b>files</b></span>  {}",
            self.theme.borrow().accent,
            esc(&tilde(&dir))
        ));
        self.header_right.set_text("quit to close");
    }

    /// lazygit in its own pane; quitting it returns to where you were.
    fn open_git(self: &Rc<Self>, dir: &Path) {
        let key = format!("git:{}", dir.display());
        *self.current_key.borrow_mut() = Some(key.clone());
        if !self.status_of(&key).running() {
            let argv: Vec<String> = if agents::on_path("lazygit") {
                vec!["lazygit".into()]
            } else {
                vec!["sh".into(), "-c".into(), "git status; exec \"${SHELL:-sh}\"".into()]
            };
            self.spawn(&key, "git", &argv, dir, &[]);
        }
        self.focus_terminal(&key);
        self.header_left.set_markup(&format!(
            "<span foreground='{}'><b>git</b></span>  {}",
            self.theme.borrow().accent,
            esc(&tilde(dir))
        ));
        self.header_right.set_text("q to close");
    }

    /// Back to the row you were on before an editor or git pane opened.
    fn go_back(self: &Rc<Self>) {
        let target = self.return_to.borrow_mut().take();
        let pid = self.current_project.borrow().clone();
        *self.current_key.borrow_mut() = None;
        match target.or(pid.map(Row::Project)) {
            Some(Row::Session(sid)) => {
                self.select(&Row::Session(sid.clone()));
                self.show_session(&sid);
            }
            Some(Row::Notes(pid)) => {
                self.select(&Row::Notes(pid.clone()));
                self.show_notes(&pid);
            }
            Some(Row::Project(pid)) => {
                self.select(&Row::Project(pid.clone()));
                self.show_project(&pid);
            }
            Some(Row::NewTask(pid)) => self.select(&Row::Project(pid)),
            None => self.show_empty(),
        }
        self.rebuild_sidebar();
    }

    /// Commits the worktree task's changes and merges its branch into the
    /// project. Press twice.
    fn merge_current(self: &Rc<Self>) {
        let Some(Row::Session(sid)) = self.selected_row() else {
            self.flash("select a worktree task to merge");
            return;
        };
        let Some((project, session)) = self.state.borrow().session(&sid).map(|(p, s)| (p.clone(), s.clone())) else {
            return;
        };
        let Some(w) = session.worktree.clone() else {
            self.flash("this task works in the project folder directly; there is nothing to merge");
            return;
        };
        let armed = self
            .pending_merge
            .borrow()
            .as_ref()
            .is_some_and(|(id, at)| *id == sid && at.elapsed() < Duration::from_secs(3));
        if !armed {
            *self.pending_merge.borrow_mut() = Some((sid.clone(), Instant::now()));
            let into = git::branch(&project.path).unwrap_or_else(|| "?".into());
            let warn = if into != w.base { format!(" (it started from {}!)", w.base) } else { String::new() };
            self.flash(&format!("press ^⇧M again to commit this task's work and merge {} into {into}{warn}", w.branch));
            return;
        }
        *self.pending_merge.borrow_mut() = None;
        if self.status_of(&sid) == Status::Working {
            self.flash("the agent is still working. merge when it is done");
            return;
        }
        if let Err(e) = git::commit_all(&w.path, &format!("{} (codebench task)", session.title)) {
            self.flash(&format!("could not commit the task's changes: {e}"));
            return;
        }
        let message = format!("Merge codebench task: {}", session.title);
        self.git_cache.borrow_mut().clear();
        match git::merge(&project.path, &w.branch, &message) {
            Ok(git::Merge::Merged) => self.flash("merged. ^⇧D removes the task and its worktree when you are done with it"),
            Ok(git::Merge::UpToDate) => self.flash("nothing new to merge"),
            Ok(git::Merge::Conflicts) => {
                *self.return_to.borrow_mut() = Some(Row::Session(sid));
                self.open_git(&project.path);
                self.flash("merge conflicts: resolve them here in lazygit, or ask an agent in the project to fix them");
            }
            Err(e) => self.flash(&format!("merge refused: {}", e.lines().next().unwrap_or(&e))),
        }
        self.rebuild_sidebar();
        self.update_header();
    }

    /// Gives the project's Claude and Codex tasks a browser (Playwright's MCP
    /// server with a visible Chromium), or takes it away. Applies to tasks
    /// as they start or resume.
    fn toggle_browser(self: &Rc<Self>) {
        let Some(pid) = self.current_project.borrow().clone() else {
            self.flash("select a project first");
            return;
        };
        if !agents::on_path("npx") {
            self.flash("the browser needs node (npx). install it, for example with mise use -g node");
            return;
        }
        let on = {
            let mut state = self.state.borrow_mut();
            let Some(p) = state.project_mut(&pid) else { return };
            p.browser = !p.browser;
            let on = p.browser;
            state.save();
            on
        };
        self.rebuild_sidebar();
        let running = self
            .state
            .borrow()
            .project(&pid)
            .is_some_and(|p| p.sessions.iter().any(|s| self.status_of(&s.id).running()));
        let restart = if running { " running tasks pick it up when restarted (^⇧W, then select them)" } else { "" };
        if on {
            let chrome = if agents::chromium().is_some() { "" } else { " (no chromium found: playwright will download its own)" };
            self.flash(&format!("browser on: agents can open pages, click, type and take screenshots{chrome}.{restart}"));
        } else {
            self.flash(&format!("browser off.{restart}"));
        }
    }

    // ── workflows ──────────────────────────────────────────────────────────

    fn open_workflows(self: &Rc<Self>) {
        let Some(pid) = self.current_project.borrow().clone() else {
            self.flash("select a project first");
            return;
        };
        self.picker.fill("workflows   enter run · ^E edit · type to filter or name a new one", Some(("filter, or a name for a new workflow", "")), &[]);
        *self.picker.mode.borrow_mut() = PickerMode::Workflows(pid.clone());
        self.fill_workflows(&pid);
        self.picker.show();
    }

    fn fill_workflows(&self, pid: &str) {
        let Some(project) = self.state.borrow().project(pid).cloned() else { return };
        let filter = self.picker.entry.text().trim().to_lowercase();
        let mut rows: Vec<Option<Workflow>> = workflow::list(&project)
            .into_iter()
            .filter(|w| filter.is_empty() || w.name.to_lowercase().contains(&filter))
            .map(Some)
            .collect();
        rows.push(None);
        let labels: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Some(wf) => {
                    let when = match (&wf.schedule, &wf.schedule_text) {
                        (Some(_), Some(text)) => text.clone(),
                        (None, Some(_)) => "bad schedule".into(),
                        _ => String::new(),
                    };
                    let scope = match (&wf.collection, wf.global) {
                        (Some(c), _) => format!(" ({c})"),
                        (None, true) => " (global)".to_string(),
                        _ => String::new(),
                    };
                    format!("{:<26}{:<9}{when}{scope}", wf.name, wf.agent)
                }
                None if filter.is_empty() => "+ new workflow".into(),
                None => format!("+ new workflow \"{filter}\""),
            })
            .collect();
        self.picker.set_options(&labels);
        *self.picker.workflows.borrow_mut() = rows;
    }

    fn edit_picked_workflow(self: &Rc<Self>) {
        let pid = match &*self.picker.mode.borrow() {
            PickerMode::Workflows(pid) => pid.clone(),
            _ => return,
        };
        let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
        let Some(Some(wf)) = self.picker.workflows.borrow().get(idx).cloned() else { return };
        let Some(project) = self.state.borrow().project(&pid).cloned() else { return };
        *self.picker.mode.borrow_mut() = PickerMode::Closed;
        self.picker.root.set_visible(false);
        self.edit_file(&project, &wf.path);
    }

    /// Opens a file in the editor in its own pane; the pane closes when the
    /// editor exits.
    /// Opens a note or workflow in the built-in editor: type, it saves as
    /// you go, Esc goes back.
    fn edit_file(self: &Rc<Self>, project: &store::Project, path: &Path) {
        self.close_editor();
        *self.return_to.borrow_mut() = self.selected_row();
        *self.current_project.borrow_mut() = Some(project.id.clone());
        *self.current_key.borrow_mut() = Some(format!("note:{}", path.display()));
        *self.editor_path.borrow_mut() = Some(path.to_path_buf());
        self.load_editor(path);
        self.stack.set_visible_child_name("editor");
        self.editor.grab_focus();
        self.header_left.set_markup(&format!(
            "<span foreground='{}'><b>{}</b></span>  <span foreground='{}'>›</span>  {}",
            self.theme.borrow().accent,
            esc(&project.name),
            self.theme.borrow().muted,
            esc(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        ));
        self.header_right.set_text("saves as you type · esc back · ^O open in your editor");
    }

    fn load_editor(&self, path: &Path) {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        self.editor_loading.set(true);
        let buffer = self.editor.buffer();
        buffer.set_text(&text);
        buffer.place_cursor(&buffer.end_iter());
        self.editor_loading.set(false);
        self.editor_dirty.set(false);
        *self.editor_disk.borrow_mut() = Some(text);
    }

    /// Saves pending edits and lets go of the file, so later navigation
    /// never writes it again.
    fn close_editor(self: &Rc<Self>) {
        self.save_editor();
        if !self.editor_dirty.get() {
            *self.editor_path.borrow_mut() = None;
            *self.editor_disk.borrow_mut() = None;
        }
    }

    /// Writes what was typed to the file. Untouched files are never
    /// written; if the file changed on disk meanwhile, the typed text goes
    /// to a copy beside it instead of over the other edit.
    fn save_editor(self: &Rc<Self>) {
        let Some(path) = self.editor_path.borrow().clone() else { return };
        if !self.editor_dirty.get() {
            return;
        }
        let buffer = self.editor.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let disk = std::fs::read_to_string(&path).ok();
        let loaded = self.editor_disk.borrow().clone();
        if disk.as_deref() == Some(text.as_str()) {
            self.editor_dirty.set(false);
            *self.editor_disk.borrow_mut() = disk;
            return;
        }
        if disk.is_some() && disk != loaded {
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let copy = path.with_file_name(format!("{stem} (my edits {}){ext}", store::now()));
            match std::fs::write(&copy, &text) {
                Ok(()) => {
                    self.flash(&format!(
                        "{} changed on disk while you typed; your version is in {}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        copy.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    if self.current_key.borrow().as_deref() == Some(&format!("note:{}", path.display())) {
                        self.load_editor(&path);
                    } else {
                        self.editor_dirty.set(false);
                        *self.editor_disk.borrow_mut() = disk;
                    }
                }
                Err(e) => self.flash(&format!("could not save your edits: {e}")),
            }
            return;
        }
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        match std::fs::write(&tmp, &text).and_then(|_| std::fs::rename(&tmp, &path)) {
            Ok(()) => {
                self.editor_dirty.set(false);
                *self.editor_disk.borrow_mut() = Some(text);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                self.flash(&format!("not saved: {e}"));
            }
        }
    }

    /// Light markdown: headings in bold accent, list markers in accent.
    fn highlight_markdown(&self, buffer: &gtk::TextBuffer) {
        let table = buffer.tag_table();
        let theme = self.theme.borrow();
        if let Some(t) = table.lookup("heading") {
            t.set_foreground(Some(&theme.accent));
        }
        if let Some(t) = table.lookup("mark") {
            t.set_foreground(Some(&theme.accent));
        }
        drop(theme);
        buffer.remove_all_tags(&buffer.start_iter(), &buffer.end_iter());
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        for (i, line) in text.lines().enumerate() {
            let Some(start) = buffer.iter_at_line(i as i32) else { continue };
            let trimmed = line.trim_start();
            let indent = (line.len() - trimmed.len()) as i32;
            if trimmed.starts_with('#') {
                let mut end = start;
                end.forward_to_line_end();
                buffer.apply_tag_by_name("heading", &start, &end);
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("- [") {
                let mut a = start;
                a.forward_chars(indent);
                let mut b = a;
                b.forward_chars(if trimmed.starts_with("- [") { 5 } else { 1 });
                buffer.apply_tag_by_name("mark", &a, &b);
            }
        }
    }

    /// The old way: the file in your terminal editor.
    fn edit_in_terminal(self: &Rc<Self>, project: &store::Project, path: &Path) {
        let key = format!("edit:{}", path.display());
        *self.return_to.borrow_mut() = self.selected_row();
        *self.current_project.borrow_mut() = Some(project.id.clone());
        *self.current_key.borrow_mut() = Some(key.clone());
        if !self.status_of(&key).running() {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let file = path.to_string_lossy().replace('\'', "'\\''");
            let argv = vec!["sh".to_string(), "-c".to_string(), format!("exec {editor} '{file}'")];
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| project.path.clone());
            self.spawn(&key, "editor", &argv, &dir, &[]);
        }
        self.focus_terminal(&key);
        self.header_left.set_markup(&format!(
            "<span foreground='{}'><b>{}</b></span>  editing {}",
            self.theme.borrow().accent,
            esc(&project.name),
            esc(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        ));
        self.flash("save and quit the editor to return. workflows run with ^⇧P");
    }

    /// Starts a run of the workflow as a new task. Earlier runs of the same
    /// workflow are archived so only the latest shows.
    fn run_workflow(self: &Rc<Self>, pid: &str, wf: &Workflow, focus: bool) {
        if !agents::takes_opening_prompt(&wf.agent) {
            self.flash(&format!("{}: agent \"{}\" cannot take a workflow prompt", wf.name, wf.agent));
            return;
        }
        let now = store::now();
        let id = uuid::Uuid::new_v4().to_string();
        self.state.borrow_mut().add_run(
            pid,
            Session {
                id: id.clone(),
                agent: wf.agent.clone(),
                title: format!("{} · {}", wf.name, workflow::stamp(now)),
                created: now,
                launched: false,
                prompt: Some(wf.prompt.clone()),
                archived: false,
                workflow: Some(wf.path.clone()),
                worktree: None,
                codex_id: None,
            },
        );
        if focus {
            self.rebuild_sidebar();
            self.select(&Row::Session(id));
        } else {
            self.launch(&id, None);
            self.rebuild_sidebar();
        }
    }

    fn run_due_workflows(self: &Rc<Self>) {
        let now = store::now();
        let projects = self.state.borrow().projects.clone();
        for (pid, wf) in workflow::due(&projects, now) {
            if let Some(project) = projects.iter().find(|p| p.id == pid) {
                workflow::mark_ran(&wf, project, now);
            }
            self.run_workflow(&pid, &wf, false);
            self.flash(&format!("scheduled workflow started: {}", wf.name));
        }
    }

    // ── messages between tasks ─────────────────────────────────────────────

    fn process_requests(self: &Rc<Self>) {
        for req in bus::take_all() {
            match req {
                Request::Send { from, to, text } => self.receive(&from, &to, &text),
                Request::StartTask { from, title, agent, prompt } => {
                    self.start_task_for(&from, &title, &agent, &prompt)
                }
                Request::ShowArtifact { from, file } => {
                    let pid = self.state.borrow().session(&from).map(|(p, _)| p.id.clone());
                    if let Some(pid) = pid {
                        self.rebuild_sidebar();
                        self.open_viewer(&pid, &file);
                        self.flash(&format!("{} showed an artifact: {file}", self.sender_label(&from)));
                    }
                }
            }
        }
    }

    fn sender_label(&self, from: &str) -> String {
        self.state
            .borrow()
            .session(from)
            .map(|(_, s)| format!("\"{}\" ({})", s.title, s.agent))
            .unwrap_or_else(|| "another task".into())
    }

    /// Delivers a message from another task: typed in now if the agent is
    /// free, queued if it is busy, or used to start it if it is stopped.
    fn receive(self: &Rc<Self>, from: &str, to: &str, text: &str) {
        let msg = format!("Message from Codebench task {}: {text}", self.sender_label(from));
        self.deliver_text(to, &msg);
    }

    /// Types a prompt into a task now if its agent is free, queues it while
    /// it is busy, and starts the task with it if it is stopped.
    fn deliver_text(self: &Rc<Self>, to: &str, msg: &str) {
        let msg = msg.to_string();
        let Some(agent) = self.state.borrow().session(to).map(|(_, s)| s.agent.clone()) else { return };
        let status = self.status_of(to);
        if status.running() {
            self.inbox.borrow_mut().entry(to.to_string()).or_default().push(msg);
            if status.free() {
                self.deliver_inbox(to);
            }
        } else if agents::takes_prompt_on_resume(&agent) {
            self.launch(to, Some(&msg));
        } else {
            self.inbox.borrow_mut().entry(to.to_string()).or_default().push(msg);
            self.launch(to, None);
            // No hook reports when these agents are ready; give them a moment.
            let b = self.clone();
            let id = to.to_string();
            glib::timeout_add_local_once(Duration::from_secs(5), move || b.deliver_inbox(&id));
        }
        self.rebuild_sidebar();
    }

    /// Types all queued messages into the task as one prompt.
    fn deliver_inbox(self: &Rc<Self>, key: &str) {
        let Some(msgs) = self.inbox.borrow_mut().remove(key) else { return };
        let term = self.running.borrow().get(key).map(|r| r.term.clone());
        match term {
            Some(term) => type_into(&term, &msgs.join("\n\n")),
            None => {
                self.inbox.borrow_mut().insert(key.to_string(), msgs);
            }
        }
    }

    /// A task asked for a new task on its project; create and start it in the
    /// background.
    fn start_task_for(self: &Rc<Self>, from: &str, title: &str, agent: &str, prompt: &str) {
        let Some(pid) = self.state.borrow().session(from).map(|(p, _)| p.id.clone()) else { return };
        let starter = self.sender_label(from);
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.state.borrow_mut();
            let Some(project) = state.project_mut(&pid) else { return };
            project.sessions.push(Session {
                id: id.clone(),
                agent: agent.to_string(),
                title: title.to_string(),
                created: store::now(),
                launched: false,
                prompt: Some(format!("(Started by Codebench task {starter}.) {prompt}")),
                archived: false,
                workflow: None,
                worktree: None,
                codex_id: None,
            });
            state.save();
        }
        self.launch(&id, None);
        self.flash(&format!("{starter} started a new task: {title}"));
    }

    // ── actions ────────────────────────────────────────────────────────────

    fn on_key(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
        if !matches!(*self.picker.mode.borrow(), PickerMode::Closed) {
            return glib::Propagation::Proceed;
        }
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);
        let key = key.to_lower();

        let in_editor = self.current_key.borrow().as_deref().is_some_and(|k| k.starts_with("note:"));
        if in_editor && key == gdk::Key::Escape && !ctrl && !alt && !shift {
            self.close_editor();
            // Back to the list it was opened from.
            match self.view.get() {
                v @ (View::Notes | View::Workflows) => self.show_view(v),
                _ => self.go_back(),
            }
            return glib::Propagation::Stop;
        }
        if in_editor && ctrl && !shift && !alt && key == gdk::Key::o {
            let path = self.editor_path.borrow().clone();
            self.close_editor();
            let project = self.current_project.borrow().clone().and_then(|pid| self.state.borrow().project(&pid).cloned());
            if let (Some(path), Some(p)) = (path, project) {
                self.edit_in_terminal(&p, &path);
            }
            return glib::Propagation::Stop;
        }
        if in_editor && ctrl && !shift && !alt && key == gdk::Key::s {
            self.save_editor();
            if !self.editor_dirty.get() {
                self.flash("saved");
            }
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::F1 && !ctrl && !alt {
            self.open_help();
            return glib::Propagation::Stop;
        }
        // F2 because fcitx5 (and GTK) take Ctrl+Shift+U for unicode input.
        if key == gdk::Key::F2 && !ctrl && !alt {
            self.open_accounts();
            return glib::Propagation::Stop;
        }
        if ctrl && shift && !alt {
            match key {
                gdk::Key::n => self.open_new_task(),
                gdk::Key::p => self.open_workflows(),
                gdk::Key::g => self.open_git_here(),
                gdk::Key::f => self.open_files(),
                gdk::Key::m => self.merge_current(),
                gdk::Key::e => self.open_notes(),
                gdk::Key::h => self.start_handoff(),
                gdk::Key::o => self.open_add_project(),
                gdk::Key::u => self.open_accounts(),
                gdk::Key::i => self.open_import(),
                gdk::Key::l => self.link_notes_dialog(false),
                gdk::Key::j => self.open_in_obsidian(),
                gdk::Key::r => self.open_rename(),
                gdk::Key::w => self.stop_current(),
                gdk::Key::d => self.delete_current(),
                gdk::Key::a => {
                    self.show_archived.set(!self.show_archived.get());
                    self.rebuild_sidebar();
                    self.flash(if self.show_archived.get() {
                        "showing handed-off tasks"
                    } else {
                        "hiding handed-off tasks"
                    });
                }
                gdk::Key::b => self.sidebar_box.set_visible(!self.sidebar_box.is_visible()),
                gdk::Key::t => self.toggle_top_bars(),
                gdk::Key::s => self.toggle_split(),
                gdk::Key::k => self.toggle_browser(),
                gdk::Key::y => self.open_phone_panel(),
                gdk::Key::Left => {
                    let key = self.current_key.borrow().clone();
                    if let Some(t) = key.and_then(|k| self.running.borrow().get(&k).map(|r| r.term.clone())) {
                        t.grab_focus();
                    }
                }
                gdk::Key::Right => {
                    let key = self.pinned.borrow().clone();
                    if let Some(t) = key.and_then(|k| self.running.borrow().get(&k).map(|r| r.term.clone())) {
                        t.grab_focus();
                    }
                }
                gdk::Key::c => {
                    if let Some(t) = self.current_term() {
                        t.copy_clipboard_format(vte::Format::Text);
                    }
                }
                gdk::Key::v => {
                    if let Some(t) = self.current_term() {
                        t.paste_clipboard();
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            return glib::Propagation::Stop;
        }
        if alt && !ctrl && !shift {
            let view = match key {
                gdk::Key::_1 => Some(View::Tasks),
                gdk::Key::_2 => Some(View::Files),
                gdk::Key::_3 => Some(View::Notes),
                gdk::Key::_4 => Some(View::Workflows),
                gdk::Key::_5 => Some(View::Artifacts),
                gdk::Key::_6 => Some(View::Git),
                gdk::Key::_7 => Some(View::Processes),
                _ => None,
            };
            if let Some(view) = view {
                self.show_view(view);
                return glib::Propagation::Stop;
            }
            match key {
                gdk::Key::Up => self.move_selection(-1),
                gdk::Key::Down => self.move_selection(1),
                gdk::Key::slash => self.open_search(),
                _ => return glib::Propagation::Proceed,
            }
            return glib::Propagation::Stop;
        }
        if ctrl && !shift && !alt {
            let tab = match key {
                gdk::Key::_1 => Some(0),
                gdk::Key::_2 => Some(1),
                gdk::Key::_3 => Some(2),
                gdk::Key::_4 => Some(3),
                gdk::Key::_5 => Some(4),
                gdk::Key::_6 => Some(5),
                gdk::Key::_7 => Some(6),
                gdk::Key::_8 => Some(7),
                gdk::Key::_9 => Some(8),
                _ => None,
            };
            if let Some(i) = tab {
                self.project_tab(i);
                return glib::Propagation::Stop;
            }
            match key {
                gdk::Key::Page_Up => self.cycle_project(-1),
                gdk::Key::Page_Down => self.cycle_project(1),
                _ => return glib::Propagation::Proceed,
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }

    fn open_path(self: &Rc<Self>, path: &Path) {
        let pid = self.state.borrow_mut().add_project(path);
        self.rebuild_sidebar();
        self.select(&Row::Project(pid));
    }

    fn open_add_project(self: &Rc<Self>) {
        self.picker.fill(
            &format!("add project   type a name for a new one in {}", tilde(&projects_root())),
            Some(("new project name, or filter your folders", "")),
            &[],
        );
        *self.picker.mode.borrow_mut() = PickerMode::AddProject;
        self.fill_add_project();
        self.picker.show();
    }

    /// Rows: a new project named after what was typed, the file dialog, and
    /// folders in the projects folder that are not projects yet.
    fn fill_add_project(&self) {
        let typed = self.picker.entry.text().trim().to_string();
        let name = project_folder_name(&typed);
        let root = projects_root();
        let known: Vec<PathBuf> = self.state.borrow().projects.iter().map(|p| p.path.clone()).collect();
        let mut folders: Vec<(u64, PathBuf)> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .filter(|p| !known.contains(&p.canonicalize().unwrap_or_else(|_| p.clone())))
            .filter(|p| typed.is_empty() || p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&typed.to_lowercase())))
            .map(|p| {
                let modified = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                (modified, p)
            })
            .collect();
        folders.sort_by(|a, b| b.0.cmp(&a.0));

        let mut choices = Vec::new();
        let exists = !name.is_empty() && root.join(&name).exists();
        if !name.is_empty() && !exists {
            choices.push(AddChoice::New(name.clone()));
        }
        choices.extend(folders.into_iter().take(12).map(|(_, p)| AddChoice::Existing(p)));
        choices.push(AddChoice::Browse);

        let labels: Vec<String> = choices
            .iter()
            .map(|c| match c {
                AddChoice::New(n) => format!("+ new project {n}   (creates {}, with git)", tilde(&root.join(n))),
                AddChoice::Browse => "open another folder…".to_string(),
                AddChoice::Existing(p) => {
                    let git = if git::is_repo(p) { "  git" } else { "" };
                    format!("  {}{git}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                }
            })
            .collect();
        self.picker.set_options(&labels);
        *self.picker.add_choices.borrow_mut() = choices;
    }

    /// Makes the folder, runs git init, adds it as a project and opens it.
    fn create_project(self: &Rc<Self>, name: &str) {
        let dir = projects_root().join(name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.flash(&format!("could not create {}: {e}", tilde(&dir)));
            return;
        }
        if !git::is_repo(&dir)
            && let Err(e) = git::init(&dir)
        {
            self.flash(&format!("created {}, but git init failed: {e}", tilde(&dir)));
        }
        self.open_path(&dir);
        self.flash("new project. ^⇧E to write its brief, ^⇧N to start a task");
    }

    fn add_project_dialog(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Add project folder");
        dialog.set_initial_folder(Some(&gio::File::for_path(store::home().join("code"))));
        let b = self.clone();
        dialog.select_folder(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                b.open_path(&path);
            }
        });
    }

    fn open_notes(self: &Rc<Self>) {
        if self.current_project.borrow().is_some() {
            self.show_view(View::Notes);
            return;
        }
        let pid = self.current_project.borrow().clone();
        match pid {
            Some(pid) => self.select(&Row::Notes(pid)),
            None => self.flash("select a project first"),
        }
    }

    /// Points the project's notes at another folder, such as one in an
    /// Obsidian vault, carrying the existing brief and handoffs across.
    /// Points the notes at another folder. With `then_open`, opens the brief
    /// in Obsidian afterwards if the folder is in a vault.
    /// Reports saves that failed, and asks what to do when the saved
    /// projects could not be read, instead of quietly starting empty.
    fn watch_state_errors(self: &Rc<Self>) {
        let b = self.clone();
        glib::timeout_add_seconds_local(2, move || {
            if let Some(e) = store::take_save_error() {
                b.flash(&format!("not saved: {e}"));
            }
            glib::ControlFlow::Continue
        });
        let Some(err) = self.state.borrow().load_error.clone() else { return };
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message("Codebench could not read its saved projects")
            .detail(format!(
                "{err}\n\nStart fresh to use Codebench with no projects (the old file is kept), or quit and fix the file."
            ))
            .buttons(["Quit", "Start fresh"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let b = self.clone();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            if res == Ok(1) {
                b.state.borrow_mut().start_fresh();
                b.state.borrow().save();
            } else {
                b.window.application().inspect(|a| a.quit());
            }
        });
    }

    fn link_notes_dialog(self: &Rc<Self>, then_open: bool) {
        let Some(pid) = self.current_project.borrow().clone() else {
            self.flash("select a project first");
            return;
        };
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Folder for this project's notes");
        let start = notes::default_vault().unwrap_or_else(store::home);
        dialog.set_initial_folder(Some(&gio::File::for_path(start)));
        let b = self.clone();
        dialog.select_folder(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            let Some(dir) = res.ok().and_then(|f| f.path()) else { return };
            // Copies everything over first; the project points at the new
            // folder only once that worked.
            let result = {
                let mut state = b.state.borrow_mut();
                let Some(mut p) = state.project(&pid).cloned() else { return };
                notes::relink(&mut p, &dir).and_then(|left| {
                    let before = state.project(&pid).cloned();
                    if let Some(slot) = state.project_mut(&pid) {
                        *slot = p;
                    }
                    state.try_save().map(|_| left).inspect_err(|_| {
                        if let (Some(slot), Some(before)) = (state.project_mut(&pid), before) {
                            *slot = before;
                        }
                    })
                })
            };
            let left = match result {
                Ok(left) => left,
                Err(e) => {
                    b.flash(&format!("notes not moved: {e}"));
                    return;
                }
            };
            // The editor pane still has the old folder open.
            let key = notes_key(&pid);
            b.stop(&key);
            b.rebuild_sidebar();
            b.update_header();
            let in_vault = notes::vault_root(&dir).is_some();
            let where_ = if in_vault { "in your vault" } else { "" };
            let kept = match left.as_slice() {
                [] => String::new(),
                [one] => format!(" · {} not copied (the new folder has its own)", one.display()),
                many => format!(" · {} files not copied (the new folder has its own), e.g. {}", many.len(), many[0].display()),
            };
            b.flash(&format!("notes now live {where_} at {}{kept}", tilde(&dir)));
            if then_open && in_vault {
                open_obsidian(&dir);
            } else if then_open {
                b.flash("that folder is not inside an Obsidian vault, so Obsidian cannot open it");
            }
        });
    }

    fn open_in_obsidian(self: &Rc<Self>) {
        let Some(pid) = self.current_project.borrow().clone() else { return };
        let Some(project) = self.state.borrow().project(&pid).cloned() else { return };
        let dir = notes::ensure(&project);
        if notes::vault_root(&dir).is_none() {
            // Obsidian only opens files inside a vault: pick a folder there
            // first, then the brief opens.
            self.flash("these notes are not in your vault yet. pick a folder in it for them");
            self.link_notes_dialog(true);
            return;
        }
        open_obsidian(&dir);
    }

    fn stop_current(self: &Rc<Self>) {
        let Some(key) = self.focused_key() else { return };
        self.stop(&key);
        self.flash("stopped. select it again to resume");
    }

    /// Asks before deleting the selected task, or removing the selected
    /// project, in a dialog that defaults to Cancel.
    fn delete_current(self: &Rc<Self>) {
        let Some(target) = self.selected_row() else { return };
        let (message, detail, button) = {
            let state = self.state.borrow();
            match &target {
                Row::Project(pid) => {
                    let Some(p) = state.project(pid) else { return };
                    (
                        format!("Remove project “{}”?", p.name),
                        "It leaves codebench and its tasks stop. The folder and its notes stay on disk.".to_string(),
                        "Remove",
                    )
                }
                Row::Session(sid) => {
                    let Some((_, s)) = state.session(sid) else { return };
                    let detail = if s.worktree.is_some() {
                        "The task stops and its worktree is deleted. Anything not merged is lost."
                    } else {
                        "The task stops and its conversation leaves codebench."
                    };
                    (format!("Delete task “{}”?", s.title), detail.to_string(), "Delete")
                }
                Row::Notes(_) | Row::NewTask(_) => return,
            }
        };
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(message)
            .detail(detail)
            .buttons(["Cancel", button])
            .cancel_button(0)
            .default_button(0)
            .build();
        let b = self.clone();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            if res == Ok(1) {
                b.delete_row(&target);
            }
        });
    }

    fn delete_row(self: &Rc<Self>, target: &Row) {
        let doomed: Vec<String> = match target {
            Row::Session(sid) => vec![sid.clone()],
            Row::Project(pid) => self
                .state
                .borrow()
                .project(pid)
                .map(|p| p.sessions.iter().map(|s| s.id.clone()).chain([notes_key(pid)]).collect())
                .unwrap_or_default(),
            Row::Notes(_) | Row::NewTask(_) => Vec::new(),
        };
        for key in &doomed {
            self.stop(key);
            let removed = self.running.borrow_mut().remove(key);
            if let Some(r) = removed {
                self.detach(key, &r.term);
            }
            self.status.borrow_mut().remove(key);
            if let Some(r) = self.remote.borrow().as_ref() {
                r.forget_task(key);
            }
        }

        let mut state = self.state.borrow_mut();
        let next = match target {
            Row::Session(sid) => {
                let pid = state.session(sid).map(|(p, _)| p.id.clone());
                if let Some(p) = pid.as_deref().and_then(|id| state.project_mut(id)) {
                    // A handoff shares its worktree with the next task; only
                    // remove it when no other task uses it.
                    let wt = p.sessions.iter().find(|s| &s.id == sid).and_then(|s| s.worktree.clone());
                    if let Some(w) = wt
                        && !p.sessions.iter().any(|s| &s.id != sid && s.worktree.as_ref().is_some_and(|o| o.path == w.path))
                    {
                        git::remove_worktree(&p.path, &w.path, &w.branch);
                    }
                    p.sessions.retain(|s| &s.id != sid);
                }
                pid.map(Row::Project)
            }
            Row::Project(pid) => {
                state.projects.retain(|p| &p.id != pid);
                state.projects.first().map(|p| Row::Project(p.id.clone()))
            }
            Row::Notes(_) | Row::NewTask(_) => None,
        };
        state.save();
        drop(state);

        *self.current_key.borrow_mut() = None;
        *self.current_project.borrow_mut() = None;
        self.rebuild_sidebar();
        match next {
            Some(row) => self.select(&row),
            None => self.show_empty(),
        }
    }

    // ── picker ─────────────────────────────────────────────────────────────

    fn open_new_task(self: &Rc<Self>) {
        let Some(pid) = self.current_project.borrow().clone() else {
            self.flash("select a project first, or ^⇧O to add one");
            return;
        };
        let (is_repo, has_commit) = self
            .state
            .borrow()
            .project(&pid)
            .map_or((false, false), |p| (git::is_repo(&p.path), git::has_commit(&p.path)));
        let agents = agents::installed();
        self.picker.fill(
            "",
            Some(("task name (optional)", "")),
            &agents.iter().map(|a| format!("{:<9}{}", a.id, a.label)).collect::<Vec<_>>(),
        );
        self.picker.isolate.set((is_repo && has_commit).then_some(false));
        *self.picker.items.borrow_mut() = agents.iter().map(|a| a.id).collect();
        *self.picker.mode.borrow_mut() = PickerMode::NewTask;
        self.new_task_title();
        if is_repo && !has_commit {
            let title = self.picker.title.text();
            self.picker.title.set_text(&format!("{title}   (own worktree needs a first commit)"));
        }
        self.picker.show();
    }

    fn new_task_title(&self) {
        let pid = self.current_project.borrow().clone().unwrap_or_default();
        let name = self.state.borrow().project(&pid).map(|p| p.name.clone()).unwrap_or_default();
        let isolate = match self.picker.isolate.get() {
            Some(true) => "   tab: own worktree [on]",
            Some(false) => "   tab: own worktree [off]",
            None => "",
        };
        self.picker.title.set_text(&format!("new task in {name}{isolate}"));
    }

    fn open_rename(self: &Rc<Self>) {
        match self.selected_row() {
            Some(Row::Session(sid)) => {
                let title = self.state.borrow().session(&sid).map(|(_, s)| s.title.clone()).unwrap_or_default();
                self.picker.fill("rename task", Some(("task name", &title)), &[]);
                *self.picker.mode.borrow_mut() = PickerMode::Rename(sid);
                self.picker.show();
            }
            Some(Row::Project(pid)) => self.open_rename_project(&pid),
            _ => self.flash("select a task or a project to rename it"),
        }
    }

    fn open_rename_project(self: &Rc<Self>, pid: &str) {
        let name = self.state.borrow().project(pid).map(|p| p.name.clone()).unwrap_or_default();
        self.picker.fill("rename project (the folder is not renamed)", Some(("project name", &name)), &[]);
        *self.picker.mode.borrow_mut() = PickerMode::RenameProject(pid.to_string());
        self.picker.show();
    }

    fn open_help(self: &Rc<Self>) {
        self.picker.fill("commands   type to filter · enter runs · esc closes", Some(("filter", "")), &[]);
        *self.picker.mode.borrow_mut() = PickerMode::Help;
        self.fill_help();
        self.picker.show();
    }

    fn fill_help(&self) {
        let filter = self.picker.entry.text().trim().to_lowercase();
        let matches = |k: &str, what: &str| filter.is_empty() || what.to_lowercase().contains(&filter) || k.to_lowercase().contains(&filter);
        let picked: Vec<usize> = ACTIONS.iter().enumerate().filter(|(_, (k, w, _))| matches(k, w)).map(|(i, _)| i).collect();
        let mut lines: Vec<String> = picked.iter().map(|&i| format!("{:<18}{}", ACTIONS[i].0, ACTIONS[i].1)).collect();
        lines.extend(OTHER_KEYS.iter().filter(|(k, w)| matches(k, w)).map(|(k, w)| format!("{k:<18}{w}")));
        self.picker.set_options(&lines);
        *self.picker.actions.borrow_mut() = picked;
    }

    fn run_action(self: &Rc<Self>, action: Action) {
        match action {
            Action::NewTask => self.open_new_task(),
            Action::Workflows => self.open_workflows(),
            Action::Notes => self.open_notes(),
            Action::Handoff => self.start_handoff(),
            Action::AddProject => self.open_add_project(),
            Action::Import => self.open_import(),
            Action::Accounts => self.open_accounts(),
            Action::Phone => self.open_phone_panel(),
            Action::LinkNotes => self.link_notes_dialog(false),
            Action::Obsidian => self.open_in_obsidian(),
            Action::Rename => self.open_rename(),
            Action::Git => self.open_git_here(),
            Action::Merge => self.merge_current(),
            Action::Browser => self.toggle_browser(),
            Action::Split => self.toggle_split(),
            Action::Stop => self.stop_current(),
            Action::Delete => self.delete_current(),
            Action::Archived => {
                self.show_archived.set(!self.show_archived.get());
                self.rebuild_sidebar();
                self.flash(if self.show_archived.get() { "showing handed-off tasks" } else { "hiding handed-off tasks" });
            }
            Action::Sidebar => self.sidebar_box.set_visible(!self.sidebar_box.is_visible()),
            Action::Files => self.open_files(),
            Action::TopBars => self.toggle_top_bars(),
            Action::Search => self.open_search(),
        }
    }

    fn close_picker(self: &Rc<Self>) {
        let mode = std::mem::replace(&mut *self.picker.mode.borrow_mut(), PickerMode::Closed);
        if let PickerMode::Pair(id) = mode
            && let Some(r) = self.remote.borrow().as_ref()
        {
            r.decide_pairing(&id, false);
            self.flash("phone pairing denied");
        }
        self.picker.root.set_visible(false);
        if let Some(t) = self.current_term() {
            t.grab_focus();
        }
    }

    fn picker_accept(self: &Rc<Self>) {
        let text = self.picker.entry.text().trim().to_string();
        let mode = std::mem::replace(&mut *self.picker.mode.borrow_mut(), PickerMode::Closed);
        match mode {
            PickerMode::Closed => {}
            PickerMode::Help => {
                let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
                let action = self.picker.actions.borrow().get(idx).map(|&i| ACTIONS[i].2);
                self.close_picker();
                if let Some(action) = action {
                    self.run_action(action);
                }
            }
            PickerMode::NewNote(pid) => {
                self.close_picker();
                let Some(p) = self.state.borrow().project(&pid).cloned() else { return };
                let name = if text.is_empty() { "note".to_string() } else { text };
                let stem = workflow::slug(&name);
                let path = notes::ensure(&p).join(format!("{}.md", if stem.is_empty() { "note" } else { &stem }));
                if !path.exists() {
                    let _ = std::fs::write(&path, format!("# {name}\n\n"));
                }
                self.edit_file(&p, &path);
            }
            PickerMode::RenameProject(pid) => {
                if !text.is_empty() {
                    let mut state = self.state.borrow_mut();
                    if let Some(p) = state.project_mut(&pid) {
                        // The default notes folder is named after the
                        // project; pin it so renaming does not lose notes.
                        if p.notes.is_none() {
                            p.notes = Some(p.notes_dir());
                        }
                        p.name = text;
                    }
                    state.save();
                    drop(state);
                    self.rebuild_sidebar();
                    self.update_header();
                    self.show_project(&pid);
                }
                self.close_picker();
            }
            PickerMode::Accounts => {
                *self.picker.mode.borrow_mut() = PickerMode::Accounts;
                self.sign_in_picked(false);
            }
            PickerMode::Pair(id) => {
                if let Some(r) = self.remote.borrow().as_ref() {
                    r.decide_pairing(&id, true);
                }
                self.close_picker();
                self.flash("phone paired");
            }
            PickerMode::Phone => {
                let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0);
                *self.picker.mode.borrow_mut() = PickerMode::Phone;
                self.phone_panel_pick(idx as usize);
            }
            PickerMode::AddProject => {
                let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
                let choice = self.picker.add_choices.borrow().get(idx).cloned();
                self.close_picker();
                match choice {
                    Some(AddChoice::New(name)) => self.create_project(&name),
                    Some(AddChoice::Browse) => self.add_project_dialog(),
                    Some(AddChoice::Existing(path)) => self.open_path(&path),
                    None => {}
                }
            }
            PickerMode::Import(pid) => {
                *self.picker.mode.borrow_mut() = PickerMode::Import(pid.clone());
                self.import_picked(&pid);
            }
            PickerMode::Workflows(pid) => {
                let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
                let picked = self.picker.workflows.borrow().get(idx).cloned();
                self.picker.root.set_visible(false);
                match picked {
                    Some(Some(wf)) => self.run_workflow(&pid, &wf, true),
                    Some(None) => {
                        let Some(project) = self.state.borrow().project(&pid).cloned() else { return };
                        let name = if text.is_empty() { "new workflow" } else { &text };
                        let path = workflow::create(&project, name);
                        self.edit_file(&project, &path);
                    }
                    None => self.close_picker(),
                }
            }
            PickerMode::Rename(sid) => {
                if !text.is_empty() {
                    if let Some(s) = self.state.borrow_mut().session_mut(&sid) {
                        s.title = text;
                    }
                    self.state.borrow().save();
                    self.rebuild_sidebar();
                    self.update_header();
                }
                self.close_picker();
            }
            PickerMode::NewTask => {
                let idx = self.picker.list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
                let agent = self.picker.items.borrow().get(idx).copied().unwrap_or("claude");
                let Some(pid) = self.current_project.borrow().clone() else { return };
                let Some(project) = self.state.borrow().project(&pid).cloned() else { return };
                let id = uuid::Uuid::new_v4().to_string();
                let title = if text.is_empty() {
                    format!("task {}", project.sessions.len() + 1)
                } else {
                    text
                };
                let worktree = if self.picker.isolate.get() == Some(true) {
                    match make_worktree(&project, &id, &title) {
                        Ok(w) => Some(w),
                        Err(e) => {
                            self.close_picker();
                            self.flash(&format!("could not create a worktree: {e}"));
                            return;
                        }
                    }
                } else {
                    None
                };
                {
                    let mut state = self.state.borrow_mut();
                    let Some(project) = state.project_mut(&pid) else { return };
                    project.sessions.push(Session {
                        id: id.clone(),
                        agent: agent.to_string(),
                        title,
                        created: store::now(),
                        launched: false,
                        prompt: None,
                        archived: false,
                        workflow: None,
                        worktree,
                        codex_id: None,
                    });
                    state.save();
                }
                self.picker.root.set_visible(false);
                self.rebuild_sidebar();
                self.select(&Row::Session(id));
            }
        }
    }
}

impl Picker {
    fn build() -> Picker {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("cb-picker");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_width_request(460);
        root.set_visible(false);
        let title = label("cb-picker-title");
        let entry = gtk::Entry::new();
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        // Long lists scroll inside the picker, keeping the pick in view.
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(440);
        scroll.set_child(Some(&list));
        list.connect_row_selected(|list, row| {
            let (Some(row), Some(scroll)) = (row, list.parent().and_then(|v| v.parent()).and_downcast::<gtk::ScrolledWindow>()) else { return };
            let Some(r) = row.compute_bounds(list) else { return };
            let adj = scroll.vadjustment();
            let (y, h) = (r.y() as f64, r.height() as f64);
            if y < adj.value() {
                adj.set_value(y);
            } else if y + h > adj.value() + adj.page_size() {
                adj.set_value(y + h - adj.page_size());
            }
        });
        root.append(&title);
        root.append(&entry);
        root.append(&scroll);
        Picker {
            root,
            title,
            entry,
            list,
            mode: RefCell::new(PickerMode::Closed),
            items: RefCell::default(),
            workflows: RefCell::default(),
            isolate: Cell::new(None),
            past: RefCell::default(),
            add_choices: RefCell::default(),
            actions: RefCell::default(),
        }
    }

    /// `entry` is (placeholder, initial text), or None to hide the entry.
    fn fill(&self, title: &str, entry: Option<(&str, &str)>, options: &[String]) {
        self.title.set_text(title);
        self.entry.set_visible(entry.is_some());
        if let Some((placeholder, text)) = entry {
            self.entry.set_placeholder_text(Some(placeholder));
            self.entry.set_text(text);
        }
        self.set_options(options);
    }

    /// Replaces the list rows without touching the entry.
    fn set_options(&self, options: &[String]) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for option in options {
            let l = label("cb-option");
            l.set_text(option);
            l.set_wrap(true);
            l.set_wrap_mode(pango::WrapMode::WordChar);
            l.set_max_width_chars(60);
            self.list.append(&l);
        }
        self.list.set_visible(!options.is_empty());
        if let Some(scroll) = self.list.parent().and_then(|v| v.parent()) {
            scroll.set_visible(!options.is_empty());
        }
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
    }

    fn show(&self) {
        self.root.set_visible(true);
        if WidgetExt::is_visible(&self.entry) {
            self.entry.grab_focus();
            self.entry.select_region(0, -1);
        } else if let Some(row) = self.list.row_at_index(0) {
            row.grab_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_become_safe_folders() {
        assert_eq!(project_folder_name("My New App"), "My-New-App");
        assert_eq!(project_folder_name("../../etc"), "etc");
        assert_eq!(project_folder_name("  a/b\\c "), "abc");
        assert_eq!(project_folder_name("..."), "");
    }
}
