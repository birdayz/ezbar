# RFC 0018: `pick()` — a host-provided native picker WASM plugins invoke

- **Status:** **Implemented** — the `pick()` host service shipped (WIT `v0.4.0`, reactor `mod v4`
  + the WALL blocking-service escape, the global pick channel, the native iced picker, the SDK
  `ctx.pick`, and the kube rewrite). LGTM'd on the implementation by both reviewers (Torvalds:
  correct/additive; r/unixporn: "front-page, rofi/fuzzel-tier"). Verified e2e: kube loads as WIT
  v0.4.0, the picker renders + filters, weather stays v0.1.0. Deferred glow-ups: real subsequence
  fuzzy match (today `contains`), scroll-follows-selection.

## What changed in v4 (round-2 review fold-ins)

The `pick()` pivot is sound — Torvalds confirmed `pick` as a host import is **additive** (no
`events`/`ui` fork, no `call_update` retype; v4 just *copies* v3's host impl + adds `pick`). Four
blockers, fixed:

1. **WALL (the real one, blocker 1).** `step()` wraps every guest call in
   `tokio::time::timeout(WALL=12s)` (`lib.rs:1223`). A fiber parked in `pick` for a human-paced
   selection trips it → "exceeded 12s — disabling." `http_get` only survives by self-bounding at
   8s<12s; `exec` because `kubectl` is sub-second. `pick` is unbounded. **Fix:** a
   `Host.in_blocking_service: bool` set across the `pick().await`; `step()` does **not** enforce
   WALL while it's set (epoch already guards runaway *guest code* — WALL is the park backstop, and
   a human pick is a legitimate unbounded park). This is a control-flow change in `step()`/`drive`,
   not additive — re-costed (§3).
2. **`Message::Pick` won't compile (blocker 2).** `oneshot::Sender` isn't `Clone`; `Message`
   derives `Clone`. The `PickRequest` (with its `reply` sender) rides the `Message` as
   `Arc<Mutex<Option<PickRequest>>>` (Clone+Debug), `take()`n in `update` (§3).
3. **Single popup owner (blocker 3).** The picker is **not** a third `Bar` field — it folds into
   the RFC 0017 one-popup owner so existing teardown (`reconcile_modules`, the panic-disable path,
   `WasmModule` drop) closes it. A guest torn down while parked: the fiber aborts, its `rx` drops,
   the bar's later `reply.send` returns `Err` (ignored), and the popup is closed by the owner (§4).
4. **Global channel (blocker 4).** `pick_tx` lives on the process-global `Reactor` (installed by
   the bar at startup via a `set_pick_sink(tx)` setter, same precedent as `set_feed_sampler`/
   `set_sway_source`), drained by a bar-level `Subscription` — not threaded per-plugin (§3).

Plus: v4's host trait **copies** v3's `exec`+`sway_snapshot` (v1 has neither) + adds `pick`, with
`DrivenPlugin::V4` in all four `call_*` arms + `linker_v4`; `popup_settings` gains a
`KeyboardInteractivity` param (the picker is `Exclusive`, `events_transparent:false`); ↑/↓/Esc
need a keyboard `event::listen_with` filtered to the picker surface (~30 lines, not "free"); focus
**restore is the compositor's job** (drop that language); `current: option<u32>` is added to the
**v0.4.0 signature now** (adding it later is a v0.5.0 bump); and kube's migration **removes the now-
dead interactive-popup machinery** (`popup_is_interactive`, `PopupMode::Click`, `click_message`) so
there aren't two "pick a context" paths.

