# codebench

A terminal-first workspace for running coding agents across projects. Every
agent runs as its normal interactive CLI in a real terminal, so it is exactly
as fast and capable as your terminal. Codebench adds the organization around
it: projects, one task per conversation, live status, and resume.

It takes its colors and font from the active Omarchy theme and restyles itself
live when you switch themes.

## Install

On Arch or Omarchy:

```sh
sudo pacman -S vte4
git clone https://github.com/fluxcapctr/codebench && cd codebench
./install.sh
```

Or build the package in `packaging/aur` with `makepkg -si`.

Codebench drives the agent CLIs you already have (Claude Code, Codex,
Gemini CLI, Grok Build, OpenCode, Antigravity), each on its own login.
lazygit is used for the git pane.

## Use

`Ctrl+Shift+O` adds a project. Type a name and pick **+ new project** to
create `~/code/<name>` with git, pick one of your `~/code` folders that is not
a project yet, or browse to any folder. From a terminal:

```sh
codebench ~/code/some-project   # add a folder as a project and open it
```

| Key | Action |
| --- | --- |
| `Ctrl+Shift+N` | New task in the current project (pick the agent) |
| `Ctrl+Shift+P` | Workflows: run one, edit it (`Ctrl+E`), or type a name to create one |
| `Ctrl+Shift+E` | Edit the project's notes in `$EDITOR` |
| `Ctrl+Shift+G` | Git (lazygit) for the task's folder or the project; `q` returns |
| `Ctrl+Shift+F` | Browse files (yazi if installed, else your editor) in the task's folder |
| `Ctrl+Shift+M` | Merge a worktree task back (press twice) |
| `Tab` | In the new task box: give the task its own git worktree |
| `Ctrl+Shift+H` | Hand off: the agent writes a note, the task continues in a fresh session |
| `Ctrl+Shift+O` | Add a project: new (creates the folder with git), one of your folders, or browse |
| `Ctrl+Shift+I` | Import past Claude and Codex chats for this project |
| `F2` | Accounts: logins, limits, sign in, switch account, update agents (also `Ctrl+Shift+U`, which fcitx5 takes on Omarchy) |
| `Ctrl+Shift+L` | Link the project's notes to a folder, such as one in your Obsidian vault |
| `Ctrl+Shift+J` | Open the brief in Obsidian (asks for a vault folder first if the notes are not in one) |
| `Ctrl+Shift+R` | Rename the task or project (the folder keeps its name) |
| `Ctrl+Shift+W` | Stop the task's agent (select it again to resume) |
| `Ctrl+Shift+D` | Delete the task, or remove the project (press twice) |
| `Ctrl+Shift+A` | Show or hide handed-off tasks |
| `Ctrl+Shift+K` | Give this project's agents a browser (on or off) |
| `Ctrl+Shift+Y` | Phone access: on or off, its address, paired phones |
| `Ctrl+Shift+S` | Split: pin this task on the right, pick another for the left; again to unpin |
| `Ctrl+Shift+←/→` | Focus the left or right side of a split |
| `Ctrl+Shift+B` | Toggle the sidebar (drag its edge to resize) |
| `Ctrl+Shift+T` | Toggle the project tabs and view bar |
| `Ctrl+Shift+C/V` | Copy and paste |
| `Ctrl+1…9`, `Ctrl+PgUp/PgDn` | Switch project tab |
| `Alt+1…7` | Views: tasks, files, notes, workflows, artifacts, git, processes |
| `Alt+Up/Down` | Move through the project's tasks |
| `F1` | All commands: type to filter, Enter runs one |

## Layout

Projects are **tabs** across the top (`Ctrl+1…9`, `Ctrl+PgUp/PgDn`); each
shows how many of its tasks need you. Under them, the current project's
**views** (`Alt+1…7`): tasks, files, notes, workflows, artifacts, git and
processes. The sidebar lists the project's tasks (`Alt+↑/↓`). Switching
back to a tab reopens the task you had open there.

- **notes** lists the brief, your own notes (+ new note), handoffs and
  workflow runs; Enter opens one in your editor.
- **workflows** runs a workflow (Enter) or edits it (`e`).
- **processes** shows servers started from the project's folders, like a
  dev server an agent left running: Enter opens it in the browser, `x`
  stops it.

## Project board

