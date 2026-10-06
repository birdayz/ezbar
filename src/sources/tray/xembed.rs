//! Legacy XEmbed tray host for Wine and other X11 clients. Foreign icon windows
//! are saved, reparented and redirected with Composite; only their pixels enter
//! iced. We never take focus, warp the user's pointer, or claim another tray's
//! selection. X11 protocol work stays on a dedicated worker thread.
use super::{icon, publish, Action, Command, Item, ItemId, Snapshot, MAX_ITEMS};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;
use x11rb::{
    connection::Connection,
    protocol::{
        composite::{ConnectionExt as _, Redirect},
        damage::{ConnectionExt as _, ReportLevel},
        shape::ConnectionExt as _,
        xfixes::ConnectionExt as _,
        xproto::*,
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE,
};

type Error = Box<dyn std::error::Error + Send + Sync>;
const SIZE: u16 = 24;

struct Atoms {
    selection: Atom,
    opcode: Atom,
    manager: Atom,
    xembed: Atom,
    info: Atom,
    name: Atom,
    utf8: Atom,
    opacity: Atom,
}
struct Embedded {
    container: Window,
    damage: u32,
    item: Item,
    dirty: bool,
    retry: Instant,
}
struct Host {
    conn: RustConnection,
    root: Window,
    manager: Window,
    visual: Visualid,
    depth: u8,
    colormap: Colormap,
    atoms: Atoms,
    icons: BTreeMap<Window, Embedded>,
}

impl Host {
    fn open() -> Result<Self, Error> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> Result<Atom, Error> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        let atoms = Atoms {
            selection: atom(&format!("_NET_SYSTEM_TRAY_S{screen}"))?,
            opcode: atom("_NET_SYSTEM_TRAY_OPCODE")?,
            manager: atom("MANAGER")?,
            xembed: atom("_XEMBED")?,
            info: atom("_XEMBED_INFO")?,
            name: atom("_NET_WM_NAME")?,
            utf8: atom("UTF8_STRING")?,
            opacity: atom("_NET_WM_WINDOW_OPACITY")?,
        };
        if conn.get_selection_owner(atoms.selection)?.reply()?.owner != NONE {
            return Err("another XEmbed tray already owns the selection".into());
        }
        conn.composite_query_version(0, 4)?.reply()?;
        conn.damage_query_version(1, 1)?.reply()?;
        conn.xfixes_query_version(5, 0)?.reply()?;
        let manager = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            manager,
            root,
            -4096,
            -4096,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .override_redirect(1)
                .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
        )?
        .check()?;
        conn.change_property32(
            PropMode::REPLACE,
            manager,
            atom("_NET_SYSTEM_TRAY_ORIENTATION")?,
            AtomEnum::CARDINAL,
            &[0],
        )?;
        // Advertise an alpha-capable TrueColor visual when available.
        let alpha_visual = conn.setup().roots[screen]
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| &d.visuals)
            .find(|v| v.class == VisualClass::TRUE_COLOR)
            .map(|v| v.visual_id);
        let visual = alpha_visual.unwrap_or(conn.setup().roots[screen].root_visual);
        let depth = if alpha_visual.is_some() {
            32
        } else {
            conn.setup().roots[screen].root_depth
        };
        let colormap = conn.generate_id()?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual)?
            .check()?;
        conn.change_property32(
            PropMode::REPLACE,
            manager,
            atom("_NET_SYSTEM_TRAY_VISUAL")?,
            AtomEnum::VISUALID,
            &[visual],
        )?;
        // Claim and verify without replacing an owner discovered during setup.
        conn.grab_server()?.check()?;
        let claim = (|| -> Result<(), Error> {
            if conn.get_selection_owner(atoms.selection)?.reply()?.owner != NONE {
                return Err("tray selection claimed during setup".into());
            }
            conn.set_selection_owner(manager, atoms.selection, CURRENT_TIME)?
                .check()?;
            Ok(())
        })();
        conn.ungrab_server()?;
        conn.flush()?;
        claim?;
        if conn.get_selection_owner(atoms.selection)?.reply()?.owner != manager {
            return Err("could not own tray selection".into());
        }
        conn.send_event(
            false,
            root,
            EventMask::STRUCTURE_NOTIFY,
            ClientMessageEvent::new(
                32,
                root,
                atoms.manager,
                [CURRENT_TIME, atoms.selection, manager, 0, 0],
            ),
        )?;
        conn.flush()?;
        log::info!("tray: XEmbed selection claimed (window {manager:#x})");
        Ok(Self {
            conn,
            root,
            manager,
            visual,
            depth,
            colormap,
            atoms,
            icons: BTreeMap::new(),
        })
    }

    fn embed(&mut self, window: Window) -> Result<(), Error> {
        if window == NONE
            || window == self.root
            || window == self.manager
            || self.icons.contains_key(&window)
            || self.icons.len() >= MAX_ITEMS
        {
            return Ok(());
        }
        // Only dock windows that explicitly implement XEmbed, not arbitrary app
        // windows named by another X11 client in a spoofed dock message.
        let info = self
            .conn
            .get_property(false, window, self.atoms.info, self.atoms.info, 0, 2)?
            .reply()?;
        if info.format != 32 || info.value.len() != 8 {
            return Ok(());
        }
        let attr = self.conn.get_window_attributes(window)?.reply()?;
        if attr.class != WindowClass::INPUT_OUTPUT {
            return Ok(());
        }
        let container = self.conn.generate_id()?;
        self.conn
            .create_window(
                self.depth,
                container,
                self.root,
                -4096,
                -4096,
                SIZE,
                SIZE,
                0,
                WindowClass::INPUT_OUTPUT,
                self.visual,
                &CreateWindowAux::new()
                    .colormap(self.colormap)
                    .border_pixel(0)
                    .override_redirect(1)
                    .background_pixel(0)
                    .event_mask(EventMask::SUBSTRUCTURE_NOTIFY | EventMask::STRUCTURE_NOTIFY),
            )?
            .check()?;
        let damage = self.conn.generate_id()?;
        // Track resources before the remaining fallible protocol steps, so a
        // rejected/dead client cannot leak containers or retain save-set state.
        self.icons.insert(
            window,
            Embedded {
                container,
                damage,
                dirty: true,
                retry: Instant::now(),
                item: Item {
                    id: ItemId::Xembed(window),
                    title: self.title(window),
                    icon: None,
                    passive: info
                        .value32()
                        .and_then(|mut v| v.nth(1))
                        .is_none_or(|flags| flags & 1 == 0),
                    attention: false,
                    is_menu: false,
                    menu: None,
                },
            },
        );
        let result = (|| -> Result<(), Error> {
            self.conn.change_property8(
                PropMode::REPLACE,
                container,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"ezbar-tray\0ezbar-tray\0",
            )?;
            self.conn.change_property32(
                PropMode::REPLACE,
                container,
                self.atoms.opacity,
                AtomEnum::CARDINAL,
                &[0],
            )?;
            // Invisible containers never intercept native Wayland bar input.
            let region = self.conn.generate_id()?;
            self.conn.xfixes_create_region(region, &[])?;
            self.conn.xfixes_set_window_shape_region(
                container,
                x11rb::protocol::shape::SK::INPUT,
                0,
                0,
                region,
            )?;
            self.conn.xfixes_destroy_region(region)?;
            self.conn
                .change_save_set(SetMode::INSERT, window)?
                .check()?;
            self.conn.map_window(container)?;
            self.conn
                .reparent_window(window, container, 0, 0)?
                .check()?;
            // Reparent first: Xwayland's WM already redirects root children.
            self.conn
                .composite_redirect_window(window, Redirect::MANUAL)?
                .check()?;
            self.conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(0)
                    .y(0)
                    .width(u32::from(SIZE))
                    .height(u32::from(SIZE))
                    .border_width(0),
            )?;
            self.conn.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
            )?;
            self.conn.send_event(
                false,
                window,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    window,
                    self.atoms.xembed,
                    [CURRENT_TIME, 0, 0, container, 0],
                ),
            )?;
            self.conn.send_event(
                false,
                window,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(32, window, self.atoms.xembed, [CURRENT_TIME, 1, 0, 0, 0]),
            )?;
            self.conn
                .damage_create(damage, window, ReportLevel::NON_EMPTY)?
                .check()?;
            self.conn.map_window(window)?;
            self.conn.flush()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.remove(window);
            return Err(error);
        }
        log::info!("tray: embedded X11 icon {window:#x}");
        Ok(())
    }

    fn title(&self, window: Window) -> String {
        for (property, kind) in [
            (self.atoms.name, self.atoms.utf8),
            (AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()),
        ] {
            if let Ok(cookie) = self
                .conn
                .get_property(false, window, property, kind, 0, 128)
            {
                if let Ok(reply) = cookie.reply() {
                    if !reply.value.is_empty() {
                        return String::from_utf8_lossy(&reply.value)
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(128)
                            .collect();
                    }
                }
            }
        }
        "X11 tray application".into()
    }

    fn capture(&self, window: Window) -> Result<Option<icon::Icon>, Error> {
        let geometry = self.conn.get_geometry(window)?.reply()?;
        let width = geometry.width.min(SIZE);
        let height = geometry.height.min(SIZE);
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, u32::MAX)?
            .reply()?;
        let setup = self.conn.setup();
        let Some(format) = setup.pixmap_formats.iter().find(|f| f.depth == reply.depth) else {
            return Ok(None);
        };
        let Some(visual) = setup
            .roots
            .iter()
            .flat_map(|s| &s.allowed_depths)
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == reply.visual)
        else {
            return Ok(None);
        };
        let pixels = icon::ximage(
            u32::from(width),
            u32::from(height),
            format.bits_per_pixel,
            format.scanline_pad,
            reply.depth,
            setup.image_byte_order == ImageOrder::LSB_FIRST,
            [visual.red_mask, visual.green_mask, visual.blue_mask],
            &reply.data,
        );
        // Wine can briefly send a fully transparent repaint; keep the last real
        // icon until the next damage rather than blinking out of the bar.
        Ok(pixels
            .filter(|p| p.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0))
            .map(icon::Icon::Pixels))
    }

    fn remove(&mut self, window: Window) {
        if let Some(icon) = self.icons.remove(&window) {
            let _ = self.conn.damage_destroy(icon.damage);
            let _ = self
                .conn
                .composite_unredirect_window(window, Redirect::MANUAL);
            // A client may already have moved to another host. Never pull it
            // back out of its new parent on ReparentNotify.
            let ours = self
                .conn
                .query_tree(window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|tree| tree.parent == icon.container);
            if ours {
                let _ = self.conn.unmap_window(window);
                let _ = self.conn.reparent_window(window, self.root, 0, 0);
            }
            let _ = self.conn.change_save_set(SetMode::DELETE, window);
            let _ = self.conn.destroy_window(icon.container);
        }
    }

    fn events(&mut self) -> Result<bool, Error> {
        // Bound one pass so a flooding X client cannot starve actions/shutdown.
        for _ in 0..256 {
            let Some(event) = self.conn.poll_for_event()? else {
                break;
            };
            match event {
                Event::SelectionClear(e) if e.selection == self.atoms.selection => {
                    return Ok(false)
                }
                Event::ClientMessage(e)
                    if e.type_ == self.atoms.opcode
                        && e.format == 32
                        && e.window == self.manager =>
                {
                    let data = e.data.as_data32();
                    if data[1] == 0 {
                        if let Err(e) = self.embed(data[2]) {
                            log::debug!("tray dock rejected: {e}");
                        }
                    }
                }
                Event::DestroyNotify(e) => self.remove(e.window),
                Event::ReparentNotify(e) => {
                    if self
                        .icons
                        .get(&e.window)
                        .is_some_and(|i| i.container != e.parent)
                    {
                        self.remove(e.window);
                    }
                }
                Event::DamageNotify(e) => {
                    if let Some(icon) = self.icons.get_mut(&e.drawable) {
                        icon.dirty = true;
                    }
                    self.conn.damage_subtract(e.damage, NONE, NONE)?;
                }
                Event::PropertyNotify(e) => {
                    if e.atom == self.atoms.info {
                        if let Ok(reply) = self
                            .conn
                            .get_property(false, e.window, self.atoms.info, self.atoms.info, 0, 2)?
                            .reply()
                        {
                            let mapped = reply
                                .value32()
                                .and_then(|mut v| v.nth(1))
                                .is_some_and(|flags| flags & 1 != 0);
                            if let Some(icon) = self.icons.get_mut(&e.window) {
                                icon.item.passive = !mapped;
                            }
                        }
                    }
                    let title = self.title(e.window);
                    if let Some(icon) = self.icons.get_mut(&e.window) {
                        icon.item.title = title;
                        icon.dirty = true;
                    }
                }
                Event::ConfigureNotify(e) if self.icons.contains_key(&e.window) => {
                    if e.width > SIZE || e.height > SIZE || e.x != 0 || e.y != 0 {
                        self.conn.configure_window(
                            e.window,
                            &ConfigureWindowAux::new()
                                .x(0)
                                .y(0)
                                .width(u32::from(SIZE))
                                .height(u32::from(SIZE)),
                        )?;
                    }
                    if let Some(icon) = self.icons.get_mut(&e.window) {
                        icon.dirty = true;
                    }
                }
                _ => {}
            }
        }
        Ok(true)
    }

    fn action(&self, command: Command) -> Result<(), Error> {
        let ItemId::Xembed(window) = command.id else {
            return Ok(());
        };
        let Some(icon) = self.icons.get(&window) else {
            return Ok(());
        };
        let button = match command.action {
            Action::Activate => 1,
            Action::Secondary => 2,
            Action::ContextMenu => 3,
            Action::Scroll(n, false) => {
                if n > 0 {
                    4
                } else {
                    5
                }
            }
            Action::Scroll(n, true) => {
                if n > 0 {
                    6
                } else {
                    7
                }
            }
            Action::MenuClick(_) => return Ok(()),
        };
        let (x, y) = command.position;
        log::debug!(
            "tray XEmbed {:?} at ({x}, {y}), icon {window:#x}",
            command.action
        );
        // Wine's alpha-shaped tray icons can have a hole at their center.
        // Target a point inside the actual bounding shape, not that hole.
        let shape = self
            .conn
            .shape_get_rectangles(window, x11rb::protocol::shape::SK::BOUNDING)?
            .reply()?;
        let (local_x, local_y) = click_point(&shape.rectangles);
        // Put the hidden source at the real icon's location, so clients using
        // window-to-root translation position their native context menus here.
        self.conn.configure_window(
            icon.container,
            &ConfigureWindowAux::new()
                .x(x - i32::from(local_x))
                .y(y - i32::from(local_y)),
        )?;
        let press = ButtonPressEvent {
            response_type: BUTTON_PRESS_EVENT,
            detail: button,
            sequence: 0,
            time: CURRENT_TIME,
            root: self.root,
            event: window,
            child: NONE,
            root_x: x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            root_y: y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            event_x: local_x,
            event_y: local_y,
            state: KeyButMask::default(),
            same_screen: true,
        };
        self.conn
            .send_event(false, window, EventMask::BUTTON_PRESS, press)?;
        let release = ButtonPressEvent {
            response_type: BUTTON_RELEASE_EVENT,
            ..press
        };
        self.conn
            .send_event(false, window, EventMask::BUTTON_RELEASE, release)?;
        self.conn.flush()?;
        Ok(())
    }
}

