# Application tray

Add `"tray"` to a placement group (included before the clock in the default layout):

```toml
right = [["cpu", "memory"], ["tray"], ["clock"]]

[modules.tray]
icon_size = 22        # logical px; clamped to 12..48
spacing = 8          # clamped to 0..32
show_passive = false
```

An empty tray hides itself. One subscription-owned service is shared across tray
instances and outputs. Removing the last tray module releases both protocol
owners; no external daemon is installed or started.

## Protocols and interaction

- **StatusNotifierItem (SNI):** ezbar hosts `org.kde.StatusNotifierWatcher` if it is
  unowned, otherwise registers with the existing watcher. It never replaces an
  existing watcher. Items are keyed by unique bus owner plus object path, and
  disconnected owners are pruned. Discovery/properties are refreshed every two
  seconds (not signal-driven). A busy or unresponsive peer can delay its own
  appearance; action calls run independently of discovery.
- **D-Bus menus:** right-click opens `com.canonical.dbusmenu` when exported, with
  submenus, separators, enabled/visible state and check/radio state. Clicking an
  entry sends the protocol's `clicked` event. Otherwise right-click calls the
  application's native `ContextMenu`. Menu-only SNI items also open on left-click.
  Menus are fetched on opening; live menu mutation and lazy `AboutToShow` on each
  submenu are not yet supported.
- **Legacy XEmbed:** for Wine/Battle.net and other X11 clients. Requires Xwayland
  (`DISPLAY`) with Composite, Damage and XFixes. ezbar claims the standard tray
  selection only if vacant, redirects embedded windows into invisible,
  input-transparent containers, and draws their actual pixels in the Wayland bar.
  Left/middle/right clicks and scrolling are forwarded as X11 events. Native menus
  are positioned using the bar pointer location. No pointer warp or focus grab is
  performed. Clients requiring real rather than synthetic input may not work.
- Left-click invokes `Activate`, middle-click `SecondaryActivate`, and scroll
  invokes `Scroll` for SNI clients. Application-specific behavior remains up to
  the application (some Wine apps activate on double-click).

Wine normally withdraws its floating fallback tray once it sees ezbar's manager
announcement. If an older client does not re-register, restart that application,
not the whole Wine prefix. Another XEmbed tray owner prevents ezbar from hosting
legacy icons; do not run two legacy tray hosts simultaneously.

Output coordinates currently use Sway's logical layout. Native X11 popup placement
on mixed-scale/rotated layouts has not been validated; it may differ from Xwayland's
pixel coordinate space. Multi-output bar instances share the same tray icons.

GE-Proton 11-7's Battle.net native SNI menu exposes an X11 managed normal window:
a live X11 event trace shows it first opening at the correct tray coordinates,
then Sway centering it. This is not an ezbar D-Bus-menu popup or missing click
coordinates. No generic window-moving workaround is installed. GE's native
Wayland backend (`PROTON_ENABLE_WAYLAND=1`, requiring a complete launcher/game
restart) correctly places this menu beside the tray on the tested Sway setup.
Both launcher and WoW rendering were verified through that backend; this is not
a guarantee that every Wine application works with native Wayland.

## Resource and trust boundaries

The tray sends protocol messages; it does not execute command strings from peers.
D-Bus registration accepts only items/hosts owned by the calling connection. X11
is a shared-trust protocol, **not a security sandbox**: `_XEMBED_INFO` validation
cannot authenticate the sender of a dock message. Do not expose your X server or
session bus to untrusted users.

Queues and concurrent actions are bounded; peer calls have timeouts. There are at
most 128 icons per protocol and 256 menu nodes, with depth limited to eight.
Pixmap dimensions are capped at 512, PNG files at 1 MiB, and SVG source at 64 KiB.
SVGs are rasterized at 48×48 on a worker, with external and embedded image loading
explicitly disabled. No gzip SVGs or broad image-codec bundle is introduced.
Named icons search common hicolor/Adwaita directories and an optional application
icon-theme path; this is not a complete desktop icon-theme inheritance engine.

These limits do not turn the renderer, filesystem or X11 server into a sandbox.
X11 replies run on a dedicated thread, and file decoding has only two concurrent
workers so a blocked filesystem does not accumulate unbounded jobs. A blocked
server/filesystem may still retain a worker until it responds.

## Verification

Ordinary tests do not connect to the real desktop. Opt-in integration tests need
**private** D-Bus/X11 sessions; never set the test flags on your normal bus/display:

```sh
python3 scripts/check-lock.py --self-test
python3 scripts/check-lock.py
cargo test --locked --workspace --all-targets
EZBAR_TRAY_TEST_BUS=1 dbus-run-session -- \
  cargo test --locked --lib isolated_bus_protocols -- --ignored
# Xvfb + xauth are test-only tools, not runtime dependencies:
EZBAR_TRAY_TEST_X11=1 xvfb-run -a \
  cargo test --locked --lib isolated_xembed_protocols -- --ignored
```

The X11 test also runs against a disposable headless Sway/Xwayland server. It tests
selection non-stealing, docking, pixel capture, button forwarding, root-coordinate
translation, reparenting and shutdown. D-Bus tests cover path/service registration,
deduplication, sender validation, item properties, menu layout/events, disconnect
cleanup, coexistence with another watcher, takeover and release.

Dependency review and outstanding advisories: [tray-dependency-review.md](tray-dependency-review.md).
