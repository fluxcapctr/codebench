//! The window: a project and task sidebar on the left, the selected task's
//! terminal on the right, a header line above and a key-hint line below.

use crate::agents;
use crate::notes;
use crate::store::{self, Session, State};
use crate::theme::{self, Theme};
use crate::usage;
use gtk::{gdk, gio, glib, pango, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use vte::prelude::*;

pub const APP_ID: &str = "co.ericstevens.codebench";

const HINTS: &str = "^⇧N new task   ^⇧E notes   ^⇧H handoff   ^⇧O add project   alt ↑↓ switch   F1 all keys";

const KEYS: &[(&str, &str)] = &[
    ("Ctrl+Shift+N", "new task in this project"),
    ("Ctrl+Shift+E", "edit project notes (brief and handoffs)"),
    ("Ctrl+Shift+H", "hand off: write a note, continue in a fresh session"),
    ("Ctrl+Shift+O", "add a project folder"),
    ("Ctrl+Shift+L", "link notes to a folder, e.g. in your Obsidian vault"),
    ("Ctrl+Shift+J", "open the brief in Obsidian"),
    ("Ctrl+Shift+R", "rename task"),
    ("Ctrl+Shift+W", "stop task (select it again to resume)"),
    ("Ctrl+Shift+D", "delete task or remove project (press twice)"),
    ("Ctrl+Shift+A", "show or hide handed-off tasks"),
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
}

#[derive(Clone, PartialEq, Eq)]
enum Row {
    Project(String),
    Notes(String),
    Session(String),
}

fn notes_key(pid: &str) -> String {
    format!("notes:{pid}")
}

enum PickerMode {
    Closed,
    NewTask,
    Rename(String),
    Help,
}

struct Picker {
    root: gtk::Box,
    title: gtk::Label,
    entry: gtk::Entry,
    list: gtk::ListBox,
    mode: RefCell<PickerMode>,
    /// Agent ids, one per list row, in NewTask mode.
    items: RefCell<Vec<&'static str>>,
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
        if let Some(arg) = cmd.arguments().get(1) {
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
        sidebar_box.append(&subs);

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

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        let picker = Picker::build();
        overlay.add_overlay(&picker.root);

        let footer = label("cb-footer");
        footer.set_text(HINTS);
        footer.set_ellipsize(pango::EllipsizeMode::End);

        main.append(&header);
        main.append(&overlay);
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
        self.picker.list.connect_row_activated(move |_, _| b.picker_accept());
        let picker_keys = gtk::EventControllerKey::new();
        picker_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let b = self.clone();
        picker_keys.connect_key_pressed(move |_, key, _, _| {
            let list = &b.picker.list;
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
            let row = row_with(&format!("<b>{}</b>{badge}", esc(&p.name)));
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
                let title = if s.archived {
                    format!("<span foreground='{}'>{} ↳</span>", theme.muted, esc(&s.title))
                } else {
                    esc(&s.title)
                };
                self.sidebar.append(&row_with(&format!(
                    "  <span foreground='{color}'>{glyph}</span> <span foreground='{}'>{:<8}</span>{title}{tokens}{word}",
                    theme.muted,
                    esc(&s.agent),
                )));
                rows.push(Row::Session(s.id.clone()));
            }
        }
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

    fn show_project(&self, pid: &str) {
        let Some(p) = self.state.borrow().project(pid).cloned() else { return };
        *self.current_project.borrow_mut() = Some(pid.to_string());
        *self.current_key.borrow_mut() = None;
        let live = p.sessions.iter().filter(|s| !s.archived).count();
        let tasks = match live {
            0 => "no tasks yet".to_string(),
            1 => "1 task".to_string(),
            n => format!("{n} tasks"),
        };
        let brief = if notes::brief(&p).is_some() { "brief written" } else { "no brief yet" };
        self.empty.set_text(&format!(
            "{}\n{}\n\n{tasks}   ·   {brief}\nnotes in {}\n\n^⇧N  new task      ^⇧E  edit notes",
            p.name,
            tilde(&p.path),
            tilde(&p.notes_dir()),
        ));
        self.stack.set_visible_child_name("empty");
        self.update_header();
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
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
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
            self.stack.set_visible_child_name(key);
            term.grab_focus();
        }
    }

    fn show_session(self: &Rc<Self>, sid: &str) {
        let Some(pid) = self.state.borrow().session(sid).map(|(p, _)| p.id.clone()) else { return };
        *self.current_project.borrow_mut() = Some(pid);
        *self.current_key.borrow_mut() = Some(sid.to_string());
        let id = sid.to_string();
        self.show_terminal(sid, move |b| b.launch(&id));
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

        let right = match (&row, project) {
            (Some(Row::Notes(_)), Some(p)) => tilde(&p.notes_dir()),
            (_, Some(p)) => match git_branch(&p.path) {
                Some(branch) => format!("{}   {branch}", tilde(&p.path)),
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
        self.style_terminal(&term);

        let b = self.clone();
        let id = key.to_string();
        term.connect_child_exited(move |_, _| {
            if let Some(r) = b.running.borrow().get(&id) {
                r.pid.set(None);
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

    /// Starts the task's agent, resuming its previous conversation if it has one.
    fn launch(self: &Rc<Self>, sid: &str) {
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
        let argv = agents::argv(&session, &project);
        self.spawn(sid, &session.agent, &argv, &project.path, &env);

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
        self.rebuild_sidebar();
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

        // Type the request, then press Enter separately so the TUI does not
        // treat the newline as part of a paste.
        term.feed_child(notes::handoff_request(&path).as_bytes());
        glib::timeout_add_local_once(Duration::from_millis(250), move || term.feed_child(b"\r"));
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
                gdk::Key::e => self.open_notes(),
                gdk::Key::h => self.start_handoff(),
                gdk::Key::o => self.add_project_dialog(),
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
        let vault = store::home().join("Documents/Mission Control");
        let start = if vault.is_dir() { vault } else { store::home() };
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
            Row::Notes(_) => return,
        };
        let armed = self
            .pending_delete
            .borrow()
            .as_ref()
            .is_some_and(|(id, at)| *id == key && at.elapsed() < Duration::from_secs(3));
        if !armed {
            *self.pending_delete.borrow_mut() = Some((key, Instant::now()));
            self.flash(match &target {
                Row::Project(_) => "press ^⇧D again to remove this project from codebench (files and notes are kept)",
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
            Row::Notes(_) => Vec::new(),
        };
        for key in &doomed {
            self.stop(key);
            if let Some(r) = self.running.borrow_mut().remove(key) {
                self.stack.remove(&r.term);
            }
            self.status.borrow_mut().remove(key);
        }

        let mut state = self.state.borrow_mut();
        let next = match &target {
            Row::Session(sid) => {
                let pid = state.session(sid).map(|(p, _)| p.id.clone());
                if let Some(p) = pid.as_deref().and_then(|id| state.project_mut(id)) {
                    p.sessions.retain(|s| &s.id != sid);
                }
                pid.map(Row::Project)
            }
            Row::Project(pid) => {
                state.projects.retain(|p| &p.id != pid);
                state.projects.first().map(|p| Row::Project(p.id.clone()))
            }
            Row::Notes(_) => None,
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
        let name = self.state.borrow().project(&pid).map(|p| p.name.clone()).unwrap_or_default();
        let agents = agents::installed();
        self.picker.fill(
            &format!("new task in {name}"),
            Some(("task name (optional)", "")),
            &agents.iter().map(|a| format!("{:<9}{}", a.id, a.label)).collect::<Vec<_>>(),
        );
        *self.picker.items.borrow_mut() = agents.iter().map(|a| a.id).collect();
        *self.picker.mode.borrow_mut() = PickerMode::NewTask;
        self.picker.show();
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
                let id = uuid::Uuid::new_v4().to_string();
                {
                    let mut state = self.state.borrow_mut();
                    let Some(project) = state.project_mut(&pid) else { return };
                    let title = if text.is_empty() {
                        format!("task {}", project.sessions.len() + 1)
                    } else {
                        text
                    };
                    project.sessions.push(Session {
                        id: id.clone(),
                        agent: agent.to_string(),
                        title,
                        created: store::now(),
                        launched: false,
                        prompt: None,
                        archived: false,
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
