# RFC 0022: Agent dock — a first-class, click-to-focus Claude Code panel

- **Status:** **Proposed**
- **Created:** 2026-06-28
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (pluggable modules), RFC 0004 (per-output surfaces),
  RFC 0006 (WASM plugins — the prototype this promotes), RFC 0010 (motion)

## Summary

Promote the `claude` agent meter from a hover-popup WASM plugin to a **first-class native
module rendered in an always-on vertical dock**, and make **each agent row click-to-focus**:
one click jumps to the workspace/output the agent's terminal is on and focuses its window.
Reuse the existing pure brain (`crates/claude-logic`) verbatim; only the I/O shell changes
(real `std::fs` + the shared sway connection instead of the cap-std sandbox). Fix the
`$0/hr` headline along the way — it is a metric-presentation bug, not a data bug.

## Motivation

Running several agents at once is now a primary workflow, and the bar should treat it as
one. Today the agent meter is a sandboxed WASM chip (`wasm/claude/src/lib.rs`): the rich
"one bar per agent" view (`popup()`) is **hidden behind a hover**, and the sandbox
**structurally cannot** do the two things that would make the panel actually useful at scale:

1. **Correlate agents with windows.** The sandbox can't even `readlink /proc/<pid>/cwd` — it
   falls back to parsing `PWD` out of `/proc/<pid>/environ` and reconstructs `--worktree`
   cwds by hand (`wasm/claude/src/lib.rs:989` `proc_cwd`). It has no path to the sway window
   tree at all.
2. **Act on a window.** No `exec`, no sockets — it can render a row, but clicking it can't
   take you anywhere.

Meanwhile the data and the brain are already solid. The per-session cost/limit snapshots are
written by the statusline wrapper (`~/.claude/ezbar/sessions/<id>.json`, verified live with a
real `total_cost_usd`), and the parsing/projection/DPS logic lives **pure and unit-tested**
in `crates/claude-logic` (`parse_session`, `parse_stat`, `windowed_rate`, `dps`,
`has_active_descendant`, `disambiguate_labels`, `project_to_full`, `TokenCounter`). Nothing
about the meter's *logic* needs the sandbox — only the WASM *delivery* did, and that delivery
is now the thing in the way.

The native modules already prove the missing half: `workspaces.rs` and `window_title.rs`
share one sway connection via `src/sources/sway.rs` and already switch workspaces with
`sway::run_command("workspace …")` (`src/modules/workspaces.rs:145`). The same path focuses a
window. We are not inventing a capability; we are moving the meter to where the capability
already lives.

### The `$0/hr` bug (presentation, not data)

The live snapshot for the current session reads `total_cost_usd: 0.523867`,
`total_api_duration_ms: 56888` — real money, real active time. The chip still shows `$0/hr`
for three compounding reasons, all in the renderer:

- **Rounds to zero.** The chip prints `${total_dps:.0}/hr` (`wasm/claude/src/lib.rs:308`) — any
  rate under $0.50/hr renders literally `$0`.
- **Hard $1 floor.** `burning()` gates on `dps >= 1.0` (`:829`), so any agent spending less
  than a dollar per *active* hour contributes **nothing** to the headline — three live agents
  can sum to `$0/hr`.
- **Anchored to ezbar's start, not the session's.** DPS is `Δcost` since ezbar first saw the
  session (`anchors`, `:718`). Restart the bar over already-running `--continue` sessions
  (the common case) and Δ≈0 until they burn *fresh* money — so a freshly-started bar reads
  `$0/hr` by construction.

The doc-comment's intent ("a $0 reads as idle, not broken") is right, but the current output
is indistinguishable from broken. Fix: drop the floor, print `<$1/hr`/cents instead of `$0`,
and anchor from session start (the transcript holds the history) so a restart doesn't blank
the rate.

## Goals / Non-goals

**Goals**
- An always-on **vertical dock**, one row per live agent, biggest spender on top — the
  `popup()` content, promoted to a persistent surface.
- **Click a row → focus that agent's window**, switching workspace and output as needed.
- Reuse `crates/claude-logic` unchanged; delete the sandbox-only hacks in the port.
- Fix the `$/hr` headline so a spending fleet never reads `$0`.

**Non-goals**
- Removing the WASM plugin system (RFC 0006) — this promotes *one* plugin that outgrew the
  sandbox; the plugin path stays for everything else.
- An idle/"waiting for you" alarm — still deliberately out (a session waiting on you isn't
  actionable from the bar; carry it as a dim dot + idle time, as today).
- Cross-machine / non-sway support. The focus mechanism is sway/wlroots-specific by design.

## Design

### 1. Native module `agents` (replaces the `claude` wasm plugin's role)

A native `Module` (RFC 0001) registered in `src/modules/mod.rs` next to `workspaces`/`clock`
(the `match id` at `:444`). It keeps the WASM plugin's exact model — poll `/proc`, derive
idle from transcript mtimes, read per-session cost snapshots, window the rates — but as
in-process Rust:

- **Brain:** `crates/claude-logic`, called verbatim. No re-implementation, existing tests
  keep covering it.
