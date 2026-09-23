# codebench

A terminal-first workspace for running coding agents across projects. Every
agent runs as its normal interactive CLI in a real terminal, so it is exactly
as fast and capable as your terminal. Codebench adds the organization around
it: projects, one task per conversation, live status, and resume.

It takes its colors and font from the active Omarchy theme and restyles itself
live when you switch themes.

## Install

```sh
sudo pacman -S vte4
./install.sh
```

## Use

```sh
codebench ~/code/some-project   # add a project and open it
```

| Key | Action |
| --- | --- |
| `Ctrl+Shift+N` | New task in the current project (pick the agent) |
| `Ctrl+Shift+E` | Edit the project's notes in `$EDITOR` |
| `Ctrl+Shift+H` | Hand off: the agent writes a note, the task continues in a fresh session |
| `Ctrl+Shift+O` | Add a project folder |
| `Ctrl+Shift+L` | Link the project's notes to a folder, such as one in your Obsidian vault |
| `Ctrl+Shift+J` | Open the brief in Obsidian |
| `Ctrl+Shift+R` | Rename the task |
| `Ctrl+Shift+W` | Stop the task's agent (select it again to resume) |
| `Ctrl+Shift+D` | Delete the task, or remove the project (press twice) |
| `Ctrl+Shift+A` | Show or hide handed-off tasks |
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

## Agents

Claude Code, Codex, Gemini CLI, Grok Build, OpenCode, Antigravity and a plain
shell, whichever are installed. Each uses its own login, so your existing
subscriptions apply.

- **Claude Code** tasks keep a fixed session id and resume with `--resume`.
  Status (working, needs you, done) comes from hooks passed with `--settings`,
  layered on top of your own settings.
- **Codex** reports "done" through its `notify` hook and resumes with
  `codex resume`.
- Other agents report "needs you" when they ring the terminal bell.

## Files

- `~/.config/codebench/state.json`: projects and tasks
- `~/.cache/codebench/status/`: per-task status written by agent hooks
- `~/.local/share/codebench/notes/`: default notes folders
- Theme: `~/.local/state/omarchy/current/theme/` (`colors.toml`, `ghostty.conf`)
- Font: `~/.config/ghostty/config`
