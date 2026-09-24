# Codebench codebase review

Reviewed 2026-09-24. Base commit: `a6164c91e582e6569676d8f51fdfe7cc5f563854`, including the working tree changes present during review in `README.md`, `src/accounts.rs`, `src/agents.rs`, `src/history.rs`, and `src/ui.rs`. Line numbers refer to that working tree. No application code was changed.

## Summary

18 actionable findings: **5 P1**, **10 P2**, **3 P3**. The most important fixes concern lost file contents, stale editor saves, invalid state recovery, device revocation, and input going to the wrong split terminal.

The existing 24 tests pass. Six additional backend probes reproduce bugs against an isolated copy of the actual source. Two phone probes reproduce bugs using the actual JavaScript with a minimal DOM double. These probes intentionally assert the broken behavior: passing confirms the finding, not application correctness.

**Evidence labels:** Reproduced = exercised in the supplied probes. Source-confirmed = a concrete control/data-flow defect, with manual reproduction steps supplied; not exercised in the live UI. Layout findings require visual checks at the suggested sizes.

## Findings

### R01 · P1 · Showing an artifact from its own saved path empties the file

**Reproduced.** `src/artifacts.rs:86–110` (`save`).

`save` derives a destination from the title, then blindly calls `std::fs::copy`. If an agent edits `notes/artifacts/chart.html` and calls `show_artifact` with that path and title `chart`, source and destination are the same file. On this machine the call succeeds and leaves a zero-byte artifact. This is a natural workflow when revising an existing artifact.

**Fix:** Detect identical files, including symlink/hardlink aliases, and treat showing an existing destination as a no-op save. Stage other copies in a separate temporary file before replacement.

**Acceptance:** Re-show an artifact from its own path and an alias; bytes must remain intact. Verify normal replacement and live reload still work. Probe: `review_artifact_copy_to_self_erases_content`.

### R02 · P1 · Opening another note can overwrite an external edit to the previous note

**Source-confirmed.** `src/ui.rs:3453–3489` (`edit_file`, `save_editor`), `1554`, `1623`, `2097` (navigation).

`save_editor` interprets any difference between disk and the editor buffer as an unsaved local edit. There is no dirty flag or loaded-version check. Opening another file calls it unconditionally. Open note A, switch to a task or another view, let an agent/Obsidian update A, then open note B: the old buffer overwrites A before B loads, even if the user never edited A. Navigation also leaves `editor_path` set. Separately, there is no close/shutdown save handler for the last 600 ms of debounced edits, and write failures are swallowed.

**Fix:** Track local dirty state and the loaded disk version; save only local edits and handle external conflicts. Flush pending edits on close and deliberate navigation; report failed saves.

**Acceptance:** Opening/closing an untouched note never writes it. External edits survive navigation. Closing immediately after typing saves the final characters. A failed save remains visibly unsaved.

### R03 · P1 · Invalid project state silently becomes an empty state and is overwritten

**Reproduced.** `src/store.rs:141–162` (`State::load`, `save`).

All read/parse failures become `State::default()`. After a malformed `state.json`, the application looks like a new installation; adding a project saves over the recoverable old file. Persistence errors are also ignored, so changes may appear saved until restart.

**Fix:** Distinguish a missing file from unreadable/invalid state, retain a recoverable copy, and prevent normal saves over failed loads. Return and surface persistence errors.

**Acceptance:** Invalid JSON and permission errors produce a recovery/error state, preserving original bytes. A genuinely missing file still starts normally. Probe: `review_corrupt_state_is_silently_replaced`.

### R04 · P1 · Revoking one phone logs out every phone and does not enforce revocation on its existing socket

**Source-confirmed.** `src/remote.rs:262–267`, `653–706`; `src/phone/app.js:56–61`, `114`.

`Remote::revoke` broadcasts an untargeted `{"type":"revoked"}`. Every official client handles that by deleting its own token, including phones that remain authorized. Conversely, an already authenticated client that ignores this message can continue receiving state and watched-screen events: `live_socket` never remembers/rechecks the device identity or closes the revoked connection. HTTP authorization correctly rejects subsequent requests from the revoked device; the defect is ongoing WebSocket reads.

**Fix:** Associate sockets with device hashes, close/revalidate only the revoked device, and enforce the decision server-side before further data delivery.

**Acceptance:** Pair A and B; revoke A. B stays paired and connected. A's existing socket receives no subsequent task state/screens even if it ignores the notification.

### R05 · P1 · Split-view paste and stop actions target the left selection rather than the focused terminal

**Source-confirmed.** `src/ui.rs:2054–2094`, `2519–2522`, `3765–3785`, `4002–4006`.