- **I/O shell:** real `std::fs`. `proc_cwd` becomes a one-line `read_link("/proc/<pid>/cwd")`;
  the `PWD`-from-`environ` and `--worktree` reconstruction (`pwd_from_environ`,
  `worktree_from_cmdline`, `worktree_cwd`) are no longer needed for cwd (kept in the crate,
  just unused by this caller). The incremental transcript tailing (`read_new_lines`,
  `TokenCounter`) ports as-is.
- **Subscription:** its own timer like today (fast warm-up, then the calm cadence), plus the
  shared sway snapshot stream (`sources::sway`) so the window mapping refreshes on tree
  changes without its own socket.

### 2. The dock is a second bar, rotated — not a bespoke surface

`config::Bar` already carries `position` / `height` / `outputs` (`src/config.rs:348`) and the
host already builds one layer surface per matching output (RFC 0004, `BarSurface`,
`src/main.rs:682`). The dock reuses all of it:

- **`config::Position` gains `Left` and `Right`** (`src/config.rs:64`, currently `Top`/`Bottom`).
- **`bar_geometry` gains the vertical case** (`src/main.rs:906`, Top/Bottom only today):
  anchor `edge | Anchor::Top | Anchor::Bottom`, span the height, reserve **width** as the
  exclusive zone (mirror of the horizontal reserve-height path at `:929`).
- **A second bar instance** carries the `agents` module on a `Left`/`Right` surface while the
  main bar stays horizontal. Either generalise the single `bar` to a small list (`[[bar]]`),
  or add a dedicated `[dock]` that is a `Bar` with a defaulted vertical position. (Open
  question — see Risks. The geometry work is identical either way.)
- Rows stack vertically; the existing `row(...)`-per-agent layout becomes `column(rows)`.
  RFC 0021 retraction applies naturally — a narrow dock drops the tok/s and `$total` cells
  before the `$/hr` and label.

### 3. Click-to-focus

The enabler, confirmed: `swayipc::Node` exposes `pub pid: Option<i32>`
(`swayipc-types-1.4.3/src/reply.rs:427`) and `Connection::run_command` is free-form, so
criteria commands work. Mechanism:

1. The module already holds each agent's `claude` PID and a `ppid → children` map from its
   `/proc` scan. **Walk up** each agent's ancestry to the first PID that appears as a `pid`
   on a node in `get_tree()` — that node is the **terminal-emulator window** hosting the
   agent (a one-time-per-tick join: collect `{node.pid → node.id}` from the tree, then for
   each agent walk parents until one hits the map).
2. Store that `con_id` on the agent row.
3. On row click (the module's `update` gets the pointer event, exactly like the popup's
   window selector does today, `wasm/claude/src/lib.rs:240`), call
   `sway::run_command(format!("[con_id={id}] focus"))`.

Sway's `focus` on a window that lives on another workspace/output **automatically activates
that workspace and switches to that output** — so "send me to the screen it's on and focus
the window" is a single IPC command. No `wmctrl`, no EWMH, no per-compositor branching. Rows
whose window can't be resolved (agent in a tmux/ssh/detached context with no mappable window)
render normally but are non-clickable — a missing `con_id` simply yields no focus action.

### 4. Theme / motion

Reuse the `popup()` visual vocabulary (dot state, `meter_bar`, Accent `$/hr`, dim secondary
cells). The focused-agent row can take an Accent highlight that cross-fades on focus change
via the RFC 0010 `Animation<bool>` pattern `workspaces.rs` already uses — so the dock shows
*which* agent owns the focused window, and clicking visibly moves it.

## Risks / open questions

- **One bar vs many.** Does the host model a single `Bar` or a set? The dock wants to coexist
  with the horizontal bar on the same output. Cleanest is a surface list keyed by
  `(output, position)`; needs an honest look at `plan_surfaces` (`src/main.rs:779`) so the
  dock and bar don't dedup each other.
- **Exclusive-zone arithmetic on the vertical axis** must subtract dock width from the
  horizontal bars' span (or they overlap at the corner). Layer-shell handles stacking, but
  the popup-clamp math (`centered_left_margin`, `:98`) assumes horizontal — popups opened
  *from* the dock need a vertical anchor variant.
- **Losing the sandbox.** The native module gets full `/proc` + sway. That's the point, but
  it drops the capability-gated isolation the WASM version advertised. Acceptable for a
  first-party, headline module; documented here so the trade is explicit.
- **PID→window join cost.** `get_tree()` per tick is cheap (sway already serves it for
  `window_title`), and the join is O(agents · ancestry-depth). No concern, but the tree fetch
  should ride the shared sway snapshot, not a new connection.
- **Terminal multiplexers.** An agent under tmux/zellij/ssh has no direct window; it maps to
  the *outer* terminal at best (focus the terminal, not the pane). Acceptable; note it.

## Phasing

1. **Metric fix** in the existing wasm chip (floor, rounding, session-start anchor) — ships
   value immediately, independent of the rest.
2. **Native `agents` module** rendering the current chip+popup content, registered alongside
   the wasm plugin (run both, compare), reusing `claude-logic`.
3. **Vertical bar geometry** (`Position::Left/Right` + `bar_geometry` + the second-surface
   model) — the dock with no focus yet.
4. **Click-to-focus** via the `con_id` join + `sway::run_command`.
5. Retire the `claude.wasm` plugin from the default set once the native dock supersedes it.
