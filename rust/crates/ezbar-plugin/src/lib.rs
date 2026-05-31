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
        Response { task: iced::Task::none(), requests: Vec::new() }
    }
    pub fn task(task: iced::Task<ModMsg>) -> Self {
        Response { task, requests: Vec::new() }
    }
    pub fn request(req: HostRequest) -> Self {
        Response { task: iced::Task::none(), requests: vec![req] }
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

    /// Bar content. Full iced: `canvas`, `mouse_area`, etc.
    fn view(&self, ctx: &Ctx) -> iced::Element<'_, ModMsg>;

    /// Optional detail surface; the host opens/places a popup and renders this.
    /// Leaf-only: must not emit `HostRequest`.
    fn popup(&self, ctx: &Ctx) -> Option<iced::Element<'_, ModMsg>> {
        let _ = ctx;
        None
    }

    /// Teardown before the instance is retired.
    fn shutdown(&mut self) {}
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