/// Closest in-bounds shaped pixel to the icon center. A transparent center is
/// common for Wine icons; Windows hit-testing silently discards clicks there.
fn click_point(rectangles: &[Rectangle]) -> (i16, i16) {
    let center = i32::from(SIZE / 2);
    rectangles
        .iter()
        .take(4096)
        .filter_map(|r| {
            let left = i32::from(r.x).max(0);
            let top = i32::from(r.y).max(0);
            let right = (i32::from(r.x) + i32::from(r.width)).min(i32::from(SIZE)) - 1;
            let bottom = (i32::from(r.y) + i32::from(r.height)).min(i32::from(SIZE)) - 1;
            if left > right || top > bottom {
                return None;
            }
            Some((center.clamp(left, right), center.clamp(top, bottom)))
        })
        .min_by_key(|(x, y)| (x - center).pow(2) + (y - center).pow(2))
        .map_or((center as i16, center as i16), |(x, y)| {
            (x as i16, y as i16)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn click_avoids_transparent_center_and_invalid_rectangles() {
        assert_eq!(
            click_point(&[Rectangle {
                x: 0,
                y: 0,
                width: 24,
                height: 24
            }]),
            (12, 12)
        );
        assert_eq!(
            click_point(&[
                Rectangle {
                    x: -50,
                    y: -50,
                    width: 3,
                    height: 3
                },
                Rectangle {
                    x: 8,
                    y: 8,
                    width: 2,
                    height: 2
                },
                Rectangle {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2
                },
            ]),
            (9, 9)
        );
        assert_eq!(click_point(&[]), (12, 12));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let windows: Vec<_> = self.icons.keys().copied().collect();
        for window in windows {
            self.remove(window);
        }
        // Destroying our owner releases the selection, without clearing a new
        // owner's selection after SelectionClear.
        let _ = self.conn.destroy_window(self.manager);
        let _ = self.conn.free_colormap(self.colormap);
        let _ = self.conn.flush();
    }
}

pub(super) fn run(
    commands: Receiver<Command>,
    updates: watch::Sender<Snapshot>,
    stop: Arc<AtomicBool>,
) {
    let mut host: Option<Host> = None;
    let mut next_open = Instant::now();
    let mut last_error = String::new();
    while !stop.load(Ordering::Acquire) {
        if host.is_none() && Instant::now() >= next_open {
            match Host::open() {
                Ok(h) => {
                    host = Some(h);
                    last_error.clear();
                }
                Err(e) => {
                    let error = e.to_string();
                    if error != last_error {
                        log::warn!("tray XEmbed unavailable: {error}");
                        last_error = error;
                    }
                }
            }
            next_open = Instant::now() + Duration::from_secs(3);
        }
        if let Some(h) = host.as_mut() {
            match h.events() {
                Ok(true) => {}
                _ => {
                    host = None;
                    publish(&updates, Vec::new(), true);
                    continue;
                }
            }
            for _ in 0..64 {
                let Ok(command) = commands.try_recv() else {
                    break;
                };
                if let Err(e) = h.action(command) {
                    log::debug!("tray XEmbed action: {e}");
                }
            }
            let dirty: Vec<_> = h
                .icons
                .iter()
                .filter(|(_, i)| (i.dirty || i.item.icon.is_none()) && Instant::now() >= i.retry)
                .map(|(w, _)| *w)
                .collect();
            for window in dirty {
                let pixels = h.capture(window).ok().flatten();
                if let Some(icon) = h.icons.get_mut(&window) {
                    if pixels.is_some() {
                        icon.item.icon = pixels;
                    }
                    icon.dirty = false;
                    icon.retry = Instant::now() + Duration::from_millis(100);
                }
            }
            publish(
                &updates,
                h.icons.values().map(|i| i.item.clone()).collect(),
                true,
            );
            let _ = h.conn.flush();
        } else {
            while commands.try_recv().is_ok() {}
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // Host's Drop reparents saved clients and releases the selection.
}

#[cfg(test)]
#[path = "xembed_tests.rs"]
mod integration_tests;