Selecting a project shows its board: every task with its status and context
size, its current plan as a checklist (from Claude Code's task list or
Codex's plan), and the latest thing the agent said. Below that are the
project's workflows.

## Keeping context small

Each task is its own conversation. Instead of one long chat per project:

- **Brief.** Every project has a `brief.md`. Claude and Codex tasks get it as
  extra instructions when they start, plus read and write access to the notes
  folder, so a new task starts informed without old history.
- **Handoff.** When a task gets long, `Ctrl+Shift+H` has the agent write a
  handoff note, then continues the task in a fresh session that starts by
  reading it. The old task is archived, not deleted.
- **Context size.** Claude tasks show how many tokens their conversation
  carries. It turns yellow at 100k and red at 200k.

Notes are plain markdown. By default they live in
`~/.local/share/codebench/notes/`; link them into an Obsidian vault with
`Ctrl+Shift+L` and the brief and handoffs come along.

## Git and worktrees

Each git project has a `± git` row showing its branch and uncommitted file
count. `Ctrl+Shift+G` opens lazygit for whatever you are looking at, for
diffs, commits and merge conflicts.

For work that should not collide with other agents, press `Tab` in the new
task box to give the task its **own worktree**: a separate checkout on a new
`cb/<task>` branch, kept under `~/.local/share/codebench/worktrees/`. The
sidebar marks it `⑂` with how many files it has changed. When it is done,
`Ctrl+Shift+M` commits its work and merges the branch into the project;
conflicts open in lazygit. Deleting the task removes its worktree and branch.
A handoff keeps working in the same worktree.

## Workflows

A workflow is a saved prompt: a markdown file in the project's
`notes/workflows/` folder (or `~/.config/codebench/workflows/` for ones every
project can use).

```markdown
---
agent: claude
schedule: weekdays 09:00
---
Check for outdated dependencies and summarize what would change.
```

Each run is a new task named after the workflow and time; earlier runs of the
same workflow are archived. Schedules can be `hourly`, `every 6h`,
`daily 18:30`, `weekdays 09:00`, `weekends 10:00` or `mon,thu 08:00`. A new
schedule waits for its next time instead of firing at once.

Global workflows can be scheduled too: add `projects: all` (or
`projects: compy, omaform`) to say which projects the schedule runs in.

**Starter set.** `workflows/` in this repo holds nine ready-made workflows
(review changes, commit my work, release notes, dependency check, run tests
and fix, refresh brief, whats changed, codex second opinion, security
review). `install.sh` copies them to `~/.config/codebench/workflows/` if you
have none yet.

**Collections.** Share workflow sets through git:

```sh
codebench workflows add https://github.com/someone/their-workflows
codebench workflows update    # pull every collection
codebench workflows           # list global and collection workflows
```

A collection's workflows show in every project, marked with its name.

Scheduled workflows run while Codebench is open. To also run them while it is
closed:

```sh
codebench schedule on    # systemd user timer, checks every 15 minutes
codebench schedule off
```

Background runs use `claude -p` or `codex exec`, save the answer to
`notes/runs/`, send a notification, and show up as a task you can open and
continue.

## In the Omarchy bar

`plugin/` is an Omarchy shell bar widget: a `>_` that shows how many tasks
need you (or how many are working), with a dropdown listing them; click one
to jump straight to it. `install.sh` links it into
`~/.config/omarchy/plugins`; add it to the bar with
`omarchy bar put io.github.fluxcapctr.codebench`. It reads
`codebench status --follow`, and `codebench --task <id>` opens a task.

While any agent is working, Codebench holds a `systemd-inhibit` sleep lock
so the machine does not suspend mid-task. The screen still locks as usual.

Drop files onto a task to type their paths into it, or onto the notes or
artifacts view to copy them into that folder. Screenshots on the clipboard
paste into Claude Code with its own `Ctrl+V`.

## Artifacts

Ask an agent to show you something (a chart, a page mockup, a diagram, a
document) and it uses the `show_artifact` tool: the artifact opens in a
viewer window next to Codebench and reloads live as the agent revises it.
HTML (scripts allowed), SVG, Mermaid, markdown, images and PDF all work.
Each project's artifacts are listed under its **◆ artifacts** row and kept
in `notes/artifacts/`, so Obsidian sees them too; the phone app lists them
as well.

Artifacts are served from their own port with a random key, and every page
runs in a browser sandbox, so an artifact's scripts cannot reach Codebench
or the phone app. To open them on the phone, also run
`tailscale serve --bg --https=8443 47824`.

## A browser for agents

`Ctrl+Shift+K` gives a project's Claude and Codex tasks a browser: Playwright's
MCP server driving a visible Chromium window, so an agent can open your dev
server, click, fill forms, read the page and take screenshots while you
watch. Each task keeps its own browser profile, so logins survive resumes
and two tasks never fight over one. It is off by default because its tools
add to every task's context; the sidebar marks projects that have it with ◍.
Needs node (`npx`); uses your installed Chromium.

## Accounts, limits and past chats

The bottom of the sidebar shows which agents are signed in, and for Claude
and Codex how much of their tightest rate limit is used (yellow past 60%,
red past 80%). `F2` lists every limit: Claude's session and
weekly limits from `claude /usage`, and Codex's 5-hour and weekly windows
from its latest session. They refresh every ten minutes, and the bar
widget's dropdown shows them too.

The sidebar also shows which agents are signed in. `F2`
lists each agent's login (for Claude, the plan and organization), signs in
with Enter, or switches account with `s` (sign out, then in), in a pane that
closes when done. `codebench accounts` prints the same from a terminal.

`Ctrl+Shift+I` lists the Claude Code and Codex conversations you had in the
project's folder outside Codebench; Enter brings one in as a task that
resumes where it left off.

## Agents talking to each other

Every Claude and Codex task gets a `codebench` MCP server with four tools:

| Tool | What it does |
| --- | --- |
| `list_tasks` | The project's other tasks, their agent, status and context size |
| `read_task` | The latest messages of another Claude task, or its handoff note |
| `send_to_task` | Sends a message into another task. It is typed in when that agent is free, queued (shown as ✉) while it is busy, and starts the task if it is stopped |
| `start_task` | Starts a new task on the project, with any agent, from an opening prompt |

So you can tell one agent "have a Codex task review this" or "ask the
refactor task what it changed", without copying anything between windows.
Tasks only see tasks in their own project.

## Agents

Claude Code, Codex, Gemini CLI, Grok Build, OpenCode, Antigravity and a plain
shell, whichever are installed. Each uses its own login, so your existing
subscriptions apply.

- **Claude Code** tasks keep a fixed session id and resume with `--resume`.
  Status (working, needs you, done) comes from hooks passed with `--settings`,
  layered on top of your own settings.
- **Codex** reports "done" through its `notify` hook. Codebench finds the
  task's own Codex conversation by a marker in its instructions and resumes
  exactly that one.
- Gemini, Grok, OpenCode and Antigravity have no status hooks, so Codebench
  watches the screen: after you send a prompt the task is working while the
  screen keeps changing, and done once it has been still for five seconds.
  A terminal bell means it needs you.

## Files

- `~/.config/codebench/state.json`: projects and tasks
- `~/.cache/codebench/status/`: per-task status written by agent hooks
- `~/.local/share/codebench/notes/`: default notes folders
- `~/.cache/codebench/requests/`: messages and new-task requests from agents
- `~/.cache/codebench/tasks.json`: live task status, read by the MCP server
- `~/.config/codebench/schedule.json`: when each scheduled workflow last ran
- Theme: `~/.local/state/omarchy/current/theme/` (`colors.toml`, `ghostty.conf`)
- Font: `~/.config/ghostty/config`

## On your phone

Codebench can serve a phone app over [Tailscale](https://tailscale.com): an
inbox of tasks that need you, each task's conversation and live terminal
with one-tap keys (yes, no, arrows, numbers) and a reply box, new tasks,
workflows, your plan limits, and push notifications when a task needs you
or finishes. It installs to the home screen.

1. In the Tailscale admin console, turn on **HTTPS certificates** (DNS page).
2. Let your user manage `tailscale serve`: `sudo tailscale set --operator=$USER`.
3. `tailscale serve --bg 47823`, then `Ctrl+Shift+Y` in Codebench and turn
   phone access on.
4. On the phone (on your tailnet), open the address shown, pair it, and
   approve the code in Codebench. Then turn on notifications in its settings.

The server listens on 127.0.0.1 only; `tailscale serve` is what puts it on
your tailnet, over HTTPS. A phone needs a token from pairing, which you
approve on the desktop; tokens are stored hashed and can be revoked in
`Ctrl+Shift+Y`. Requests arriving through Tailscale must come from this
machine's Tailscale login. The phone can only talk to agent tasks, never a
plain shell, editor, git or sign-in pane.

## Trust and safety

Codebench runs agents with the permissions you already gave them; it adds no
sandbox of its own. Things worth knowing:

- **Agents can prompt each other.** Through the MCP tools a task can type a
  message into another task on the same project, or start a new one. A task
  never reaches other projects.
- **Notes are instructions.** The brief is given to every Claude and Codex
  task, and workflows are prompts. If the notes folder lives in a shared or
  synced place (such as an Obsidian vault other agents write to), whoever can
  edit it can steer your agents, and scheduled workflows will run what it
  says without you watching.
- **Background runs are unattended.** `codebench schedule on` runs due
  workflows while you are away, with the agent's usual permission settings.
- **Phone access** lets a paired phone type into your agent tasks. Only pair
  your own phones, and revoke any you lose from `Ctrl+Shift+Y`.
- **Local files.** Codebench's folders are created readable by you only,
  since anything that can write to them can prompt your agents.
