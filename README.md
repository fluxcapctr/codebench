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
| `Ctrl+Shift+O` | Add a project folder |
| `Ctrl+Shift+R` | Rename the task |
| `Ctrl+Shift+W` | Stop the task's agent (select it again to resume) |
| `Ctrl+Shift+D` | Delete the task, or remove the project (press twice) |
| `Alt+Up/Down` | Move through projects and tasks |
| `Ctrl+Shift+B` | Toggle the sidebar |
| `Ctrl+Shift+C/V` | Copy and paste |

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
- Theme: `~/.local/state/omarchy/current/theme/` (`colors.toml`, `ghostty.conf`)
- Font: `~/.config/ghostty/config`
