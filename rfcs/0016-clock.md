# RFC 0016: the perfect clock — event-driven, drift-free, with a calendar

- **Status:** Draft (v2 — review folded in: a systems reviewer (Torvalds) + an r/unixporn
  reviewer. Both ACK the *shape* (boundary-aligned event sleep + watchdog; built-in; no
  animation); v1's boundary arithmetic was wrong and the visual spec was thin — both fixed
  below.)
- **Created:** 2026-06-05
- **Target:** ezbar (the built-in `clock` module, `src/modules/clock.rs`).
- **Depends on:** RFC 0001 (modules, popups, per-output instances, theme tokens), RFC 0010
  (host-side popup motion), RFC 0011 (event-driven cadence — its philosophy, ported to the
  built-in subscription path), and the sticky-popup *menu* dismiss model (click-outside /
  re-click, landed this cycle).

## What changed in v2 (review fold-ins)

The systems review found **three correctness bugs and a parser hole** in v1's scheduling, all
from hand-rolling per-unit epoch arithmetic; the aesthetics review found the chip would read
as a *bolted-on terminal widget* and the calendar would break on light themes. Fixes:

1. **v1's epoch-remainder boundary math was wrong** — the minute branch overshot by exactly
   1 s (a `%H:%M` clock would tick a second *late*, every minute), and the hour/day branches
   were wrong for fractional-offset zones (+5:30, +5:45, +8:45, +9:30, +12:45) and could
   **panic** at a DST-transition midnight (`LocalResult::None`/`Ambiguous` unwrapped).
   **Fix:** one generic boundary function over **local civil time** (§3.1) — no `rem_euclid`,
   correct for fractional offsets and DST, explicit `LocalResult` handling. The
   "epoch-remainder is DST-proof" claim is narrowed to where it's true (it isn't used at all
   now).
2. **`granularity()` misclassified composite specifiers** (`%c %r %X %+` all contain seconds)
   and false-matched `%%S`. **Fix:** a real specifier-stream scan (§9), `%%` skipped, complete
   finest-unit table, unknown specifier → finest unit (a wasted 1 Hz wake is cheap; a frozen
   seconds field is a bug).
3. **`seconds` bool deleted** (§8) — it was a second source of truth for a fact `format`
   already encodes. One knob for "show seconds": `format`.
4. **Suspend story told honestly** (§3.2): `CLOCK_MONOTONIC` *freezes* across suspend (it
   doesn't "undercount"); `CLOCK_BOOTTIME` would count it but `tokio::time::sleep` is
   MONOTONIC and won't expose it — *that* is why the `MAX_IDLE` watchdog exists. logind
   dependency dropped; Open-Q4 closed (watchdog ships).
5. **Typography fully specified** (§6): tabular **figures** via the `tnum` OpenType feature on
   the bar's own UI font — **not** generic `monospace` (which resolves to a terminal face and
   breaks the type voice); dim colon; small-caps meridiem; dimmed seconds; load-bearing
   leading zeros.
6. **Calendar visual spec added** (§6.1): today = an `accent`-filled disc with **cut-out ink =
   the popup background token** (v1 would've copied `calendar.rs`'s hardcoded dark ink and
   gone to mud on light themes); a **three-level alpha-on-`fg`** recessive hierarchy (one
   `fg_dim` can't do weekend + adjacent-month + week-number); a **fixed 6-row grid** (never
   resize the popup on month nav); hard-aligned columns.
7. **Hover stops advertising controls it can't honor** (§4): hover = a glance card with **no
   chevrons** (display-only per RFC 0001); the navigable grid is click-only. Resolves
   Open-Q2/Q3.
8. **§2 de-rationalized:** dropped the "highest-frequency widget" line (a WASM clock would
   `set_timeout(60_000)` — 1×/min, not high-frequency) and the invented "wall-clock cap"; the
   call rests on *universal + zero-capability + native popup fidelity*.

## 1. Problem

The shipped clock is a one-line `text` label on a **blind 1-second timer**:

```rust
loop {
    let s = chrono::Local::now().format(&fmt).to_string();
    if changed { out.send(Tick(s)); }
    tokio::time::sleep(Duration::from_secs(1)).await;   // ← the problem
}
```

Three faults:

