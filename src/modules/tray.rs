//! Tray icons: StatusNotifier + legacy XEmbed, backed by one shared service.
//! `[modules.tray] icon_size = 22, spacing = 8, show_passive = false`.
use crate::sources::tray::{Action, Icon, Item, ItemId, MenuNode, Pixels, Service, Snapshot};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};
use iced::{
    alignment::Vertical,
    futures::{SinkExt, Stream},
    mouse::ScrollDelta,
    widget::{button, canvas, column, container, mouse_area, row, rule, scrollable, text, Space},
    Color, Element, Length, Point, Rectangle, Renderer, Size, Subscription, Task, Theme,
};
use std::sync::Arc;

enum Msg {
    Ready(Arc<Service>),
    State(Snapshot),
    Action(ItemId, Action),
    Menu(u64, ItemId, Vec<MenuNode>),
    Submenu(i32),
    Back,
    Choose(i32),
}

pub struct Tray {
    instance: u64,
    service: Option<Arc<Service>>,
    snapshot: Snapshot,
    size: u32,
    spacing: u32,
    show_passive: bool,
    pointer: (i32, i32),
    menu_item: Option<Item>,
    menu_request: u64,
    menu_pending: bool,
    menu: Vec<MenuNode>,
    submenu: Vec<i32>,
}

impl Tray {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        Self {
            instance,
            service: None,
            snapshot: Snapshot::default(),
            size: cfg
                .get("icon_size")
                .and_then(toml::Value::as_integer)
                .unwrap_or(22)
                .clamp(12, 48) as u32,
            spacing: cfg
                .get("spacing")
                .and_then(toml::Value::as_integer)
                .unwrap_or(8)
                .clamp(0, 32) as u32,
            show_passive: cfg
                .get("show_passive")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false),
            pointer: (0, 0),
            menu_item: None,
            menu_request: 0,
            menu_pending: false,
            menu: Vec::new(),
            submenu: Vec::new(),
        }
    }
    fn items(&self) -> impl Iterator<Item = &Item> {
        self.snapshot
            .sni
            .iter()
            .chain(&self.snapshot.xembed)
            .filter(|i| self.show_passive || !i.passive)
    }
    fn menu_nodes(&self) -> &[MenuNode] {
        let mut nodes = self.menu.as_slice();
        for id in &self.submenu {
            if let Some(node) = nodes.iter().find(|n| n.id == *id) {
                nodes = &node.children;
            }
        }
        nodes
    }
}

