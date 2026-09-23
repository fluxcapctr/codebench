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

```sh
codebench ~/code/some-project   # add a project and open it
```

| Key | Action |
| --- | --- |
| `Ctrl+Shift+N` | New task in the current project (pick the agent) |
| `Ctrl+Shift+P` | Workflows: run one, edit it (`Ctrl+E`), or type a name to create one |
| `Ctrl+Shift+E` | Edit the project's notes in `$EDITOR` |
| `Ctrl+Shift+G` | Git (lazygit) for the task's folder or the project; `q` returns |
| `Ctrl+Shift+M` | Merge a worktree task back (press twice) |
| `Tab` | In the new task box: give the task its own git worktree |
| `Ctrl+Shift+H` | Hand off: the agent writes a note, the task continues in a fresh session |
| `Ctrl+Shift+O` | Add a project folder |
| `Ctrl+Shift+I` | Import past Claude and Codex chats for this project |
| `Ctrl+Shift+U` | Accounts: logins per agent, sign in, switch account |
| `Ctrl+Shift+L` | Link the project's notes to a folder, such as one in your Obsidian vault |
| `Ctrl+Shift+J` | Open the brief in Obsidian |
| `Ctrl+Shift+R` | Rename the task |
| `Ctrl+Shift+W` | Stop the task's agent (select it again to resume) |
| `Ctrl+Shift+D` | Delete the task, or remove the project (press twice) |
| `Ctrl+Shift+A` | Show or hide handed-off tasks |
| `Ctrl+Shift+S` | Split: pin this task on the right, pick another for the left; again to unpin |
| `Ctrl+Shift+←/→` | Focus the left or right side of a split |
| `Ctrl+Shift+B` | Toggle the sidebar |
| `Ctrl+Shift+C/V` | Copy and paste |
| `Alt+Up/Down` | Move through projects, notes and tasks |
| `F1` | All keys |

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

Scheduled workflows run while Codebench is open. To also run them while it is
closed:

```sh
codebench schedule on    # systemd user timer, checks every 15 minutes
codebench schedule off
```

Background runs use `claude -p` or `codex exec`, save the answer to
`notes/runs/`, send a notification, and show up as a task you can open and
continue.

## Accounts and past chats

The bottom of the sidebar shows which agents are signed in. `Ctrl+Shift+U`
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
- Other agents report "needs you" when they ring the terminal bell.

## Files

- `~/.config/codebench/state.json`: projects and tasks
- `~/.cache/codebench/status/`: per-task status written by agent hooks
- `~/.local/share/codebench/notes/`: default notes folders
- `~/.cache/codebench/requests/`: messages and new-task requests from agents
- `~/.cache/codebench/tasks.json`: live task status, read by the MCP server
- `~/.config/codebench/schedule.json`: when each scheduled workflow last ran
- Theme: `~/.local/state/omarchy/current/theme/` (`colors.toml`, `ghostty.conf`)
- Font: `~/.config/ghostty/config`

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
- **Local files.** Codebench's folders are created readable by you only,
  since anything that can write to them can prompt your agents.