1. **It drifts.** Each cycle is `render + 1s`, so the wake walks off the second boundary by
   the render time, every tick, forever. It updates *late* and will eventually skip or double
   a second. A clock that can't keep time is an embarrassing bug.
2. **It's a poll, not an event source.** For `%H:%M` it wakes **60×** to change the string
   **once**. RFC 0011 killed exactly this for WASM plugins; the built-in clock never got the
   memo.
3. **It's just a number.** No calendar, no date detail, no world clock — the most-glanced-at
   widget on the bar does the least.

Plus a latent hole: after **suspend/resume** or an **NTP step**, the chip shows a stale time
until the next ordinary wake (§3.2).

## 2. Decision: built-in, not WASM

The clock stays **built-in**. The rule:

| Go **WASM** when… | Go **built-in** when… |
|---|---|
| optional / niche / user-authored | every user wants it, identically |
| touches network/exec/fs you'd sandbox | needs no dangerous capability |
| you distribute & iterate it out-of-tree | wants rich native interaction |

The clock fails every WASM column: it's **universal** (optionality buys nothing), needs **zero
capabilities** (wall-time is intrinsic and harmless — the sandbox would guard nothing), and a
month-grid popup wants **native iced fidelity** on the youngest, most interaction-heavy
surface we have. A *world-clock* variant is a fine WASM **showcase** (custom timezones via
`chrono-tz`) — but that's the optional one. Ship the great built-in for everyone.

## 3. Event-driven scheduling — the heart of it

**Can a clock be event-driven instead of a timer? Yes — and it should be.** A clock's events
are *derivable from its own data*. It renders `display(T) = strftime(T, fmt)`, and that string
changes **only** at the boundaries of the *finest field present in `fmt`*. So:

> Parse `fmt` → find the smallest unit it shows (sub-second / second / minute / hour / day).
> Sleep until the **next boundary of that unit**. Wake, render, recompute, repeat.

One wake per *visible* change, aligned to the boundary — a sequence of *"the display changes
now"* events the clock computes for itself. RFC 0011's "idle = zero wasted wakes," on the
built-in path.

### 3.1 One generic boundary function (local civil time)