impl Module for Tray {
    fn id(&self) -> &str {
        "tray"
    }
    fn visible(&self) -> bool {
        self.items().next().is_some()
    }
    fn pointer_position(&mut self, x: i32, y: i32) {
        self.pointer = (x, y);
    }
    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, tray_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Ready(service)) => self.service = Some(service.clone()),
            Some(Msg::State(state)) => {
                self.snapshot = state.clone();
                if self
                    .menu_item
                    .as_ref()
                    .is_some_and(|item| !self.items().any(|i| i.id == item.id))
                {
                    self.menu_item = None;
                    self.menu.clear();
                    return Response::request(HostRequest::ClosePopup);
                }
            }
            Some(Msg::Action(id, action)) => {
                let Some(service) = self.service.clone() else {
                    return Response::none();
                };
                let Some(item) = self.items().find(|i| &i.id == id).cloned() else {
                    return Response::none();
                };
                let menu = matches!(action, Action::ContextMenu)
                    || (matches!(action, Action::Activate) && item.is_menu);
                if menu && item.menu.is_some() {
                    if self.menu_pending {
                        return Response::none();
                    }
                    self.menu_request = self.menu_request.wrapping_add(1);
                    self.menu_pending = true;
                    let request = self.menu_request;
                    let id = item.id.clone();
                    return Response::task(Task::perform(
                        async move { service.menu(&item).await },
                        move |nodes| ModMsg::new(Msg::Menu(request, id.clone(), nodes)),
                    ));
                }
                self.menu_pending = false;
                self.menu_request = self.menu_request.wrapping_add(1);
                service.action(
                    &item,
                    if menu { Action::ContextMenu } else { *action },
                    self.pointer,
                );
            }
            Some(Msg::Menu(request, id, nodes)) => {
                if *request != self.menu_request || !self.menu_pending {
                    return Response::none();
                }
                self.menu_pending = false;
                let Some(item) = self.items().find(|i| &i.id == id).cloned() else {
                    return Response::none();
                };
                if nodes.is_empty() {
                    if let Some(service) = &self.service {
                        service.action(&item, Action::ContextMenu, self.pointer);
                    }
                } else {
                    self.menu_item = Some(item);
                    self.menu = nodes.clone();
                    self.submenu.clear();
                    return Response::request(HostRequest::OpenPopup(PopupMode::Click));
                }
            }
            Some(Msg::Submenu(id)) => {
                if self
                    .menu_nodes()
                    .iter()
                    .any(|n| n.id == *id && n.enabled && !n.children.is_empty())
                {
                    self.submenu.push(*id);
                }
            }
            Some(Msg::Back) => {
                self.submenu.pop();
            }
            Some(Msg::Choose(id))
                if self
                    .menu_nodes()
                    .iter()
                    .any(|n| n.id == *id && n.enabled && !n.separator) =>
            {
                if let (Some(service), Some(item)) = (&self.service, &self.menu_item) {
                    service.action(item, Action::MenuClick(*id), self.pointer);
                }
                return Response::request(HostRequest::ClosePopup);
            }
            Some(Msg::Choose(_)) => {}
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let urgent = ctx.urgent();
        let icons = self
            .items()
            .map(|item| {
                let image: Element<'_, ModMsg> = match &item.icon {
                    Some(Icon::Pixels(pixels)) => canvas::Canvas::new(Raster(pixels))
                        .width(self.size)
                        .height(self.size)
                        .into(),
                    None => container(
                        text(item.title.chars().next().unwrap_or('?').to_string())
                            .size(self.size)
                            .color(ctx.fg()),
                    )
                    .width(self.size)
                    .height(self.size)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into(),
                };
                let image: Element<'_, ModMsg> = if item.attention {
                    container(image)
                        .padding(1)
                        .style(move |_| container::Style {
                            border: iced::Border {
                                color: urgent,
                                width: 1.0,
                                radius: 3.0.into(),
                            },
                            ..Default::default()
                        })
                        .into()
                } else {
                    image
                };
                let id = item.id.clone();
                let target = mouse_area(image)
                    .on_press(ModMsg::new(Msg::Action(id.clone(), Action::Activate)))
                    .on_middle_press(ModMsg::new(Msg::Action(id.clone(), Action::Secondary)))
                    .on_right_press(ModMsg::new(Msg::Action(id.clone(), Action::ContextMenu)))
                    .on_scroll(move |delta| {
                        let (x, y, factor) = match delta {
                            ScrollDelta::Lines { x, y } => (x, y, 120.0),
                            ScrollDelta::Pixels { x, y } => (x, y, 1.0),
                        };
                        let horizontal = x.abs() > y.abs();
                        let delta = ((if horizontal { x } else { y }) * factor)
                            .clamp(-1200.0, 1200.0) as i32;
                        ModMsg::new(Msg::Action(id.clone(), Action::Scroll(delta, horizontal)))
                    });
                // A widget tooltip is clipped to the thin layer-shell surface;
                // do not paint overlapping labels across neighboring tray pills.
                target.into()
            })
            .collect::<Vec<Element<'_, ModMsg>>>();
        if icons.is_empty() {
            Space::new().into()
        } else {
            row(icons)
                .spacing(self.spacing)
                .align_y(Vertical::Center)
                .into()
        }
    }

    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        self.menu_item.as_ref()?;
        let mut rows: Vec<Element<'_, ModMsg>> = Vec::new();
        if !self.submenu.is_empty() {
            rows.push(
                button(text("‹ Back"))
                    .on_press(ModMsg::new(Msg::Back))
                    .into(),
            );
        }
        for node in self.menu_nodes() {
            if node.separator {
                rows.push(rule::horizontal(1).into());
                continue;
            }
            let marker = match node.checked {
                Some(true) => "✓ ",
                Some(false) => "○ ",
                None => "",
            };
            let suffix = if node.children.is_empty() {
                ""
            } else {
                "  ›"
            };
            let mut entry = button(text(format!("{marker}{}{suffix}", node.label)).size(13))
                .width(Length::Fill);
            if node.enabled {
                let msg = if node.children.is_empty() {
                    Msg::Choose(node.id)
                } else {
                    Msg::Submenu(node.id)
                };
                entry = entry.on_press(ModMsg::new(msg));
            }
            rows.push(entry.into());
        }
        Some(
            scrollable(column(rows).spacing(3))
                .height(Length::Fill)
                .into(),
        )
    }
    fn popup_size(&self) -> Option<(u32, u32)> {
        Some((
            300,
            (self.menu_nodes().len() as u32 * 32 + 48).clamp(80, 560),
        ))
    }
}

fn tray_stream(_instance: &u64) -> impl Stream<Item = ModMsg> {
    iced::stream::channel(
        2,
        |mut out: iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            let service = Service::acquire();
            let mut updates = service.subscribe();
            if out.send(ModMsg::new(Msg::Ready(service))).await.is_err() {
                return;
            }
            loop {
                let state = updates.borrow_and_update().clone();
                if out.send(ModMsg::new(Msg::State(state))).await.is_err() {
                    break;
                }
                if updates.changed().await.is_err() {
                    break;
                }
            }
        },
    )
}

/// Raw RGBA pixels through iced's existing canvas backend. Avoid enabling iced's
/// broad image-codec feature/dependency graph for tiny tray pixmaps. At most
/// 48×48 cells are drawn, regardless of the source icon's dimensions.
struct Raster<'a>(&'a Pixels);
impl canvas::Program<ModMsg> for Raster<'_> {
    type State = ();
    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let width = bounds.width.ceil().clamp(1.0, 48.0) as u32;
        let height = bounds.height.ceil().clamp(1.0, 48.0) as u32;
        let cell = Size::new(bounds.width / width as f32, bounds.height / height as f32);
        for y in 0..height {
            for x in 0..width {
                let index = ((y * self.0.height / height * self.0.width + x * self.0.width / width)
                    * 4) as usize;
                if let Some(p) = self.0.rgba.get(index..index + 4) {
                    if p[3] != 0 {
                        frame.fill_rectangle(
                            Point::new(x as f32 * cell.width, y as f32 * cell.height),
                            cell,
                            Color::from_rgba8(p[0], p[1], p[2], p[3] as f32 / 255.0),
                        );
                    }
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_is_bounded_and_empty_tray_hides() {
        let tray = Tray::new(
            1,
            &"icon_size = 100000\nspacing = -4"
                .parse::<toml::Value>()
                .unwrap(),
        );
        assert_eq!(tray.size, 48);
        assert_eq!(tray.spacing, 0);
        assert!(!tray.visible());
    }
}