Pin A on the right, select B on the left, then focus A using the mouse or Ctrl+Shift+Right. Focus changes without updating `current_key`; `current_term` still resolves B. Ctrl+Shift+V sends clipboard contents to B, copy reads B, and stop targets B. If B is a shell, a multiline paste can have consequences beyond a UI mismatch.

**Fix:** Resolve terminal input/clipboard/stop actions from actual focus. Keep the selected task and the focused terminal separate where needed for project navigation.

**Acceptance:** With two distinguishable tasks, exercise copy, paste, stop, and handoff from each pane; each action must target the intended focused task. Include a left-side board with a right-side terminal.

### R06 · P2 · Phone live state updates erase the new-task form

**Reproduced with DOM double.** `src/phone/app.js:115`, `341–387`.

Every state event calls `render`. `newView` reconstructs the project selector, agent selector, name, and prompt with defaults. While the user writes a prompt, any task changing status can erase it, reset selections, and remove focus. Only the task reply view has draft-preservation logic.

**Fix:** Persist form state across updates or update only relevant state-dependent elements without recreating the active form.

**Acceptance:** Type a multiline draft, select a nondefault project/agent, and deliver several state messages. Values, focus, and caret remain intact. See `phone-probes.cjs`.

### R07 · P2 · Linking the notes folder strands most project content

**Reproduced for copying; source-confirmed for UI consequence.** `src/notes.rs:143–156`; `src/ui.rs:3951–3984`.

The UI changes and saves `project.notes` before copying. `carry_over` transfers only `brief.md` and `handoffs/`; personal notes, `workflows/`, `runs/`, and `artifacts/` remain in the previous folder. They disappear from the application, and project workflow schedules stop being discovered. Originals remain on disk, so this is recoverable. Copy failures still result in a success message.

**Fix:** Migrate the complete notes tree with a conflict policy and error handling before committing the new location. Update workflow path references/bookkeeping as appropriate; preserve existing destination files.

**Acceptance:** Link a populated project to a new folder; all content remains discoverable and scheduled workflows keep their identity. A failed copy retains the old location. Probe: `review_link_notes_omits_user_content`.

### R08 · P2 · Gemini, Grok, and Antigravity never receive opening task/workflow prompts

**Reproduced at argv construction.** `src/agents.rs:158–258`, especially the fallback match arm; `src/ui.rs:3547`, `3664`, `3088`.

These agents fall through to an argv containing only the executable. `session.prompt` and `extra` are ignored. Starting them through a workflow, the phone, or MCP therefore opens an interactive agent without the requested instructions. The caller reports that the task started.

**Fix:** Add explicit per-agent prompt support or queue the opening prompt after a reliable readiness signal. Reject unsupported launch modes instead of dropping instructions.

**Acceptance:** Use a recording fake executable/readiness fixture for each agent and verify the opening prompt arrives exactly once. Probe: `review_agent_opening_prompts_are_dropped`.

### R09 · P2 · Daily schedules use the wrong local hour on daylight-saving transition days

**Reproduced.** `src/workflow.rs:248–277` (`local_day`, `latest_slot`).

The slot is computed by adding elapsed seconds to local midnight. On a clock-change day, elapsed hours and wall-clock hours differ. With `TZ=America/Los_Angeles`, `daily 09:00` resolves to 10:00 on March 8, 2026; the analogous fall transition shifts the time early.

**Fix:** Construct the desired local hour/minute in `tm`, with `tm_isdst = -1`, then call `mktime`. Define behavior for skipped/repeated local times.

**Acceptance:** Check both transition days, surrounding normal days, and repeated/nonexistent-hour policies. Probe: `review_dst_wall_clock_is_shifted`.

### R10 · P2 · Cold phone task links never acquire chat tabs

**Reproduced with DOM double.** `src/phone/app.js:291–337`, particularly the same-task early return.

Opening `/#/task/<id>` before the first state message builds the task view with `t == null`, chooses screen mode, and omits chat tabs. When state arrives identifying a Claude/Codex task, `taskView` updates only status and returns. Chat stays unavailable until the user leaves and re-enters. Push notifications link directly to this route.

**Fix:** Reconcile task capabilities when metadata arrives, or wait for task metadata before constructing the task view while preserving reply drafts.

**Acceptance:** Open a fresh tab directly to a Claude and a Codex task, with delayed state; chat tabs appear after loading. See `phone-probes.cjs`.

### R11 · P2 · Pressing the edit key in the process list terminates a process

**Source-confirmed.** `src/ui.rs:1027–1044`, `1845–1877`.

