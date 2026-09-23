//! The window: a project and task sidebar on the left, the selected task's
//! terminal on the right, a header line above and a key-hint line below.

use crate::accounts::{self, Account, Login};
use crate::agents;
use crate::history::{self, Past};
use crate::bus::{self, Request};
use crate::git;
use crate::notes;
use crate::store::{self, Session, State};
use crate::theme::{self, Theme};
use crate::progress;
use crate::usage;
use crate::workflow::{self, Workflow};
use crate::headless;
use gtk::{gdk, gio, glib, pango, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use vte::prelude::*;

pub const APP_ID: &str = "co.ericstevens.codebench";

const HINTS: &str = "^⇧N new task   ^⇧P workflows   ^⇧G git   ^⇧E notes   ^⇧H handoff   alt ↑↓ switch   F1 all keys";

const KEYS: &[(&str, &str)] = &[
    ("Ctrl+Shift+N", "new task in this project"),
    ("Ctrl+Shift+P", "workflows: run, edit (^E) or create saved prompts"),
    ("Ctrl+Shift+E", "edit project notes (brief and handoffs)"),
    ("Ctrl+Shift+G", "git (lazygit) for this task or project"),
    ("Ctrl+Shift+M", "merge a worktree task back into its branch (press twice)"),
    ("Tab", "in the new task box: give the task its own git worktree"),
    ("Ctrl+Shift+H", "hand off: write a note, continue in a fresh session"),
    ("Ctrl+Shift+O", "add a project folder"),
    ("Ctrl+Shift+I", "import past Claude and Codex chats for this project"),
    ("Ctrl+Shift+U", "accounts: see logins, sign in, switch accounts"),
    ("Ctrl+Shift+L", "link notes to a folder, e.g. in your Obsidian vault"),
    ("Ctrl+Shift+J", "open the brief in Obsidian"),
    ("Ctrl+Shift+R", "rename task"),
    ("Ctrl+Shift+W", "stop task (select it again to resume)"),
    ("Ctrl+Shift+D", "delete task or remove project (press twice)"),
    ("Ctrl+Shift+A", "show or hide handed-off tasks"),
    ("Ctrl+Shift+K", "give this project's agents a browser (on or off)"),
    ("Ctrl+Shift+S", "split: pin this task on the right, pick another for the left"),
    ("Ctrl+Shift+← / →", "focus the left or right side of a split"),
    ("Ctrl+Shift+B", "show or hide the sidebar"),
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
            Status::Done => "done",
            Status::Exited => "stopped",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Row {
    Project(String),
    Notes(String),
    Git(String),
    Session(String),
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
    Help,
    Workflows(String),
    Accounts,
    Import(String),
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
}

struct Running {
    term: vte::Terminal,
    pid: Cell<Option<i32>>,
}

struct App {
    window: gtk::ApplicationWindow,
    state: RefCell<State>,
    theme: RefCell<Theme>,
    css: gtk::CssProvider,
    sidebar_box: gtk::Box,
    sidebar: gtk::ListBox,
    rows: RefCell<Vec<Row>>,
    rebuilding: Cell<bool>,
    show_archived: Cell<bool>,
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
    agents_label: gtk::Label,
    paned: gtk::Paned,
    side_box: gtk::Box,
    side_header: gtk::Label,
    side_holder: gtk::Box,
    /// The task shown on the right of a split.
    pinned: RefCell<Option<String>>,
    board: gtk::Label,
    /// Holds off suspend while any agent is working.
    keep_awake: RefCell<Option<std::process::Child>>,
    current_project: RefCell<Option<String>>,
    /// Terminal key of the row on screen, if it has one.
    current_key: RefCell<Option<String>>,
    status_dir: PathBuf,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    reload_pending: Cell<bool>,
    pending_delete: RefCell<Option<(String, Instant)>>,
}

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    let bench: Rc<RefCell<Option<Rc<App>>>> = Rc::default();
    app.connect_command_line(move |app, cmd| {
        let b = bench.borrow().clone();
        let b = b.unwrap_or_else(|| {
            let b = App::build(app);
            *bench.borrow_mut() = Some(b.clone());
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

        let window = gtk::ApplicationWindow::new(app);
        window.set_title(Some("codebench"));
        window.set_default_size(1400, 900);
        // No client-side titlebar: Hyprland draws the border, like a terminal.
        window.set_decorated(false);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("cb-root");

        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.add_css_class("cb-sidebar");
        sidebar_box.set_width_request(270);
        let brand = label("cb-brand");
        brand.set_text("codebench");
        let sidebar = gtk::ListBox::new();
        sidebar.set_selection_mode(gtk::SelectionMode::Single);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&sidebar));
        let subs = label("cb-footer");
        subs.set_wrap(true);
        subs.set_wrap_mode(pango::WrapMode::WordChar);
        // Keep the login summary from widening the sidebar.
        subs.set_max_width_chars(28);
        subs.set_text(&format!(
            "agents: {}",
            agents::installed()
                .iter()
                .filter(|a| a.id != "shell")
                .map(|a| a.id)
                .collect::<Vec<_>>()
                .join(" ")
        ));
        sidebar_box.append(&brand);
        sidebar_box.append(&scroller);
        sidebar_box.append(&subs.clone());

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
        let picker = Picker::build();
        overlay.add_overlay(&picker.root);

        let footer = label("cb-footer");
        footer.set_text(HINTS);
        footer.set_ellipsize(pango::EllipsizeMode::End);

        main.append(&header);
        main.append(&paned);
        main.append(&footer);
        root.append(&sidebar_box);
        root.append(&main);
        window.set_child(Some(&root));

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
            rows: RefCell::default(),
            rebuilding: Cell::new(false),
            show_archived: Cell::new(false),
            stack,
            empty,
            header_left,
            header_right,
            footer,
            flash_gen: Cell::new(0),
            picker,
            running: RefCell::default(),
            status: RefCell::default(),
            context: RefCell::default(),
            handoffs: RefCell::default(),
            inbox: RefCell::default(),
            return_to: RefCell::default(),
            git_cache: RefCell::default(),
            pending_merge: RefCell::default(),
            accounts: RefCell::default(),
            agents_label: subs,
            paned,
            side_box,
            side_header,
            side_holder,
            pinned: RefCell::default(),
            keep_awake: RefCell::default(),
            board,
            current_project: RefCell::default(),
            current_key: RefCell::default(),
            status_dir,
            monitors: RefCell::default(),
            reload_pending: Cell::new(false),
            pending_delete: RefCell::default(),
        });
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
            match kind {
                Some(Row::Project(pid)) => b.show_project(&pid),
                Some(Row::Notes(pid)) => b.show_notes(&pid),
                Some(Row::Git(pid)) => {
                    // Like tasks, open only if the row is still selected a
                    // moment later, so moving past it does not start lazygit.
                    let b2 = b.clone();
                    glib::timeout_add_local_once(Duration::from_millis(500), move || {
                        let still = b2.sidebar.selected_row().and_then(|r| b2.rows.borrow().get(r.index() as usize).cloned());
                        if still != Some(Row::Git(pid.clone())) {
                            return;
                        }
                        let path = b2.state.borrow().project(&pid).map(|p| p.path.clone());
                        if let Some(path) = path {
                            *b2.current_project.borrow_mut() = Some(pid.clone());
                            *b2.return_to.borrow_mut() = Some(Row::Project(pid));
                            b2.open_git(&path);
                        }
                    });
                }
                Some(Row::Session(sid)) => b.show_session(&sid),
                None => {}
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let b = self.clone();
        keys.connect_key_pressed(move |_, key, _, mods| b.on_key(key, mods));
        self.window.add_controller(keys);

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
                gdk::Key::Return | gdk::Key::KP_Enter
                    if matches!(*b.picker.mode.borrow(), PickerMode::Help) =>
                {
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
            for r in b.running.borrow().values() {
                b.style_terminal(&r.term);
            }
            b.rebuild_sidebar();
            b.update_header();
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
            return Some(match key.strip_prefix("notes:") {
                Some(pid) => Row::Notes(pid.to_string()),
                None => Row::Session(key),
            });
        }
        self.current_project.borrow().clone().map(Row::Project)
    }

    fn rebuild_sidebar(&self) {
        self.rebuilding.set(true);
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let theme = self.theme.borrow();
        let state = self.state.borrow();
        let context = self.context.borrow();
        let show_archived = self.show_archived.get();
        let mut rows = Vec::new();

        for p in &state.projects {
            let attention = p.sessions.iter().filter(|s| self.status_of(&s.id).wants_attention()).count();
            let badge = if attention > 0 {
                format!("  <span foreground='{}'>{attention}</span>", theme.red)
            } else {
                String::new()
            };
            let web = if p.browser { format!("  <span foreground='{}'>◍</span>", theme.muted) } else { String::new() };
            let row = row_with(&format!("<b>{}</b>{web}{badge}", esc(&p.name)));
            row.add_css_class("cb-project");
            self.sidebar.append(&row);
            rows.push(Row::Project(p.id.clone()));

            let vault = if notes::vault_root(&p.notes_dir()).is_some() { "  vault" } else { "" };
            let glyph = if self.status_of(&notes_key(&p.id)).running() { "✎" } else { "≡" };
            self.sidebar.append(&row_with(&format!(
                "  <span foreground='{m}'>{glyph} notes{vault}</span>",
                m = theme.muted
            )));
            rows.push(Row::Notes(p.id.clone()));

            if let Some(branch) = self.git_branch_of(&p.path) {
                let changed = match self.git_changes(&p.path) {
                    Some(0) | None => String::new(),
                    Some(n) => format!("  <span foreground='{}'>+{n}</span>", theme.yellow),
                };
                self.sidebar.append(&row_with(&format!(
                    "  <span foreground='{m}'>± git  {}</span>{changed}",
                    esc(&branch),
                    m = theme.muted
                )));
                rows.push(Row::Git(p.id.clone()));
            }

            for s in p.sessions.iter().filter(|s| show_archived || !s.archived) {
                let (glyph, color, word) = self.status_of(&s.id).look(&theme);
                let word = if word.is_empty() {
                    String::new()
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
                self.sidebar.append(&row_with(&format!(
                    "  <span foreground='{color}'>{glyph}</span> <span foreground='{}'>{:<8}</span>{title}{tokens}{word}",
                    theme.muted,
                    esc(&s.agent),
                )));
                rows.push(Row::Session(s.id.clone()));
            }
        }
        let snapshot = state
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .map(|s| (s.id.clone(), self.status_of(&s.id).name()))
            .collect();
        bus::write_snapshot(&snapshot);
        drop((state, theme, context));

        let idx = self.selected_row().and_then(|sel| rows.iter().position(|r| *r == sel));
        *self.rows.borrow_mut() = rows;
        if let Some(row) = idx.and_then(|i| self.sidebar.row_at_index(i as i32)) {
            self.sidebar.select_row(Some(&row));
        }
        self.rebuilding.set(false);
    }

    /// Selects a sidebar row, which shows it through `row_selected`.
    fn select(&self, target: &Row) {
        let idx = self.rows.borrow().iter().position(|r| r == target);
        if let Some(row) = idx.and_then(|i| self.sidebar.row_at_index(i as i32)) {
            self.sidebar.select_row(Some(&row));
        }
    }

    fn move_selection(&self, delta: i32) {
        let idx = self.sidebar.selected_row().map(|r| r.index()).unwrap_or(-1) + delta;
        if let Some(row) = self.sidebar.row_at_index(idx) {
            self.sidebar.select_row(Some(&row));
        }
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
        self.board.set_markup(&self.board_markup(&p, &HashMap::new()));
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
            // Only if the page is still showing this project.
            let showing = b.current_key.borrow().is_none() && b.current_project.borrow().as_deref() == Some(pid.as_str());
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
        out.push_str(&dim("^⇧N new task   ^⇧P workflows   ^⇧E notes   ^⇧I import past chats   ^⇧G git   F1 all keys"));
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

    /// Clears "done" on the task you are looking at.
    fn mark_seen(&self) {
        let Some(key) = self.current_key.borrow().clone() else { return };
        if self.window.is_active() && self.status_of(&key) == Status::Done {
            self.status.borrow_mut().insert(key, Status::Idle);
            self.rebuild_sidebar();
        }
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
                self.stack.add_named(&term, Some(key));
                let r = Rc::new(Running { term, pid: Cell::new(None) });
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
                b.rebuild_sidebar();
            });
        }
        let visible = self.window.is_active() && self.current_key.borrow().as_deref() == Some(key);
        let status = if status == Status::Done && visible { Status::Idle } else { status };
        let prev = self.status.borrow_mut().insert(key.to_string(), status);
        if prev == Some(status) && status != Status::Idle {
            return;
        }
        if status.wants_attention() && !visible {
            self.notify(key, status);
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
        self.update_keep_awake();
    }

    /// While any agent is working, a systemd-inhibit lock stops the machine
    /// from suspending. The screen can still lock. The lock ends with the
    /// app, since it waits on our process id.
    fn update_keep_awake(&self) {
        let working = self.status.borrow().values().any(|s| *s == Status::Working);
        let mut lock = self.keep_awake.borrow_mut();
        match (working, lock.is_some()) {
            (true, false) => {
                *lock = std::process::Command::new("systemd-inhibit")
                    .args([
                        "--what=sleep",
                        "--who=Codebench",
                        "--why=Agents are working",
                        "--mode=block",
                        "tail",
                        &format!("--pid={}", std::process::id()),
                        "-f",
                        "/dev/null",
                    ])
                    .stdin(std::process::Stdio::null())
                    .spawn()
                    .ok();
            }
            (false, true) => {
                if let Some(mut child) = lock.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            _ => {}
        }
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

    fn current_term(&self) -> Option<vte::Terminal> {
        let key = self.current_key.borrow().clone()?;
        self.running.borrow().get(&key).map(|r| r.term.clone())
    }

    // ── handoff ────────────────────────────────────────────────────────────

    /// Asks the current task's agent to write a handoff note. When it
    /// finishes, `finish_handoff` moves the task to a fresh session.
    fn start_handoff(self: &Rc<Self>) {
        let Some(Row::Session(sid)) = self.selected_row() else {
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

    fn show_accounts_summary(&self) {
        let theme = self.theme.borrow();
        let parts: Vec<String> = self
            .accounts
            .borrow()
            .iter()
            .filter(|a| a.installed)
            .map(|a| match a.login {
                Login::In(_) => format!("{} <span foreground='{}'>✓</span>", a.agent, theme.green),
                Login::Out => format!("{} <span foreground='{}'>✗</span>", a.agent, theme.red),
                Login::Unknown(_) => format!("{} ?", a.agent),
            })
            .collect();
        self.agents_label.set_markup(&format!("{}   ^⇧U", parts.join("  ")));
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
                format!("{:<10}{login}{update}", a.agent)
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
            Some(Row::Project(pid) | Row::Git(pid)) => {
                self.select(&Row::Project(pid.clone()));
                self.show_project(&pid);
            }
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
    fn edit_file(self: &Rc<Self>, project: &store::Project, path: &Path) {
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
        if agents::get(&wf.agent).is_none() {
            self.flash(&format!("{}: unknown agent \"{}\"", wf.name, wf.agent));
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
        let Some(agent) = self.state.borrow().session(to).map(|(_, s)| s.agent.clone()) else { return };
        let msg = format!("Message from Codebench task {}: {text}", self.sender_label(from));
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

        if key == gdk::Key::F1 && !ctrl && !alt {
            self.open_help();
            return glib::Propagation::Stop;
        }
        if ctrl && shift && !alt {
            match key {
                gdk::Key::n => self.open_new_task(),
                gdk::Key::p => self.open_workflows(),
                gdk::Key::g => {
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
                gdk::Key::m => self.merge_current(),
                gdk::Key::e => self.open_notes(),
                gdk::Key::h => self.start_handoff(),
                gdk::Key::o => self.add_project_dialog(),
                gdk::Key::u => self.open_accounts(),
                gdk::Key::i => self.open_import(),
                gdk::Key::l => self.link_notes_dialog(),
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
                gdk::Key::s => self.toggle_split(),
                gdk::Key::k => self.toggle_browser(),
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
            match key {
                gdk::Key::Up => self.move_selection(-1),
                gdk::Key::Down => self.move_selection(1),
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
        let pid = self.current_project.borrow().clone();
        match pid {
            Some(pid) => self.select(&Row::Notes(pid)),
            None => self.flash("select a project first"),
        }
    }

    /// Points the project's notes at another folder, such as one in an
    /// Obsidian vault, carrying the existing brief and handoffs across.
    fn link_notes_dialog(self: &Rc<Self>) {
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
            let old = {
                let mut state = b.state.borrow_mut();
                let Some(p) = state.project_mut(&pid) else { return };
                let old = p.notes_dir();
                p.notes = Some(dir.clone());
                state.save();
                old
            };
            notes::carry_over(&old, &dir);
            // The editor pane still has the old folder open.
            let key = notes_key(&pid);
            b.stop(&key);
            b.rebuild_sidebar();
            b.update_header();
            let where_ = if notes::vault_root(&dir).is_some() { "in your vault" } else { "" };
            b.flash(&format!("notes now live {where_} at {}", tilde(&dir)));
        });
    }

    fn open_in_obsidian(self: &Rc<Self>) {
        let Some(pid) = self.current_project.borrow().clone() else { return };
        let Some(project) = self.state.borrow().project(&pid).cloned() else { return };
        let dir = notes::ensure(&project);
        if notes::vault_root(&dir).is_none() {
            self.flash("these notes are not in an Obsidian vault. ^⇧L links them to a vault folder");
            return;
        }
        let uri = notes::obsidian_uri(&dir.join("brief.md"));
        let _ = std::process::Command::new("xdg-open").arg(uri).spawn();
    }

    fn stop_current(self: &Rc<Self>) {
        let Some(key) = self.current_key.borrow().clone() else { return };
        self.stop(&key);
        self.flash("stopped. select it again to resume");
    }

    /// First press arms, a second press within three seconds deletes.
    fn delete_current(self: &Rc<Self>) {
        let Some(target) = self.selected_row() else { return };
        let key = match &target {
            Row::Project(id) | Row::Session(id) => id.clone(),
            Row::Notes(_) | Row::Git(_) => return,
        };
        let armed = self
            .pending_delete
            .borrow()
            .as_ref()
            .is_some_and(|(id, at)| *id == key && at.elapsed() < Duration::from_secs(3));
        if !armed {
            *self.pending_delete.borrow_mut() = Some((key, Instant::now()));
            let has_worktree = matches!(&target, Row::Session(sid)
                if self.state.borrow().session(sid).is_some_and(|(_, s)| s.worktree.is_some()));
            self.flash(match &target {
                Row::Project(_) => "press ^⇧D again to remove this project from codebench (files and notes are kept)",
                _ if has_worktree => "press ^⇧D again to delete this task and its worktree. anything not merged is lost",
                _ => "press ^⇧D again to delete this task",
            });
            return;
        }
        *self.pending_delete.borrow_mut() = None;

        let doomed: Vec<String> = match &target {
            Row::Session(sid) => vec![sid.clone()],
            Row::Project(pid) => self
                .state
                .borrow()
                .project(pid)
                .map(|p| p.sessions.iter().map(|s| s.id.clone()).chain([notes_key(pid)]).collect())
                .unwrap_or_default(),
            Row::Notes(_) | Row::Git(_) => Vec::new(),
        };
        for key in &doomed {
            self.stop(key);
            let removed = self.running.borrow_mut().remove(key);
            if let Some(r) = removed {
                self.detach(key, &r.term);
            }
            self.status.borrow_mut().remove(key);
        }

        let mut state = self.state.borrow_mut();
        let next = match &target {
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
            Row::Notes(_) | Row::Git(_) => None,
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
        let is_repo = self.state.borrow().project(&pid).is_some_and(|p| git::is_repo(&p.path));
        let agents = agents::installed();
        self.picker.fill(
            "",
            Some(("task name (optional)", "")),
            &agents.iter().map(|a| format!("{:<9}{}", a.id, a.label)).collect::<Vec<_>>(),
        );
        self.picker.isolate.set(is_repo.then_some(false));
        *self.picker.items.borrow_mut() = agents.iter().map(|a| a.id).collect();
        *self.picker.mode.borrow_mut() = PickerMode::NewTask;
        self.new_task_title();
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
        let Some(Row::Session(sid)) = self.selected_row() else { return };
        let title = self.state.borrow().session(&sid).map(|(_, s)| s.title.clone()).unwrap_or_default();
        self.picker.fill("rename task", Some(("task name", &title)), &[]);
        *self.picker.mode.borrow_mut() = PickerMode::Rename(sid);
        self.picker.show();
    }

    fn open_help(self: &Rc<Self>) {
        let lines: Vec<String> = KEYS.iter().map(|(k, what)| format!("{k:<18}{what}")).collect();
        self.picker.fill("keys   (esc to close)", None, &lines);
        *self.picker.mode.borrow_mut() = PickerMode::Help;
        self.picker.show();
    }

    fn close_picker(self: &Rc<Self>) {
        *self.picker.mode.borrow_mut() = PickerMode::Closed;
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
            PickerMode::Help => self.close_picker(),
            PickerMode::Accounts => {
                *self.picker.mode.borrow_mut() = PickerMode::Accounts;
                self.sign_in_picked(false);
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
        root.append(&title);
        root.append(&entry);
        root.append(&list);
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
            self.list.append(&l);
        }
        self.list.set_visible(!options.is_empty());
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
