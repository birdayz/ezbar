//! A Wayland idle inhibitor (`zwp_idle_inhibit_manager_v1`, idle-inhibit-unstable-v1).
//!
//! Sway and swayidle do **not** look at systemd-logind inhibitor locks: they only honour this
//! protocol (a logind `idle` lock shows up in `systemd-inhibit` but doesn't stop swayidle from
//! blanking the outputs). An inhibitor object is tied to a `wl_surface`, and a bare surface
//! without a role is not enough — Sway ignores it. The inhibitor therefore sits on a real,
//! mapped layer-shell surface, the same kind a bar uses: a transparent 1×1 pixel on the
//! `background` layer with no exclusive zone, no input region and no keyboard focus, so it is
//! invisible and never gets in the way. It is separate from the bar's own surfaces, so it does
//! not depend on their lifetime (hotplug, reconfigure); if the compositor closes it (its output
//! went away) the worker builds a new one.
//!
//! Everything lives on its own Wayland connection, so the compositor drops the inhibitor if
//! ezbar dies, just like the logind fd. A worker thread keeps reading that connection until the
//! handle is dropped.

use std::os::fd::{AsFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

/// The surface that carries the inhibitor.
struct Live {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    inhibitor: Option<ZwpIdleInhibitorV1>,
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    manager: Option<ZwpIdleInhibitManagerV1>,
    /// The 1×1 transparent buffer attached to the surface.
    buffer: Option<wl_buffer::WlBuffer>,
    live: Option<Live>,
    /// The compositor closed the layer surface; the worker rebuilds it.
    closed: bool,
}

impl State {
    /// Create the layer surface (an empty commit; the buffer and the inhibitor follow once the
    /// compositor sends `configure`).
    fn build(&mut self, qh: &QueueHandle<Self>) -> Result<(), String> {
        let compositor = self.compositor.as_ref().ok_or("wl_compositor missing")?;
        let layer_shell = self
            .layer_shell
            .as_ref()
            .ok_or("zwlr_layer_shell_v1 missing")?;

        let surface = compositor.create_surface(qh, ());
        // An empty input region: the pixel must never swallow a click.
        let region = compositor.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();

        let layer = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Background,
            "ezbar-idle-inhibit".to_string(),
            qh,
            (),
        );
        layer.set_size(1, 1);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();

        self.live = Some(Live {
            surface,
            layer,
            inhibitor: None,
        });
        Ok(())
    }

    fn teardown(&mut self) {
        if let Some(live) = self.live.take() {
            if let Some(inhibitor) = live.inhibitor {
                inhibitor.destroy();
            }
            live.layer.destroy();
            live.surface.destroy();
        }
    }
}

/// An active idle inhibitor. Dropping it releases the inhibition.
pub struct WaylandInhibitor {
    stop: Arc<AtomicBool>,
    connection: Connection,
    qh: QueueHandle<State>,
}

impl WaylandInhibitor {
    /// Connect to the compositor and create an inhibitor. Blocks for a few Wayland roundtrips,
    /// so call it from a blocking context. Fails when the compositor lacks
    /// `zwp_idle_inhibit_manager_v1` or layer-shell.
    pub fn new() -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
        let mut queue = connection.new_event_queue::<State>();
        let qh = queue.handle();
        let _registry = connection.display().get_registry(&qh, ());

        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("registry: {e}"))?;
        if state.manager.is_none() {
            return Err("zwp_idle_inhibit_manager_v1 not advertised".into());
        }
        state.buffer = Some(blank_buffer(&state, &qh)?);
        state.build(&qh)?;

        // The roundtrips deliver `configure`, which maps the surface and creates the inhibitor;
        // they also surface protocol errors.
        for _ in 0..2 {
            queue
                .roundtrip(&mut state)
                .map_err(|e| format!("create inhibitor: {e}"))?;
        }
        if !state.live.as_ref().is_some_and(|l| l.inhibitor.is_some()) {
            return Err("compositor did not configure the inhibitor surface".into());
        }

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_conn = connection.clone();
        let worker_qh = qh.clone();
        std::thread::Builder::new()
            .name("wayland-idle-inhibit".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    if let Err(e) = queue.blocking_dispatch(&mut state) {
                        log::warn!("inhibit: Wayland dispatch: {e}");
                        break;
                    }
                    if state.closed && !worker_stop.load(Ordering::Acquire) {
                        // The output the surface was on went away. Rebuild, but not in a tight
                        // loop should the compositor keep closing it (e.g. no outputs at all).
                        log::debug!("inhibit: layer surface closed, rebuilding");
                        state.closed = false;
                        state.teardown();
                        std::thread::sleep(Duration::from_secs(1));
                        if let Err(e) = state.build(&worker_qh) {
                            log::warn!("inhibit: cannot rebuild the inhibitor surface: {e}");
                            break;
                        }
                    }
                }
                state.teardown();
                if let Some(manager) = state.manager.take() {
                    manager.destroy();
                }
                let _ = worker_conn.flush();
            })
            .map_err(|e| format!("start worker: {e}"))?;

        Ok(Self {
            stop,
            connection,
            qh,
        })
    }
}

impl Drop for WaylandInhibitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the worker's blocking_dispatch so it notices `stop` and tears everything down.
        self.connection.display().sync(&self.qh, ());
        let _ = self.connection.flush();
    }
}

/// A 1×1 fully transparent ARGB8888 buffer (a zero-filled anonymous file, no mapping needed).
fn blank_buffer(state: &State, qh: &QueueHandle<State>) -> Result<wl_buffer::WlBuffer, String> {
    let shm = state.shm.as_ref().ok_or("wl_shm not advertised")?;
    // SAFETY: memfd_create has no memory-safety preconditions; a successful result is a fresh fd
    // we own and hand to `File`.
    let file = unsafe {
        let fd = libc::memfd_create(c"ezbar-idle-inhibit".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 {
            return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
        }
        std::fs::File::from_raw_fd(fd)
    };
    file.set_len(4).map_err(|e| format!("memfd size: {e}"))?;
    let pool = shm.create_pool(file.as_fd(), 4, qh, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    Ok(buffer)
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" if state.compositor.is_none() => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" if state.shm.is_none() => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "zwlr_layer_shell_v1" if state.layer_shell.is_none() => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zwp_idle_inhibit_manager_v1" if state.manager.is_none() => {
                    state.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                let (Some(live), Some(buffer)) = (state.live.as_mut(), state.buffer.as_ref())
                else {
                    return;
                };
                if &live.layer != layer {
                    return; // a stale surface from before a rebuild
                }
                live.surface.attach(Some(buffer), 0, 0);
                live.surface.commit();
                // Mapped now; only then put the inhibitor on it.
                if live.inhibitor.is_none() {
                    if let Some(manager) = state.manager.as_ref() {
                        live.inhibitor = Some(manager.create_inhibitor(&live.surface, qh, ()));
                    }
                }
            }
            zwlr_layer_surface_v1::Event::Closed
                if state.live.as_ref().is_some_and(|l| &l.layer == layer) =>
            {
                state.closed = true;
            }
            _ => {}
        }
    }
}

/// These objects send nothing we act on; they only need a `Dispatch` impl.
macro_rules! ignore_events {
    ($($proto:ty),* $(,)?) => {$(
        impl Dispatch<$proto, ()> for State {
            fn event(
                _: &mut Self,
                _: &$proto,
                _: <$proto as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore_events!(
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    wl_region::WlRegion,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    ZwlrLayerShellV1,
    ZwpIdleInhibitManagerV1,
    ZwpIdleInhibitorV1,
);
