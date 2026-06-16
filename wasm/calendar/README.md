# calendar (ezbar WASM plugin)

The next meeting + a countdown in the chip; **hover** opens the day's agenda. A meeting that
carries a **Zoom** link is **click-to-join**: clicking the row hands its rebuilt web-client URL to
`xdg-open`, so the browser lands on the in-meeting page directly — skipping Zoom's "launch the
app" wall. (Heuristic: any `…zoom.us/{j,wc,s,launch/jc}/{id}…?pwd=…` link is turned into
`https://{host}/wc/{id}/join?pwd={token}`.)

This is the sandboxed replacement for the old built-in `calendar` module.

## Build & install

```sh
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/calendar.wasm ~/.config/ezbar/plugins/
```

## Configure

The secret iCal URL is read from `~/.config/ezbar/calendar_url` (one line), exactly as before —
but a sandboxed plugin must be **granted** access to it. Paste this into
`~/.config/ezbar/config.toml` (or run `ezbar grant calendar --dangerous`, which writes it for you):

```toml
[modules.calendar]
network = ["calendar.google.com"]               # the iCal feed host
fs = [{ path = "~/.config/ezbar", mode = "r" }] # read calendar_url; mounts at /ezbar (DANGEROUS tier)
exec = ["xdg-open"]                             # open the meeting in the browser (DANGEROUS tier)
max_memory = "64M"                              # REQUIRED — see "Large feeds" below
```

`fs` and `exec` are the dangerous tier (RFC 0015), so the one-command ack writes them only when
you opt in: `ezbar grant calendar --dangerous`. Plain `ezbar grant calendar` writes just the safe
`network` grant and tells you what it withheld. `max_memory` is a resource knob, not a capability,
so add that one line by hand.

### Large feeds

A secret Google iCal URL serves your **entire calendar history** — easily tens of MB and
thousands of events — but the WASM sandbox caps a plugin at **2 MiB**, which can't even hold the
response. Two things address this:

- The plugin **slices the feed to a couple of days around today** the instant it fetches (one
  byte pass — `calendar_logic::slim_ical`), then only keeps/parses that KB-sized window, so
  steady-state memory and per-tick CPU stay tiny.
- It still has to **receive** the full body first, so raise its cap with `max_memory` (e.g.
  `"64M"`). Without it the plugin can't load the feed and the chip stays on its loading glyph.

The display timezone comes from the host (`ctx.local_timezone()`, RFC 0019), so meetings render
in your local wall-clock time without any extra config. `$GOOGLE_CALENDAR_ICAL_URL` is **not**
read — the sandbox has no environment access; use the file.

## Preview

```sh
cargo run -p ezbar-wasm --example preview -- \
    wasm/calendar/target/wasm32-wasip2/release/calendar.wasm \
    --net calendar.google.com \
    --fs ~/.config/ezbar:/ezbar:r \
    --exec xdg-open
```

Without `--fs` the chip shows the unconfigured glyph and the popup the setup hint — a useful
smoke test of the load path on its own.
