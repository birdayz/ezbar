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
//!
//! **Motion (RFC 0010):** when the focused workspace changes, the accent highlight
//! *cross-fades* from the old pill to the new instead of hard-cutting. Each pill owns an
//! `Animation<bool>` ("am I focused") keyed by name; `view` reads a single `now`, projects
//! each into a scalar `t ∈ [0,1]` and hand-lerps the focused colors (`Animation<bool>`
//! interpolates `f32`, not `Color`). Frames are requested *only while a fade runs*, so an
//! idle bar costs zero extra redraws.

use std::collections::HashMap;

use ezbar_plugin::iced::alignment::{Horizontal, Vertical};
use ezbar_plugin::iced::animation::Easing;
use ezbar_plugin::iced::futures::{Stream, StreamExt};
use ezbar_plugin::iced::mouse::ScrollDelta;
use ezbar_plugin::iced::time::{Duration, Instant};
use ezbar_plugin::iced::widget::{column, container, mouse_area, row, text, Space};
use ezbar_plugin::iced::{Animation, Background, Border, Color, Element, Length, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::config::WsStyle;
use crate::sources::sway::{self, Workspace};

/// ~150 ms snappy settle — **not** lilt's `EaseInOut` 100 ms default (its slow start reads
/// as lag). The incoming pill leads the outgoing one, so it reads as a moving highlight.
const FADE: Duration = Duration::from_millis(180);

enum Msg {
    Update(Vec<Workspace>),
    Switch(String),
    Scroll(ScrollDelta),
    /// A redraw tick while a fade runs (only when `animate` is on) — a state no-op that
    /// just re-`view`s so the interpolation advances. Dropped once every pill settles.
    Tick,
}

pub struct Workspaces {
    instance: u64,
    style: WsStyle,
    list: Vec<Workspace>,
    scroll_accum: f32,
    /// Opt-in cross-fade (RFC 0010), `[modules.workspaces].animate` (default **false**).
    /// Off by default because the fade's redraw driver must not disturb layershell's pointer
    /// seat — this gate keeps the default bar's hover safe while the timer-driven fade is
    /// verified live (the old `window::frames()` driver broke hover; this uses `time::every`).
    animate: bool,
    /// Per-workspace focus animation, keyed by name (bounded by the ~10 workspace count,
    /// evicted on every update). Each pill animates independently so the cross-fade reads
    /// as a *moving* highlight, not a synchronized blink. Unused while `animate` is off.
    anim: HashMap<String, Animation<bool>>,
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
            animate: cfg
                .get("animate")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            anim: HashMap::new(),
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
        let ws = ezbar_plugin::sub::keyed(self.instance, ws_sub);
        // Drive redraws while a fade runs — but with `time::every`, NOT `window::frames()`:
        // the frame-callback path corrupts layershell's pointer seat (`mouse hasn't entered`)
        // and kills hover. A plain timer ticks `view` without touching that path. Gated on
        // `animate` so the default bar has zero extra redraws and a known-safe hover.
        if self.animate && self.anim.values().any(|a| a.is_animating(Instant::now())) {
            Subscription::batch([
                ws,
                ezbar_plugin::iced::time::every(Duration::from_millis(16))
                    .map(|_| ModMsg::new(Msg::Tick)),
            ])
        } else {
            ws
        }
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Update(ws)) => {
                let now = Instant::now();
                for w in ws {
                    self.anim
                        .entry(w.name.clone())
                        .or_insert_with(|| {
                            Animation::new(w.focused).easing(Easing::EaseOutCubic).duration(FADE)
                        })
                        .go_mut(w.focused, now); // lilt no-ops if the target is unchanged
                }
                // Evict gone workspaces so the map can't grow — no lifecycle leak.
                self.anim.retain(|name, _| ws.iter().any(|w| &w.name == name));
                self.list = ws.clone();
            }
            Some(Msg::Switch(name)) => sway::run_command(format!("workspace {name}")),
            Some(Msg::Scroll(delta)) => {
                let dir = self.scroll_dir(*delta);
                if dir < 0 {
                    sway::run_command("workspace prev_on_output");
                } else if dir > 0 {
                    sway::run_command("workspace next_on_output");
                }
            }
            Some(Msg::Tick) | None => {}
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

        // `t` is the eased focus-ness when `animate` is on (one `now` for the whole frame, no
        // intra-frame skew), else discrete {0,1} (the default — chip renders the resting or
        // fully-focused paint, no interpolation).
        let now = Instant::now();
        let chips: Vec<Element<ModMsg>> = self
            .list
            .iter()
            .map(|w| {
                let t = if self.animate {
                    self.anim
                        .get(&w.name)
                        .map(|a| a.interpolate(0.0_f32, 1.0_f32, now))
                        .unwrap_or(if w.focused { 1.0 } else { 0.0 })
                } else if w.focused {
                    1.0
                } else {
                    0.0
                };
                chip(w, self.style, ctx, cell_w, chip_h, t)
            })
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

fn ws_sub(_id: &u64) -> impl Stream<Item = ModMsg> {
    sway::workspaces().map(|ws| ModMsg::new(Msg::Update(ws)))
}

/// `(background, border_width, border_color, text)` — the full paint of one chip in one
/// logical state. `background == TRANSPARENT` means "no fill". The border *width* is fixed
/// per pill (the resting state's), so a focus fade only recolors and never reflows.
type Paint = (Color, f32, Color, Color);

/// Linear rgba lerp — `Animation<bool>` only interpolates `f32`, so the view drives a
/// scalar `t` and lerps each color channel by hand (RFC 0010 §2).
fn lerp(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// One workspace as a square, state-filled chip — state drives the *fill*, not width.
/// `t ∈ [0,1]` is the eased focus-ness: `view` lerps between this pill's resting paint and
/// its focused paint so the accent highlight cross-fades on a workspace switch (RFC 0010).
fn chip<'a>(
    w: &'a Workspace,
    style: WsStyle,
    ctx: &Ctx,
    cell_w: f32,
    chip_h: f32,
    t: f32,
) -> Element<'a, ModMsg> {
    let accent = ctx.accent();
    let fg = ctx.fg();
    let dim = ctx.fg_dim();
    let urg = ctx.urgent();
    let base = ctx.bg(); // dark text on a bright fill — now available via the ABI
    let fs = ctx.theme.text_size;
    let radius: f32 = 0.0; // square identity

    // Resting (non-focused) state of THIS pill, by its current flags. The focused flag is
    // animated, so `visible` here is "visible on another output" (multi-monitor).
    let visible = w.visible && !w.focused;
    let urgent = w.urgent;
    let tint = |c: Color, a: f32| Color { a, ..c };

    if style == WsStyle::Underbar {
        // Underbar: a 2px bar (fixed height — no reflow) under centered text. Lerp the text
        // and bar colors between resting and focused by `t`.
        // Rest bar is accent-at-alpha-0, not `TRANSPARENT` ({0,0,0,0}): lerping from pure
        // transparent-black would drag the rgb toward black mid-fade (a muddy, darkened
        // lilac). Holding the accent rgb and animating only alpha reads as the bar cleanly
        // materializing. Alpha-0 still renders nothing, so the resting look is unchanged.
        let (rest_txt, rest_bar) = if urgent {
            (urg, urg)
        } else if visible {
            (fg, tint(accent, 0.0))
        } else {
            (dim, tint(accent, 0.0))
        };
        let (foc_txt, foc_bar) = if urgent { (urg, urg) } else { (fg, accent) };
        let txt = lerp(rest_txt, foc_txt, t);
        let bar_color = lerp(rest_bar, foc_bar, t);

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

    // The resting and focused paints for this style. `urgent` wins at both ends, so an
    // urgent pill keeps its discrete red and doesn't fade (RFC 0010 §3.3).
    let (rest, foc): (Paint, Paint) = match style {
        WsStyle::Filled => {
            let urgent_p = (urg, 0.0, urg, base);
            let focused_p = if urgent { urgent_p } else { (accent, 0.0, accent, base) };
            // accent-at-alpha-0 (not `TRANSPARENT`) so the fill fades in on-hue, not via
            // darkened-toward-black; `bg.a > 0.001` still suppresses it at rest.
            let rest = if urgent {
                urgent_p
            } else if visible {
                (tint(accent, 0.0), 0.0, fg, fg)
            } else {
                (tint(accent, 0.0), 0.0, dim, dim)
            };
            (rest, focused_p)
        }
        WsStyle::Outlined => {
            let urgent_p = (Color::TRANSPARENT, 1.0, urg, urg);
            let focused_p = if urgent {
                urgent_p
            } else {
                (Color::TRANSPARENT, 1.0, accent, accent)
            };
            let rest = if urgent {
                urgent_p
            } else if visible {
                (Color::TRANSPARENT, 1.0, tint(fg, 0.35), fg)
            } else {
                (Color::TRANSPARENT, 1.0, tint(fg, 0.12), dim)
            };
            (rest, focused_p)
        }
        // boxed (default): every ws a defined cell, tiered by state, hairline border.
        WsStyle::Boxed | WsStyle::Underbar => {
            let urgent_p = (urg, 1.0, urg, base);
            let focused_p = if urgent { urgent_p } else { (accent, 1.0, accent, base) };
            let rest = if urgent {
                urgent_p
            } else if visible {
                (tint(accent, 0.28), 1.0, tint(accent, 0.55), fg)
            } else {
                (tint(fg, 0.10), 1.0, tint(fg, 0.16), tint(fg, 0.78))
            };
            (rest, focused_p)
        }
    };

    // Fixed border width (the resting state's) so the fade only recolors — animating width
    // would reflow/jitter every frame. Focused fill == focused border color, so the 1px
    // border is an invisible seam at the focused end.
    let bw = rest.1;
    let bg = lerp(rest.0, foc.0, t);
    let bc = lerp(rest.2, foc.2, t);
    let txt = lerp(rest.3, foc.3, t);
    let background = (bg.a > 0.001).then_some(Background::Color(bg));

    let inner = container(text(w.name.clone()).size(fs).color(txt))
        .width(Length::Fixed(cell_w))
        .height(Length::Fixed(chip_h))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .style(move |_| container::Style {
            background,
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
