//! The workspaces widget: square, state-filled chips + scroll-to-switch.
//!
//! This is **host chrome**, not an RFC-0001 data module — it's themed by
//! `[theme.workspaces]` and the global tokens, so it lives host-side as a small
//! view component rather than fighting the module ABI. The host owns the live
//! workspace list (from sway IPC) and maps [`WsAction`]s back to its own messages.

use iced::alignment::{Horizontal, Vertical};
use iced::mouse::ScrollDelta;
use iced::widget::{column, container, mouse_area, row, text, Space};
use iced::{Background, Border, Color, Element, Length};

use crate::config::{Theme, WsStyle};
use crate::sources::sway::Workspace;

/// What a workspace interaction asks the host to do.
#[derive(Debug, Clone)]
pub enum WsAction {
    /// jump to this workspace (a chip was clicked)
    Switch(String),
    /// the user scrolled over the zone (host decides prev/next)
    Scroll(ScrollDelta),
}

/// Workspace list + the trackpad scroll accumulator, owned host-side.
#[derive(Default)]
pub struct WorkspacesView {
    list: Vec<Workspace>,
    scroll_accum: f32,
}

impl WorkspacesView {
    pub fn set(&mut self, list: Vec<Workspace>) {
        self.list = list;
    }

    /// Turn a scroll delta into a switch direction (`-1` prev, `+1` next, `0` none).
    /// Mouse wheels emit discrete `Lines` (one step per notch); trackpads emit small
    /// `Pixels`, accumulated to a threshold so you don't fly through workspaces.
    pub fn scroll_dir(&mut self, delta: ScrollDelta) -> i32 {
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

    /// The chip row, scrollable to switch. `blink_on` drives the urgent fade.
    pub fn view(&self, theme: &Theme, bar_height: u32, blink_on: bool) -> Element<'_, WsAction> {
        let fs = theme.font_size;
        // one shared, uniform cell width so `1` and `10` read as the same square cell
        let max_chars = self
            .list
            .iter()
            .map(|w| w.name.chars().count())
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let chip_h = (bar_height as f32 - 10.0).max(14.0);
        let cell_w = (max_chars * fs * 0.62 + 8.0).max(chip_h);

        let chips: Vec<Element<WsAction>> = self
            .list
            .iter()
            .map(|w| chip(w, theme, cell_w, chip_h, blink_on))
            .collect();

        mouse_area(row(chips).spacing(4).align_y(Vertical::Center))
            .on_scroll(WsAction::Scroll)
            .into()
    }
}

/// One workspace as a square, state-filled chip (our square/dark identity — not
/// ashell's rounded morphing pill). State drives the *fill*, not the width.
fn chip<'a>(
    w: &'a Workspace,
    theme: &Theme,
    cell_w: f32,
    chip_h: f32,
    blink_on: bool,
) -> Element<'a, WsAction> {
    let accent = theme.primary.iced();
    let fg = theme.text.iced();
    let dim = theme.dim.iced();
    let urg = theme.urgent.iced();
    let base = theme.background.base().iced(); // dark text on a bright fill
    let radius: f32 = 0.0; // square identity
    let fs = theme.font_size;

    let focused = w.focused;
    let visible = w.visible && !w.focused;
    let urgent = w.urgent;
    let blink = urgent && !blink_on;
    let fade = |c: Color| if blink { Color { a: 0.4, ..c } } else { c };
    let tint = |c: Color, a: f32| Color { a, ..c };

    // accent underbar: numbers + a 2px bar under the active ws.
    if theme.workspaces.style == WsStyle::Underbar {
        let (txt, bar_color) = if urgent {
            (fade(urg), fade(urg))
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
            .style(move |_: &iced::Theme| container::Style {
                background: Some(Background::Color(bar_color)),
                ..Default::default()
            });
        return mouse_area(column![label, underbar].height(Length::Fixed(chip_h)))
            .on_press(WsAction::Switch(w.name.clone()))
            .into();
    }

    // (background, border_width, border_color, text_color) per state.
    let (bg, bw, bc, txt): (Option<Color>, f32, Color, Color) = match theme.workspaces.style {
        // filled focus only: just the active ws is a solid square.
        WsStyle::Filled => {
            if urgent {
                (Some(fade(urg)), 0.0, urg, base)
            } else if focused {
                (Some(accent), 0.0, accent, base)
            } else if visible {
                (None, 0.0, fg, fg)
            } else {
                (None, 0.0, dim, dim)
            }
        }
        // outlined focus: square accent border, transparent fill.
        WsStyle::Outlined => {
            if urgent {
                (None, 1.5, fade(urg), fade(urg))
            } else if focused {
                (None, 1.5, accent, accent)
            } else if visible {
                (None, 1.0, tint(fg, 0.35), fg)
            } else {
                (None, 1.0, tint(fg, 0.12), dim)
            }
        }
        // boxed (default): every ws a defined cell, tiered by state. Each idle cell
        // carries a hairline border so it separates from the panel even at low fill.
        WsStyle::Boxed | WsStyle::Underbar => {
            if urgent {
                (Some(fade(urg)), 0.0, urg, base)
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
        .style(move |_: &iced::Theme| container::Style {
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
        .on_press(WsAction::Switch(w.name.clone()))
        .into()
}