**Styling (r/unixporn, folded into §6):** no accent *ring* (reads as an error ring) — the
`a:0.06,..fg` inset well defines the field, "active" carried by a `sep→accent` under-rule; native
`text_input` cursor **styled** (`cursor→accent`, border off, placeholder `fg_dim`, selection
`a:0.20,..accent`) since its caret can't be reshaped; **hover sets `sel`** (last input wins — one
highlighted row, kills the two-highlight problem); match contrast widened to **`a:0.45,..fg`** vs
full `fg` (first occurrence only); field text **size 15**, rows 13, field pad `[9,12]`; **8-row
cap (240px)**, body frame constant for the session + `scrollable` + scroll-follows-`sel`; loading
**debounced 150ms** (exec returns in ~1 frame → no flash); `✓`/`↵`/magnifier from the **icon
font** (not bare Unicode → tofu); single left axis; match-count `n/m` in the field's right gutter.
- **Created:** 2026-06-06
- **Target:** WIT `wit/since-v0.4.0` (one additive host fn), the reactor (`crates/ezbar-wasm`), the
  guest SDK (`crates/ezbar-plugin-wasm`), the bar's native picker (`src/main.rs`), the `kube` plugin.
- **Depends on:** RFC 0006 (WASM + the frozen-version window), RFC 0008 (reactor + parked-fiber
  async host calls — `http_get`/`exec` are the precedent), RFC 0015 (`exec` — the additive
  host-fn shape this copies), RFC 0017 (popup model).

## 1. The pivot

A WASM plugin can't take typed input, and the motivating need is a **searchable kube context
picker**. v1/v2 tried to let the *plugin draw* one — which forced new drawing primitives (a
fillable `surface` node, `paint::token-a`, `token::sep`) **and** a raw-keyboard subsystem
(`events` fork, channel widening, `Exclusive` focus + key routing). Review killed it: the DSL
can't draw a styled box at all today, and a layer-shell popup can't even hold keyboard focus the
way v2 assumed. That's the biggest feature in the project to reach a *worse* text field than iced
already ships.

