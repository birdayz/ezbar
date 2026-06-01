//! Stable API surface for ezbar modules. See `rfcs/0001-pluggable-modules.md`.
//!
//! Phase 1 (compile-in) uses these types directly. They are also the (future)
//! phase-2 dlopen contract — which is sound ONLY across bit-identical builds
//! (same rustc + same `ezbar-plugin`/`iced` source). `ModMsg`/`Any`/`TypeId`
//! are intra-build-unit only and MUST NOT be downcast by the host to interpret
//! a module's message; host control travels via the typed `HostRequest`.

use std::any::Any;
use std::sync::Arc;

pub use iced;

pub mod icons;
pub mod ui;

/// Async helpers re-exported so modules need no direct `tokio` dependency. They
/// run on the host's executor (the bar and the harness both drive iced on tokio),
/// so call them inside a [`Subscription`] recipe or a `Task` returned from `update`.
pub mod task {
    pub use tokio::task::spawn_blocking;
    pub use tokio::time::sleep;
}

/// Type-erased intra-module message. Modules define their own message enums;
/// the host never names or inspects them. `Arc` supplies the `Clone` iced needs;
/// `Debug` is a placeholder so the host `Message` can still derive `Debug`.
#[derive(Clone)]
pub struct ModMsg(pub Arc<dyn Any + Send + Sync>);

impl std::fmt::Debug for ModMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModMsg(..)")
    }
}

impl ModMsg {
    pub fn new<M: Any + Send + Sync>(msg: M) -> Self {
        ModMsg(Arc::new(msg))
    }
    /// Downcast to the module's own message type (only valid inside the module).
    pub fn get<M: Any>(&self) -> Option<&M> {
        self.0.downcast_ref::<M>()
    }
}

/// Host-owned theme tokens, `repr(C)`-stable so an `iced` bump does not churn the
/// (future) ABI. Colors are RGBA in 0..1.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ThemeTokens {
    pub fg: [f32; 4],
    pub fg_dim: [f32; 4],
    pub urgent: [f32; 4],
    pub warn: [f32; 4],
    pub ok: [f32; 4],
    pub accent: [f32; 4],
    pub sep: [f32; 4],
    /// the bar's background base — so a chip's dark-text-on-fill, or a canvas, can
    /// match the bar (a module shouldn't have to guess the surface colour).
    pub bg: [f32; 4],
    pub text_size: f32,
    pub bar_height: u16,
}

impl ThemeTokens {
    pub fn color(c: [f32; 4]) -> iced::Color {
        iced::Color::from_rgba(c[0], c[1], c[2], c[3])
    }
}

/// Per-instance render context handed to `view`/`popup`.
pub struct Ctx<'a> {
    pub instance_id: u64,
    pub theme: &'a ThemeTokens,
}

impl Ctx<'_> {
    /// Theme colors as ready-to-use `iced::Color`s — the ergonomic, discoverable
    /// way to color your widgets (they autocomplete under `ctx.`). For raw tokens
    /// use `ctx.theme` directly (it is a `&ThemeTokens`, and `ThemeTokens: Copy`).
    pub fn fg(&self) -> iced::Color {
        ThemeTokens::color(self.theme.fg)
    }
    pub fn fg_dim(&self) -> iced::Color {
        ThemeTokens::color(self.theme.fg_dim)
    }
    pub fn urgent(&self) -> iced::Color {
        ThemeTokens::color(self.theme.urgent)
    }
    pub fn warn(&self) -> iced::Color {
        ThemeTokens::color(self.theme.warn)
    }
    pub fn ok(&self) -> iced::Color {
        ThemeTokens::color(self.theme.ok)
    }
    pub fn accent(&self) -> iced::Color {
        ThemeTokens::color(self.theme.accent)
    }
    pub fn sep(&self) -> iced::Color {
        ThemeTokens::color(self.theme.sep)
    }
    /// the bar background base (for dark-text-on-fill, canvases matching the bar).
    pub fn bg(&self) -> iced::Color {
        ThemeTokens::color(self.theme.bg)
    }

    /// Resolve a `[modules.<id>.graph].line_color` spec to a fixed graph colour,
    /// or `None` for the default per-value threshold colour (green→red by load).
    ///
    /// Accepts: `None` / `""` / `"threshold"` → `None` (functional, the default);
    /// a theme token (`accent`/`ok`/`warn`/`urgent`/`fg`/`fg_dim`); or a `#rrggbb`
    /// / `#rrggbbaa` hex. An unrecognised string falls back to `None` (threshold)
    /// rather than panicking, so a typo degrades to the safe default.
    pub fn graph_paint(&self, spec: Option<&str>) -> Option<iced::Color> {
        match spec.map(str::trim) {
            None | Some("") | Some("threshold") => None,
            Some("accent") => Some(self.accent()),
            Some("ok") => Some(self.ok()),
            Some("warn") => Some(self.warn()),
            Some("urgent") => Some(self.urgent()),
            Some("fg") => Some(self.fg()),
            Some("fg_dim") | Some("dim") => Some(self.fg_dim()),
            Some(hex) => parse_hex(hex),
        }
    }
}

