//! `workspaces` module (RFC 0001): square, state-filled chips + scroll-to-switch.
//!
//! A plain `Module` like any other — it owns its **own** sway connection (for the
//! workspace list and for switching), reads its chip style from `[modules.workspaces]`,
//! and renders from the theme tokens in `Ctx` (incl. `ctx.bg()`). No host special-casing.
//!
//! ```toml
//! [modules.workspaces]
//! style = "boxed"   # boxed | filled | outlined | underbar
//! ```

use ezbar_plugin::iced::alignment::{Horizontal, Vertical};
use ezbar_plugin::iced::futures::{Stream, StreamExt};
use ezbar_plugin::iced::mouse::ScrollDelta;
use ezbar_plugin::iced::widget::{column, container, mouse_area, row, text, Space};
use ezbar_plugin::iced::{Background, Border, Color, Element, Length, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::config::WsStyle;
use crate::sources::sway::{workspaces_stream, Workspace};

enum Msg {
    Update(Vec<Workspace>),
    Switch(String),
    Scroll(ScrollDelta),
}

pub struct Workspaces {
    instance: u64,
    style: WsStyle,
    list: Vec<Workspace>,
    scroll_accum: f32,
}

impl Workspaces {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let style = cfg
            .get("style")
            .and_then(|v| v.as_str())
            .map(parse_style)
            .unwrap_or_default();
        Workspaces {
            instance,
            style,
            list: Vec::new(),
            scroll_accum: 0.0,
        }
    }
}

fn parse_style(s: &str) -> WsStyle {
    match s {
        "filled" => WsStyle::Filled,
        "outlined" => WsStyle::Outlined,
        "underbar" => WsStyle::Underbar,
        _ => WsStyle::Boxed,
    }
}

impl Module for Workspaces {
    fn id(&self) -> &str {
        "workspaces"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, ws_sub)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Update(ws)) => self.list = ws.clone(),
            Some(Msg::Switch(name)) => switch(&format!("workspace {name}")),
            Some(Msg::Scroll(delta)) => {
                let dir = self.scroll_dir(*delta);
                if dir < 0 {
                    switch("workspace prev_on_output");
                } else if dir > 0 {
                    switch("workspace next_on_output");
                }
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let fs = ctx.theme.text_size;
        // one shared, uniform cell width so `1` and `10` read as the same square cell
        let max_chars = self
            .list
            .iter()
            .map(|w| w.name.chars().count())
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let chip_h = (ctx.theme.bar_height as f32 - 10.0).max(14.0);
        let cell_w = (max_chars * fs * 0.62 + 8.0).max(chip_h);

        let chips: Vec<Element<ModMsg>> = self
            .list
            .iter()
            .map(|w| chip(w, self.style, ctx, cell_w, chip_h))
            .collect();

        mouse_area(row(chips).spacing(4).align_y(Vertical::Center))
            .on_scroll(|d| ModMsg::new(Msg::Scroll(d)))
            .into()
    }
}

impl Workspaces {
    /// Scroll delta → switch direction (`-1` prev, `+1` next, `0` none). Mouse wheels
    /// step per notch; trackpad pixels accumulate to a threshold.
    fn scroll_dir(&mut self, delta: ScrollDelta) -> i32 {
        match delta {
            ScrollDelta::Lines { y, .. } => (y < 0.0) as i32 - (y > 0.0) as i32,
            ScrollDelta::Pixels { y, .. } => {
                self.scroll_accum += y;
                const STEP: f32 = 40.0;
                if self.scroll_accum >= STEP {
                    self.scroll_accum = 0.0;
                    -1
                } else if self.scroll_accum <= -STEP {
                    self.scroll_accum = 0.0;
                    1
                } else {
                    0
                }
            }
        }
    }
}

/// Run a sway command on a throwaway connection (fire-and-forget, never blocks update).
fn switch(cmd: &str) {
    let cmd = cmd.to_string();
    std::thread::spawn(move || {
        if let Ok(mut c) = swayipc::Connection::new() {
            let _ = c.run_command(cmd);
        }
    });
}

fn ws_sub(_id: &u64) -> impl Stream<Item = ModMsg> {
    workspaces_stream().map(|ws| ModMsg::new(Msg::Update(ws)))
}

/// One workspace as a square, state-filled chip — state drives the *fill*, not width.
fn chip<'a>(
    w: &'a Workspace,
    style: WsStyle,
    ctx: &Ctx,
    cell_w: f32,
    chip_h: f32,
) -> Element<'a, ModMsg> {
    let accent = ctx.accent();
    let fg = ctx.fg();
    let dim = ctx.fg_dim();
    let urg = ctx.urgent();
    let base = ctx.bg(); // dark text on a bright fill — now available via the ABI
    let fs = ctx.theme.text_size;
    let radius: f32 = 0.0; // square identity

    let focused = w.focused;
    let visible = w.visible && !w.focused;
    let urgent = w.urgent;
    let tint = |c: Color, a: f32| Color { a, ..c };

    if style == WsStyle::Underbar {
        let (txt, bar_color) = if urgent {
            (urg, urg)
        } else if focused {
            (fg, accent)
        } else if visible {
            (fg, Color::TRANSPARENT)
        } else {
            (dim, Color::TRANSPARENT)
        };
        let label = container(text(w.name.clone()).size(fs).color(txt))
            .width(Length::Fixed(cell_w))
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center);
        let underbar = container(Space::new().height(Length::Fixed(2.0)))
            .width(Length::Fixed(cell_w))
            .style(move |_| container::Style {
                background: Some(Background::Color(bar_color)),
                ..Default::default()
            });
        return mouse_area(column![label, underbar].height(Length::Fixed(chip_h)))
            .on_press(ModMsg::new(Msg::Switch(w.name.clone())))
            .into();
    }

    let (bg, bw, bc, txt): (Option<Color>, f32, Color, Color) = match style {
        WsStyle::Filled => {
            if urgent {
                (Some(urg), 0.0, urg, base)
            } else if focused {
                (Some(accent), 0.0, accent, base)
            } else if visible {
                (None, 0.0, fg, fg)
            } else {
                (None, 0.0, dim, dim)
            }
        }
        WsStyle::Outlined => {
            if urgent {
                (None, 1.5, urg, urg)
            } else if focused {
                (None, 1.5, accent, accent)
            } else if visible {
                (None, 1.0, tint(fg, 0.35), fg)
            } else {
                (None, 1.0, tint(fg, 0.12), dim)
            }
        }
        // boxed (default): every ws a defined cell, tiered by state, hairline border.
        WsStyle::Boxed | WsStyle::Underbar => {
            if urgent {
                (Some(urg), 0.0, urg, base)
            } else if focused {
                (Some(accent), 0.0, accent, base)
            } else if visible {
                (Some(tint(accent, 0.28)), 1.0, tint(accent, 0.55), fg)
            } else {
                (Some(tint(fg, 0.10)), 1.0, tint(fg, 0.16), tint(fg, 0.78))
            }
        }
    };

    let inner = container(text(w.name.clone()).size(fs).color(txt))
        .width(Length::Fixed(cell_w))
        .height(Length::Fixed(chip_h))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .style(move |_| container::Style {
            background: bg.map(Background::Color),
            border: Border {
                color: bc,
                width: bw,
                radius: radius.into(),
            },
            text_color: Some(txt),
            ..Default::default()
        });
    mouse_area(inner)
        .on_press(ModMsg::new(Msg::Switch(w.name.clone())))
        .into()
}