The shared list key handler treats `e`, `x`, and Delete identically and invokes the row's `alt` action. Process rows define that action as `StopProcess`. Thus `e` kills a server although the UI says `x stops`. Workflow rows likewise open the editor on Delete or `x`.

**Fix:** Store the alternate action's actual key, or dispatch by view/action type. An editing shortcut must not invoke a termination action.

**Acceptance:** On process rows only the documented stop shortcut stops; `e` has no destructive effect. On workflow rows only documented edit shortcuts edit.

### R12 · P2 · Returning to another phone page can be undone by an older artifact request

**Source-confirmed.** `src/phone/app.js:391–414` (`artifactsView`).

`artifactsView` awaits an API request and then unconditionally replaces `view`. Open artifacts on a slow connection, then navigate to a task or the new-task form before the response arrives. The old response replaces the current page with artifact rows while the route/title still describe the new page.

**Fix:** Use a route/render generation token or cancel stale requests before committing DOM updates.

**Acceptance:** Delay the artifacts response, navigate elsewhere, then resolve it; the current view and draft must remain untouched.

### R13 · P2 · Dropped files silently overwrite existing notes/artifacts

**Source-confirmed.** `src/ui.rs:1580–1601` (`drop_into_view`).

Dropping a file copies it to `dest.join(original_name)` with no collision check. A dropped `brief.md` replaces the project's existing instructions immediately. Dropping a file already in the destination onto that view also follows the same self-copy pattern reproduced in R01.

**Fix:** Detect same-file copies and provide a collision policy (rename or explicit replace) for different files with the same name. Report copy errors.

**Acceptance:** Drop a same-named note, the original note itself, and two files with the same basename. Existing content must survive unless replacement was explicitly chosen.

### R14 · P2 · Freshly created projects offer a worktree that cannot be created

**Reproduced for Git operations; source-confirmed for UI eligibility.** `src/ui.rs:3908–3921`, `4108–4124`; `src/git.rs:27–29`, `55–63`.

New project creation runs `git init` without a first commit. The new-task picker enables isolation based solely on `is_repo`. Choosing a worktree then invokes `git worktree add ... main`, which fails because the branch has no commit yet. The normal first-project/first-isolated-task flow is broken, though the failure is displayed.

**Fix:** Check for a valid HEAD before offering isolation, with a clear initial-commit action or explanation. Avoid silently committing user files.

**Acceptance:** A freshly created empty project has a usable, understandable isolated-task flow. Probe: `review_new_repository_cannot_create_worktree`.

### R15 · P2 · The project board stops updating after it is opened

**Source-confirmed.** `src/ui.rs:1893–1928`, `2423–2469`.

`show_project` reads progress once and writes the board. Later status changes rebuild the sidebar and phone snapshot but never refresh the board; terminal output also does not refresh it. Leave the board visible while a task completes or advances its plan: the board retains old status, plan, and latest message until reopened.

**Fix:** Refresh the visible board on relevant status/transcript changes, throttling transcript reads and discarding stale asynchronous results.

**Acceptance:** Observe the board while a task starts, updates its plan, and finishes. It updates without navigation and without excessive transcript rescans.

### R16 · P3 · Phone code blocks inherit pairing-code typography

**Source-confirmed CSS defect; visual validation pending.** `src/phone/app.css:63`, `72`; `src/phone/app.js:232`.

Both the pairing number and fenced code blocks use class `code`. The later `.msg pre.code` rule overrides size but leaves `letter-spacing: 8px`, bold weight, and accent color from the pairing style. Normal code becomes very widely spaced and unnecessarily hard to read.

**Fix:** Scope pairing typography to `.pair .code` or use distinct class names; set normal code-block typography explicitly.

**Acceptance:** A multiline fenced code sample uses normal monospace spacing while the six-digit pairing number keeps its intended presentation.

### R17 · P3 · Project tabs and pickers have no overflow handling

**Source-confirmed structure; visual validation pending.** `src/ui.rs:702–715`, `1328–1359`, `4367–4431`.

Project tabs are appended to an unscrolled horizontal box next to account text, with no ellipsis/overflow menu. Picker rows are appended to an unscrolled list with no height cap. Many projects, long names, or the supported 60 imported conversations can exceed the available window; controls can become inaccessible or force excessive minimum size.

**Fix:** Add horizontal overflow handling for tabs, bounded scrolling for pickers, and truncation with accessible full labels.

**Acceptance:** Visually check 20 long project names and a 60-row import picker at 1280×720 and a half-width tiled window. Every option remains reachable by keyboard and pointer.