/// Parse `#rrggbb` / `#rrggbbaa` into an `iced::Color`; `None` on any malformed input.
fn parse_hex(s: &str) -> Option<iced::Color> {
    let s = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
    match s.len() {
        6 => Some(iced::Color::from_rgb8(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(iced::Color::from_rgba8(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            byte(6)? as f32 / 255.0,
        )),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PopupMode {
    /// Opened on hover, closed on leave; display-only, never grabs focus.
    Hover,
    /// Opened on click (toggle); interactive, sticky until closed.
    Click,
}

/// Typed host-directed requests. NEVER rides the `ModMsg`/`Any` channel.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum HostRequest {
    OpenPopup(PopupMode),
    ClosePopup,
}

/// Returned from `update`: an iced task plus typed host requests.
pub struct Response {
    pub task: iced::Task<ModMsg>,
    pub requests: Vec<HostRequest>,
}

impl Response {
    pub fn none() -> Self {
        Response {
            task: iced::Task::none(),
            requests: Vec::new(),
        }
    }
    pub fn task(task: iced::Task<ModMsg>) -> Self {
        Response {
            task,
            requests: Vec::new(),
        }
    }
    pub fn request(req: HostRequest) -> Self {
        Response {
            task: iced::Task::none(),
            requests: vec![req],
        }
    }
}

/// A bar module: "just iced". The host owns placement and the surfaces; the
/// module owns its drawing and input.
pub trait Module: Send {
    fn id(&self) -> &str;

    /// All I/O lives here (timers, sockets, procs). MUST NOT block. Recipes must
    /// be keyed by `instance_id` (use [`sub::keyed`]) so two instances of the
    /// same module type do not collide in iced's recipe-keyed runtime.
    fn subscription(&self) -> iced::Subscription<ModMsg> {
        iced::Subscription::none()
    }

    /// State transition. MUST NOT block. Returns a task + typed host requests.
    fn update(&mut self, msg: ModMsg) -> Response {
        let _ = msg;
        Response::none()
    }

    /// Whether the module has anything to show right now. `false` hides it (and its
    /// separators) entirely — e.g. a `battery` module on a desktop with no battery.
    fn visible(&self) -> bool {
        true
    }

    /// Bar content. Full iced: `canvas`, `mouse_area`, etc.
    fn view(&self, ctx: &Ctx) -> iced::Element<'_, ModMsg>;

    /// Optional detail surface; the host opens/places a popup and renders this.
    /// Leaf-only: must not emit `HostRequest`.
    fn popup(&self, ctx: &Ctx) -> Option<iced::Element<'_, ModMsg>> {
        let _ = ctx;
        None
    }

    /// Adopt a changed config live, or ask to be rebuilt (RFC 0002/0004 reconcile).
    /// Called by the host when an instance's resolved config changed but its `key`
    /// is the same. The default rebuilds the instance (dropping in-instance state);
    /// override to keep state across an edit. If the new config feeds a
    /// subscription (interval, endpoint, …), return `Applied { resubscribe: true }`
    /// so the host re-rolls this instance's streams.
    fn reconfigure(&mut self, _cfg: &toml::Value) -> Reconfigure {
        Reconfigure::Reconstruct
    }

    /// Teardown before the instance is retired.
    fn shutdown(&mut self) {}
}

/// Outcome of a live config change handed to a [`Module::reconfigure`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconfigure {
    /// Adopted in place. `resubscribe` = the new config feeds a subscription, so
    /// the host must re-roll this instance's streams (it bumps a generation that
    /// re-keys the instance's recipes); `false` keeps the running streams.
    Applied { resubscribe: bool },
    /// Rebuild the instance from scratch (the safe default).
    Reconstruct,
}

/// Subscription helpers.
pub mod sub {
    use super::ModMsg;
    use iced::futures::Stream;
    use iced::Subscription;

    /// Key a subscription by the module's `instance_id`, so two instances of the
    /// same module type produce distinct recipes. `builder` is a plain fn pointer
    /// (iced requirement); it receives the instance id as recipe data.
    pub fn keyed<S>(instance: u64, builder: fn(&u64) -> S) -> Subscription<ModMsg>
    where
        S: Stream<Item = ModMsg> + Send + 'static,
    {
        Subscription::run_with(instance, builder)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn parse_hex_rgb_and_rgba() {
        assert_eq!(
            parse_hex("#cba6f7"),
            Some(iced::Color::from_rgb8(0xcb, 0xa6, 0xf7))
        );
        // alpha byte maps onto the 0..=1 float channel
        let c = parse_hex("#cba6f780").unwrap();
        assert_eq!(
            (c.r, c.g, c.b),
            (
                0xcb as f32 / 255.0,
                0xa6 as f32 / 255.0,
                0xf7 as f32 / 255.0
            )
        );
        assert!((c.a - 0x80 as f32 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn parse_hex_rejects_malformed() {
        assert_eq!(parse_hex("cba6f7"), None); // no '#'
        assert_eq!(parse_hex("#abc"), None); // wrong length
        assert_eq!(parse_hex("#gggggg"), None); // non-hex digits
        assert_eq!(parse_hex("#"), None);
    }
}