**The right layer:** the host provides the picker as a **native service the guest invokes** —
the web model (a page draws its content but calls the browser's native `<select>`/file dialog).
The split:

- **WASM draws display** — `text`/`icon`/`graph`/`box` (the current DSL, untouched).
- **The host provides interactive input** — `pick()` (and later `prompt`/`confirm`) as **native
  iced** services. Keyboard, focus, IME, text editing, theming, styling — solved **once**, in the
  host, with the real text stack. The plugin never reimplements the tarpit.

This **vaporizes both blockers** (the picker is native iced — `container::Style`, `text_input`,
focus all free) and shrinks v0.4.0 to **one additive host function**, exactly the shape `exec`
took in v0.3.0. `kube` stays **WASM** (chip + logic + `exec`); only its *picker UI* becomes a host
service.

## 2. The contract

```wit
// host interface, v0.4.0 — additive, same shape/precedent as exec@0.3.0
/// Open the bar's native searchable picker over `items`; blocks the guest until the user
/// selects (returns the chosen item) or dismisses (returns none).
pick: func(prompt: string, items: list<string>) -> option<string>;
```

Guest side, the whole interaction:

```rust
// kube, on chip click:
if let Some(name) = ctx.pick("kube context", &self.contexts) {
    let _ = ctx.exec("kubectl", &["config", "use-context", &name], None);
}
```

`pick` is **async host-side** and the guest **parks** in it — runs no guest code, burns no epoch —
exactly like `http_get`/`exec` already do (RFC 0008 §3.3). It returns when the user acts.

## 3. Reactor — the `pick` import + the bar handoff

`pick` can't render anything itself (the reactor runs off the GUI thread). It hands the request to
the bar and awaits the answer over a `oneshot`:

```rust
// Host (store data) gains a cheap clone of a sender the bar owns:
pick_tx: mpsc::UnboundedSender<PickRequest>,
struct PickRequest { instance: u64, prompt: String, items: Vec<String>, reply: oneshot::Sender<Option<String>> }

async fn pick(&mut self, prompt: String, items: Vec<String>) -> Option<String> {
    let (tx, rx) = oneshot::channel();
    if self.pick_tx.send(PickRequest { instance: self.instance, prompt, items, reply: tx }).is_err() {
        return None;                       // bar gone
    }
    rx.await.unwrap_or(None)               // park here; GUI thread resolves on select/dismiss
}
```

The bar provides one `mpsc::UnboundedReceiver<PickRequest>` to the reactor at construction and
reads it via a `Subscription` (the same shape as `ipc_stream`/`config_stream`) → `Message::Pick(
PickRequest)`. The `Host` needs `instance` (already threaded for grants) and the `pick_tx`
(plumbed through `WasmModule::new`/`add_plugin`/`drive`, like `granted_exec`).

**Version window (additive — the easy kind).** `pick` is a new *host import*, so `host@0.4.0`'s
**interface gains a function but stays structurally compatible**; `ui` and `events` are byte-
identical to v0.3.0 and `with:`-remap unchanged. The reactor adds `mod v4` (`bindgen!` on
`since-v0.4.0`), `linker_v4`, `DrivenPlugin::V4`, and `plugin_version()` detects `host@0.4`. The v4
host-trait impl **re-delegates to v1 exactly as v3 does** and adds `pick` — ~90 mechanical lines,
no shared-event-path change, no `call_update` retype. v0.1–0.3 artifacts are byte-for-byte
unaffected; rebuilding the SDK moves the whole source tree to v4 (one SDK, one WIT — say it, but
it's free: the v4 host delegates everything else to v1).

**No epoch/wall hazard:** `pick` parks the fiber in `rx.await` (no guest code runs), so it can't
trip the WALL deadline or epoch limits — identical to `http_get` (RFC 0008 §3.3). A guest that
calls `pick` and never gets an answer (bar closing) gets `None` when `pick_tx`/`rx` drops.

## 4. The bar — the native picker (the kickass part)

A native iced popup the bar owns end-to-end. State held on `Bar`:

```rust
picker: Option<Picker>,
struct Picker { id: window::Id, instance: u64, prompt: String, items: Vec<String>,
                query: text_input state, sel: usize, reply: oneshot::Sender<Option<String>> }
```

- **Open** (`Message::Pick(req)`): close any open module popup (RFC 0017 one-popup), create a
  popup surface with **`KeyboardInteractivity::Exclusive`** (so it has focus on map — `OnDemand`
  does **not**, per review), anchored under the requesting instance's pill (RFC 0016
  `PillBounds` by `instance`). Focus the `text_input` (`text_input::focus(id)`).
- **Type** (`Message::PickerQuery(s)`): native `text_input` `on_input` — full editing/selection/
  IME for free. Refilter; reset `sel` to 0.
- **Navigate/commit**: `↑/↓` move `sel` (clamped to the filtered set); `Enter` commits the
  `sel`-th filtered item; click a row commits it; `Esc`/click-outside/another-popup → dismiss.
- **Resolve**: send `Some(item)` or `None` on `reply`, close the surface, clear `picker`,
  **restore prior keyboard focus** (the steal is bounded to the picker's life).

Because it's the bar, `view(id)` renders the `Picker` surface; it composes with the existing
`module_popup`/switcher one-popup discipline (RFC 0017 — fold the picker into that owner, don't add
a third). `text_input`, list, filtering, focus are all native iced — nothing new in the DSL.

## 5. SDK + kube

- **SDK** (`crates/ezbar-plugin-wasm`): `Ctx::pick(&mut self, prompt: &str, items: &[&str]) ->
  Option<String>`; `glue.rs` forwards to `p::host::pick`; WIT path bumps to `since-v0.4.0`. This is
  the standardized way to ask for input — the plugin-author skill points here (§7).
- **kube** (`wasm/kube`): drop the WASM popup picker entirely (`popup()` → `None`/removed). The
  chip wraps a `mouse_area`; on `Event::Pointer{Press}` → `ctx.pick("kube context", &contexts)` →
  on `Some` → `exec use-context`. Refresh the chip after. Rebuild (`host@0.4`), re-grant (`exec`
  hash changes — RFC 0015 dev-loop tax, one `ezbar grant kube`).

## 6. Styling (native iced — for r/unixporn)

The picker is the bar's, so full `container::Style` + theme tokens. Target a fuzzel/telescope feel:
- **Field (always focused — it's the only focusable thing):** a rounded (`radius 8`) `container`,
  inset bg `Color{a:0.06,..fg}`, a **1px accent hairline** (a resting `sep` border reads as
  *disabled* — there is no unfocused state). A **magnifier glyph** (`Icon`, cap-height ~13px) in
  `fg_dim`, ~8px gap, then the query (or `fg_dim` "Filter…" placeholder). A **2px accent caret** at
  cap-height (not full-height — that's a terminal block), static (no blink: nothing to signal, and
  no timer to drive it).
- **Match highlight (the signature detail):** kube's filter is contiguous `contains`, so split each
  row into ≤3 spans — `pre`/`post` in **`fg_dim`**, the matched run in full **`fg`**. **Not
  `accent`** (accent owns the ✓ and the selection; tinting matches rainbows the list). The
  luminance split *is* the premium choice; weight isn't available, alpha-contrast is.
- **Rows:** ~30px, padding `[6,10]`; a **fixed-width leading slot** (~16px) with `✓` in `accent` on
  the current context (never space-padding — it's ragged). The **selected row** (Enter target *and*
  arrow cursor) gets a **3px `accent` left edge + `Color{a:0.10,..accent}` wash + trailing `↵`** so
  Enter is never ambiguous; **mouse-hover** gets a weaker edge-less `Color{a:0.05,..fg}` wash — a
  different, lesser affordance so the two don't fight.
- **Overflow:** native `text_input` scrolls its own content — the field never grows the popup.
- **Loading / empty:** while contexts load, show "loading…" (`fg_dim`) — never a dead click. No
  match → "no context matches '<q>'" (`fg_dim`), centered. **Fixed body height** so the popup never
  resizes per keystroke.
- Tokens only (`fg fg_dim accent sep`); luminance-aware ink if any chip is accent-filled (RFC 0016
  §6.2). Motion: host-side fade only (RFC 0010).

## 7. Standardization (the skill)

`.claude/skills/ezbar-wasm-plugin-author` gains an **"Input & host services"** section: a plugin
*draws* display widgets but *invokes* host services for interactive input — `ctx.pick(...)` is the
first; the pattern is "park-and-get-a-result," same as `exec`/`http_get`. Plus the RFC 0017 popup
model. New interactive widgets ship as **host services**, not as DSL nodes or hand-drawn guest UI.

## 8. Testing

- **Filter logic** (case-insensitive `contains`, Enter-selects-`sel`, arrow clamp) — pure → unit
  tests, host-side, no compositor.
- **`pick` round-trip** — a reactor test: a `host@0.4` fixture calls `pick`, the bar resolves the
  `oneshot`, the guest gets the value; a dropped `reply` yields `None`; v3 fixture has no `pick`
  import (frozen-version regression; v0.1.0 weather stays green).
- **Focus/keystroke landing** — the one part a human still confirms (real `Exclusive` focus on
  sway), same untestable edge as RFC 0017.

## 9. Open questions (for review)

1. **Async UI from inside `update`.** kube calls `pick` from `update(Event::Pointer)`, parking the
   drive fiber for the whole picker session. Fine (it's `http_get`-shaped), but the chip can't
   update while parked — acceptable for a modal pick? *Rec:* yes (it's modal by nature).
2. **`pick` signature** — `list<string>` now; do we want `(label, value)` pairs or item metadata
   (an icon/`✓`-current hint) so the picker can mark the current context itself, vs. the plugin
   pre-marking via the prompt? *Rec:* add `current: option<u32>` (index) so the picker shows ✓
   without the plugin hacking the strings.
3. **One picker vs. fold into `module_popup`** — model the picker as a third popup owner or a
   variant of the RFC 0017 controller's single `open`? *Rec:* single owner (RFC 0017 already says
   so).
4. **Anchoring/size** — anchor under the pill (like a popup) or center-screen (rofi-style)? A bar
   context picker reads better anchored; a big fuzzy launcher centers. *Rec:* anchored for v1.
5. **Generality** — is `pick` the right first/only host input service, or do we name the line now
   (`prompt(text) -> option<string>`, `confirm(msg) -> bool`)? *Rec:* ship `pick`, design the line
   in the skill, add the others on demand.