**Do not** hand-roll per-unit epoch arithmetic (v1's three bugs all came from that). Compute
the next boundary by truncating *local civil time* to the unit and adding one unit — correct
for whole-minute, half-hour, and three-quarter-hour offsets, and for DST midnights:

```rust
enum Unit { Sub(u32 /*ns step*/), Sec, Min, Hour, Day }

/// Instant of the next start-of-`unit`, in local time, as a Duration from `now`.
fn until_next_boundary(unit: Unit, now: DateTime<Local>) -> Duration {
    use chrono::{Timelike, Duration as Cd};
    let next_local: DateTime<Local> = match unit {
        Unit::Sec  => (now + Cd::seconds(1)).with_nanosecond(0).unwrap(),
        Unit::Min  => (now + Cd::minutes(1)).with_second(0).unwrap().with_nanosecond(0).unwrap(),
        Unit::Hour => (now + Cd::hours(1)).with_minute(0).unwrap()
                          .with_second(0).unwrap().with_nanosecond(0).unwrap(),
        Unit::Day  => {
            // next local midnight; resolve the DST-transition cases explicitly (no unwrap()).
            let date = now.date_naive() + Cd::days(1);
            let nd   = date.and_hms_opt(0,0,0).unwrap();          // naive midnight always valid
            match Local.from_local_datetime(&nd) {
                LocalResult::Single(t)        => t,
                LocalResult::Ambiguous(a, _)  => a,                // fall-back: earliest
                LocalResult::None             => Local             // spring-forward: midnight
                    .from_local_datetime(&(nd + Cd::hours(1)))     // skipped → first valid instant
                    .earliest().unwrap_or(now + Cd::days(1)),
            }
        }
        Unit::Sub(step_ns) => return Duration::from_nanos(
            step_ns as u64 - (now.nanosecond() % step_ns) as u64),
    };
    // land *just past* the boundary so timer slop can't wake us a hair early into the old unit.
    (next_local - now).to_std().unwrap_or_default() + Duration::from_millis(1)
}
```

The target is recomputed from the *actual* clock each wake, so a late wake never compounds —
the next boundary is still exact (drift-free by construction). No `rem_euclid`, no +1 s carry,
no panic at a transition midnight.

### 3.2 Suspend / NTP-step safety — a watchdog, honestly justified

`tokio::time::sleep` runs on **`CLOCK_MONOTONIC`, which *freezes* during suspend** (it does not
count suspended time — that's `CLOCK_BOOTTIME`, which tokio doesn't expose). So a long sleep
across a lid-close wakes *late by the suspend duration*; same story for an NTP step. Rather
than take a logind D-Bus dependency, cap the sleep:

```rust
let wait = until_next_boundary(unit, now).min(MAX_IDLE);   // MAX_IDLE = 30 s
```

Each wake recomputes from real wall time and **re-renders only on change** (dedup kept), so:

- seconds clock: boundary < 1 s — cap never engages, exact.
- minute clock: boundary ≤ 60 s, capped 30 s — ≤ 1 "nothing changed" wake/min.
- date-only clock: would sleep to midnight; cap makes it re-check ≤ every 30 s, so a resume /
  NTP step is reflected within `MAX_IDLE` with **no D-Bus listener**.

Post-resume staleness is bounded to `MAX_IDLE` for *any* format, costing ≤ 2 redundant
wakes/min that produce no re-render (each is a `now()` + format + strcmp — negligible). This
is the whole price of suspend correctness. **`MAX_IDLE = 30 s`** (settled; not a knob).

## 4. The calendar — hover glances, click navigates

Reusing RFC 0001's **hover = display-only / click = interactive** split and the menu-dismiss
model (click-outside / re-click closes; no hover-leave timer):

- **Hover →** `PopupMode::Hover`, display-only, closes on leave. A **glance card, no
  controls**: long date (`Friday, June 5 2026`), ISO week number + day-of-year (small,
  `fg_dim`), and a **static current-week strip** — one row, this week's 7 days, today disc'd.
  **No `‹ ›` chevrons** — a hover surface can't honor them (RFC 0001), and advertising a
  control you can't click is a dead affordance.
- **Click →** `PopupMode::Click`, sticky/interactive. The full surface (§6.1): the navigable
  month grid with **`‹  JUNE 2026  ›`**, ISO week-number column, a **"Today"** reset, and the
  **world-clock** list. Dismiss = click-outside / re-click / open another popup. hover→click
  reads as a satisfying *expand*, not a repeat.

`calendar = "hover" | "click" | "off"` picks the trigger (default `"hover"`). `calendar.rs` is
unrelated — it's a *meetings/agenda* widget; no overlap.

## 5. World clock

```toml
zones = ["UTC", { tz = "America/New_York", label = "NYC" }, "Asia/Tokyo"]
```

Rendered in the click popup via **`chrono-tz`** (pure-Rust tz database — no
`/usr/share/zoneinfo` dependency, so it survives a future WASM port). **Three hard columns**,
not a ragged space-join: fixed-width **left-aligned label**, **right-aligned tabular `HH:MM`**,
**right-aligned offset** (`+5:30`). Rules:

- **Labels strip IANA ids** by default (`America/New_York` → "New York"); `{tz, label}`
  overrides. Raw zone ids in a popup are amateur hour.
- **±1 day** marker (when it's already tomorrow / still yesterday there, **relative to the
  bar's local date**) as a muted superscript: `09:00 ⁺¹` in `fg_dim`; nothing for same-day.
- **Sort by current UTC offset** (day reads left→right). The key is **recomputed per render**
  (offsets move with each zone's own DST); tiebreak by label.
- **Night cities dimmed:** rows where it's currently night render in `fg_dim`, daytime in full
  `fg` — "who's awake" reads at a glance, zero iconography. (Optional `☀`/`☾` in `fg_dim`;
  never color-temperature tinting.)
- **Home row pinned:** the user's local zone gets a 3 px `accent` lead edge so "home" is
  findable.

## 6. Typography & anti-jitter (the chip)

- **Tabular figures, not monospace.** A proportional font makes `11:11` narrower than `00:00`,
  so the chip **jiggles** every minute and shoves its neighbours. Fix with the **`tnum`
  OpenType feature on the bar's own UI font** — tabular *figures*, keeping the type voice.
  Generic `font = "monospace"` is **rejected**: it resolves to a terminal face (DejaVu/Liberation
  Mono) that reads as a bolted-on widget. *Implementation note:* confirm cosmic-text/iced
  exposes OpenType feature selection; if not, fall back to a **specific** congruent mono
  (JetBrains Mono / Iosevka), never the generic alias — and scope it to the **chip**, never
  the popup's prose.
- **Dim the colon.** Render `:` in **`fg_dim`** against full-strength `fg` digits — the eye
  locks onto the numbers. The single biggest free "premium" tell. **Not** blinking (§ no
  animation).
- **Meridiem is metadata.** If a 12 h `format` is used, the `am/pm` suffix renders **~0.7em,
  `fg_dim`, small-caps**, trailing — never a co-equal full-weight `%p`.
- **Seconds are texture.** When `format` shows seconds, render the `:%S` field **~0.8em,
  `fg_dim`** so the minute stays the headline.
- **Leading zeros are load-bearing.** Keep `%H` (zero-padded) on the chip — fixed width is the
  whole point of this section; a variable-width hour reintroduces the jitter tabular figures
  just removed.
- **Re-render only on string change** (kept) and **all colors from theme tokens** (§6.1).
- **No digit animation** by default. An opt-in flip/tick is explicitly out — it fights §3's
  "wake only on change" and the no-drift contract.

### 6.1 The calendar grid (the click popup)

- **Today = an `accent`-filled disc** (~22–24 px) with the day number in **cut-out ink**,
  tabular, centered. **Not** an underline (reads as a link), **not** a bordered box (reads as
  spreadsheet cell-selection). **Cut-out ink = the popup *background* token**, or a
  luminance-pick between `fg` and a dark token — **never a hardcoded dark RGB** (that's the
  `calendar.rs` `now_marker` landmine: invisible mud on a light theme's pastel accent).
- **Three-level recessive hierarchy via alpha on `fg`** (one `fg_dim` can't do triple duty):
  in-month weekday = full `fg`; in-month **weekend = `Color{ a: 0.55, ..fg }`** (dim the
  *numbers*, never a colored background column, never `urgent`); **adjacent-month =
  `Color{ a: 0.28, ..fg }`** (clearly ghosted). Week-number column = `fg_dim` **plus a `sep`
  hairline divider** — separated by *structure*, not just tint.
- **Fixed 6-row grid.** Months span 4–6 weeks; **always reserve 6 rows**, padded with the
  ghosted adjacent-month days — so the popup **never changes height** on `‹ ›` nav (a height
  jump per click is nauseating). Cells **square** (~28–32 px), numbers centered on **both**
  axes (tabular numerals center single/double digits identically).
- **Weekday header** `M T W T F S S` (respecting `first_day`): `fg_dim`, ~11 px,
  **letter-spaced**, each initial **centered over its column at the cell width**.
- **Month header** centered over the grid: month in `fg` medium, **letter-spaced/small-caps**,
  year trailing in `fg_dim` ("JUNE 2026"). Chevrons `fg_dim` at rest → `fg`/`accent` on hover,
  ~28 px hit targets — the only hover affordance the click popup needs.
- **Motion** is host-side (RFC 0010): ~120–150 ms ease-out fade + a few px slide-down on open.
  Month nav = hard-swap or a ~60 ms crossfade of *just the day cells* — no carousel slide, no
  per-cell stagger.

### 6.2 Four re-theme guarantees

So a re-themer only ever touches the 7 tokens (`fg fg_dim accent ok warn urgent sep`):

1. **No hardcoded ink** — cut-out text = a background token or luminance-picked.
2. **All recessive levels = alpha on a token** (`Color{a,..fg}`), never literal colors.
3. **Every divider / column rule = `sep`.**
4. **The today-disc contrast is luminance-aware** (works on light *and* dark accents).

`urgent` gets a **concrete job** or it isn't mentioned: flag a configured holiday / today when
it's a configured "deadline" day — otherwise cut it (no cargo-cult "reserved" token).

## 7. Edge cases

- **DST.** §3.1 computes boundaries in local civil time with explicit `LocalResult` handling —
  correct across the skipped spring-forward hour *and* the repeated fall-back hour, including a
  transition that lands *at* midnight (the case that would panic a naive `.unwrap()`).
- **Fractional-offset zones** (+5:30, +5:45, +8:45, +9:30, +12:45): handled by §3.1 (local
  truncation), not epoch remainder.
- **Leap second.** Ignored — the kernel smears/repeats; not a bar's job. (Reviewers concur.)
- **Locale.** `first_day = "monday" | "sunday"` (default `monday`); 12/24 h is the user's
  `format`. We don't read system locale — explicit config beats guessing. **ISO 8601 week
  numbers are Monday-defined**; with `first_day = "sunday"` the grid stays Sunday-first but the
  week-number column still shows ISO (Monday-based) weeks, labeled as such — or set
  `week_numbers = false`. (We do not implement US/MMWR week numbering.)
- **Multi-output.** Per-output instance already (RFC 0001) → N independent boundary timers; at
  ≤ 2 wakes/min each, not worth sharing.

## 8. Config surface

```toml
[modules.clock]
format       = "%H:%M"                 # bar chip (chrono strftime) — also the source of truth
                                       #   for granularity AND whether seconds show
popup_format = "%A, %B %-d %Y"         # calendar glance-card / header line
calendar     = "hover"                 # "hover" | "click" | "off"
week_numbers = true                    # ISO-8601 (Monday-based) week column
first_day    = "monday"                # "monday" | "sunday"
zones        = []                      # world clock; ["UTC", {tz="Asia/Tokyo", label="HQ"}]
```

(`seconds` removed — use `format`. `font` removed as a knob — tabular figures are on by
default per §6; the chip never uses generic monospace.) Every key has a sane default;
`[modules.clock]` with no body = today's chip, **drift-fixed**, with a hover glance card.

## 9. Implementation

`src/modules/clock.rs`. The stream tail becomes event-driven:

```rust
loop {
    let now = chrono::Local::now();
    let s = now.format(&fmt).to_string();
    if last.as_deref() != Some(&s) { last = Some(s.clone()); out.send(Tick(s)).await?; }
    let wait = until_next_boundary(unit, now).min(MAX_IDLE);   // §3.1 / §3.2
    tokio::time::sleep(wait).await;
}
```

`unit` is computed once from `fmt` by a **real specifier scan** (not `contains`):

```rust
fn granularity(fmt: &str) -> Unit {
    // walk the format; skip "%%"; map each "%x" to its finest unit; take the min seen.
    // finest-unit table MUST include composites: %f/%.f→Sub, %S %T %s %r %c %X %+→Sec,
    // %M %R→Min, %H %I %k %l→Hour, else Day. Unknown "%x" → Sec (a wasted 1 Hz wake is
    // cheap; a seconds field that updates 1/min is a bug). Bare literal text → no constraint.
    ...
}
```

**Popup is real work — spec the state machine, don't hand-wave the line count.** `Clock` gains
`shown_month: NaiveDate` (the first of the displayed month; defaults to the current month) and
its `update` grows beyond `Tick`:

- `Msg::PrevMonth` / `Msg::NextMonth` → shift `shown_month` by ∓1 month (saturating at chrono's
  range).
- `Msg::Today` → reset `shown_month` to the current month.
- hover/click triggers via `hover_messages()` / `click_message()` per `calendar`.

`popup()` builds: the month grid (§6.1 — fixed 6 rows, week column, today disc, alpha
hierarchy), the header with chevrons, and the world-clock list (§5). Honest size: the month
grid alone is ~100 iced lines; **budget ~400–500 lines total**, not v1's "~200." No host, SDK,
WIT, or capability change — it's a built-in.

## 10. Non-goals

- WASM port of the core clock (a world-clock variant *may* be a separate WASM example).
- Alarms / timers / stopwatch (a different widget).
- Animated / flip-clock digits (opt-in gimmick rejected, §6).
- Reading system locale (explicit config instead).
- Keyboard-driven month nav as a hard requirement (layer-shell focus is fiddly; deferred).
- US/MMWR week numbering (ISO-only, §7).

## 11. Resolved questions

1. `MAX_IDLE` → **30 s** (settled; idle wake is `now()`+format+strcmp).
2. Hover content → **glance card + static current-week strip, no navigable grid / chevrons**
   (a hover surface can't honor controls; the expand to a full grid is the click popup's job).
3. Default trigger → **`calendar = "hover"`** (glanceable out of the box; click is the power
   path).
4. Resume handling → **watchdog only** (`MAX_IDLE` cap); no logind/D-Bus dependency in v1.