### R18 · P3 · Phone screen subscriptions accumulate for the lifetime of the server

**Source-confirmed.** `src/remote.rs:76–77`, `433`, `475`, `682`; `src/ui.rs:2992–3020`; `src/phone/app.js:163`, `342`.

Screen/open/watch requests insert task IDs into `Shared.watched`, but nothing removes them on navigation, disconnect, or task deletion. Visiting many tasks causes all previously visited terminals to keep generating full HTML snapshots on changes, even when no phone is watching. `screens` also retains entries. This increases GTK-thread work and retained data as usage grows.

**Fix:** Track active watchers per connection with cleanup on unwatch/disconnect. Separate one-shot captures from subscriptions and remove deleted-task cache entries.

**Acceptance:** Visit several tasks, disconnect all phones, then generate terminal output. No subscription captures should continue; reconnecting restores only the selected task.

## Additional checks worth scheduling

These are follow-ups, not counted as confirmed findings above:

- **Programmatic input/status:** `type_into` and phone keys use `feed_child`, while non-Claude working status is tied to VTE `commit` signals. Opening prompts passed as argv also start with status Idle. Exercise a fake agent through phone/MCP/workflows and verify working status, message queuing, and the sleep inhibitor. Avoid assuming programmatic input emits the same signals as keyboard input.
- **State concurrency:** GUI state and `run-due` perform whole-file read/modify/write with a shared temporary filename and no lock. Test opening the GUI during a multi-workflow headless run and concurrent `run-due` invocations before claiming a specific loss/duplication scenario.
- **Session selection:** `agents::codex_session` searches for a task marker anywhere in the first 512 KiB and selects the newest matching file. Test sibling/child conversations containing copied instructions; confirm resume chooses the original session, then persist the discovered conversation ID.
- **Resume across all supported agents:** OpenCode/local explicitly start fresh and the Gemini/Grok/Antigravity fallback has no task-specific resume selection. Decide which agents genuinely support resumable Codebench tasks and make the UI promises match that matrix.
- **Artifact formats/viewers:** Exercise PDF rendering under the sandbox and Chromium window reuse with multiple artifacts. These need a real browser; no verified browser-specific failure is claimed here.

## Validation and coverage

Read all Rust modules, phone HTML/CSS/JavaScript/service worker/manifest, QML plugin and manifest, install script, package recipe, desktop entry, CI configuration, README, and bundled workflow prompts. Reviewed approximately 9,700 lines of Rust and phone implementation. Binary icon assets were not visually audited.

Executed:

```sh
cargo test --offline                          # 24 passed
cargo clippy --offline --all-targets -- -D warnings
node --check src/phone/app.js
node --check src/phone/sw.js
sh -n install.sh
bash -n packaging/aur/PKGBUILD
python3 docs/review/reproduce.py               # six reproduced backend defects
node docs/review/phone-probes.cjs              # two reproduced phone defects
```

Strict Clippy fails on nine style lints (unnecessary `sort_by`, needless lifetime, single-element loop, single-character `push_str`), not compiler errors or the functional findings. Existing CI builds/tests release mode but does not run strict Clippy. JavaScript and shell syntax checks pass.

The reproduction runner copies source to `/tmp/codebench-review-*`, redirects all Codebench state paths there, and reuses the repository Cargo target cache. It does not run agents, contact services, or touch real project state. It leaves the temporary fixtures available for inspection. Convert its bug-observation assertions into expected-correct regression tests while fixing the relevant code.

Limits: no interactive GTK session, real phone/Tailscale pairing, push delivery, agent login/update commands, package installation, release build, or dependency-advisory scan was performed. This is a broad source review with targeted execution, not a guarantee that all defects have been found.

## Suggested ownership for follow-up agents

| Work area | Findings | Key files |
| --- | --- | --- |
| File integrity and notes | R01, R02, R03, R07, R13 | `artifacts.rs`, `notes.rs`, `store.rs`, editor/drop portions of `ui.rs` |
| Remote server | R04, R18 | `remote.rs`, phone server integration in `ui.rs` |
| Phone UI | R06, R10, R12, R16 | `src/phone/app.js`, `app.css` |
| Desktop behavior | R05, R11, R15, R17 | `ui.rs` |
| Launching and scheduling | R08, R09, R14 | `agents.rs`, `workflow.rs`, `git.rs`, relevant `ui.rs` call sites |

Start with P1 findings. Since several fixes touch `ui.rs`, coordinate ownership of functions or use separate worktrees. Keep the pre-existing user changes intact. This report and its probes are prepared for sharing; no other agent was started or messaged during the review.
