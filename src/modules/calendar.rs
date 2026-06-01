//! `calendar` module: next meeting + countdown in the bar (blinks when imminent or
//! ongoing); hover opens a timeline agenda of the day. All colors come from the
//! host theme so the chip and popup match the rest of the bar.

use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Local};
use ezbar_plugin::iced::alignment::{Horizontal, Vertical};
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{column, container, mouse_area, row, scrollable, text, Space};
use ezbar_plugin::iced::{Background, Border, Color, Element, Length, Subscription};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::calendar::{self, CalendarData, CalendarEvent};

enum Msg {
    Data(CalendarData),
    Blink,
    Enter,
    Leave,
}

pub struct Calendar {
    instance: u64,
    data: CalendarData,
    blink_on: bool,
}

impl Calendar {
    pub fn new(instance: u64) -> Self {
        Calendar {
            instance,
            data: CalendarData {
                display_text: "\u{2026}".to_string(),
                ..Default::default()
            },
            blink_on: true,
        }
    }
}

impl Module for Calendar {
    fn id(&self) -> &str {
        "calendar"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::batch([
            ezbar_plugin::sub::keyed(self.instance, cal_stream),
            ezbar_plugin::iced::time::every(Duration::from_millis(500))
                .map(|_| ModMsg::new(Msg::Blink)),
        ])
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => self.data = d.clone(),
            Some(Msg::Blink) => self.blink_on = !self.blink_on,
            Some(Msg::Enter) => return Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
            Some(Msg::Leave) => return Response::request(HostRequest::ClosePopup),
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let c = &self.data;
        let glyph = text("\u{f133}").color(ctx.fg_dim());

        let chip: Element<ModMsg> = if !c.has_next {
            // Nothing upcoming (or not configured): a quiet glyph only — never a
            // raw file path or long status text on the bar. Detail lives in the
            // hover popup (an empty-day agenda or a one-line setup hint).
            glyph.into()
        } else {
            let state = if c.is_overdue {
                ctx.urgent()
            } else if c.is_urgent {
                ctx.warn()
            } else {
                ctx.fg()
            };
            let blinking = c.is_overdue || c.is_urgent;
            let alpha = if blinking && !self.blink_on {
                0.45
            } else {
                1.0
            };
            let accented = Color { a: alpha, ..state };
            let title_color = if blinking { accented } else { ctx.fg() };

            let label = if c.time_until_next == "ongoing" {
                "now".to_string()
            } else {
                c.time_until_next.clone()
            };
            row![
                glyph,
                text(truncate(&c.next_title, 24)).color(title_color),
                pill(label, accented),
            ]
            .spacing(6)
            .align_y(Vertical::Center)
            .into()
        };

        mouse_area(chip)
            .on_enter(ModMsg::new(Msg::Enter))
            .on_exit(ModMsg::new(Msg::Leave))
            .into()
    }

    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let pal = Pal::from_ctx(ctx);
        let now = Local::now();

        // Header: weekday + date and a meeting count on the left, a live clock right.
        let n = self.data.today_events.len();
        let header = row![
            text(now.format("%A, %B %-d").to_string())
                .size(15)
                .color(pal.fg),
            text(format!(
                "  ·  {} {}",
                n,
                if n == 1 { "event" } else { "events" }
            ))
            .size(13)
            .color(pal.dim),
            Space::new().width(Length::Fill),
            text(now.format("%H:%M").to_string())
                .size(15)
                .color(pal.accent),
        ]
        .align_y(Vertical::Center);

        let all_day: Vec<&CalendarEvent> = self
            .data
            .today_events
            .iter()
            .filter(|e| e.is_all_day)
            .collect();
        let timed: Vec<&CalendarEvent> = self
            .data
            .today_events
            .iter()
            .filter(|e| !e.is_all_day)
            .collect();

        let body: Element<ModMsg> = if all_day.is_empty() && timed.is_empty() {
            // Distinguish "configured, free day" from "not set up yet": the latter is
            // where the one-line setup hint belongs (kept off the bar itself).
            let unconfigured = self.data.display_text.starts_with("Setup");
            empty_state(pal, unconfigured)
        } else {
            let mut items: Vec<Element<ModMsg>> = Vec::new();
            for ev in &all_day {
                items.push(chip_row("All day", &ev.title, pal.accent, pal));
            }
            let mut shown = false;
            let mut marked = false;
            for ev in &timed {
                if !marked && shown && ev.start > now {
                    items.push(now_marker(now, pal));
                    marked = true;
                }
                items.push(event_row(ev, now, pal));
                shown = true;
            }
            scrollable(column(items).spacing(4))
                .height(Length::Fill)
                .into()
        };

        let content = column![header, rule(pal.sep, 1.0), body]
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill);
        Some(content.into())
    }
}

/// Theme colors copied out of `Ctx` so leaf helpers can own them (`Color: Copy`).
#[derive(Clone, Copy)]
struct Pal {
    fg: Color,
    dim: Color,
    ok: Color,
    warn: Color,
    accent: Color,
    sep: Color,
}

impl Pal {
    fn from_ctx(ctx: &Ctx) -> Self {
        Pal {
            fg: ctx.fg(),
            dim: ctx.fg_dim(),
            ok: ctx.ok(),
            warn: ctx.warn(),
            accent: ctx.accent(),
            sep: ctx.sep(),
        }
    }
}

/// One agenda row: status dot, time range, title — dimmed when past, tinted green
/// while ongoing (with a faint highlight), amber when starting within 15 minutes.
fn event_row<'a>(ev: &CalendarEvent, now: DateTime<Local>, pal: Pal) -> Element<'a, ModMsg> {
    let (title_c, dot_c, ongoing) = if now >= ev.end {
        (pal.dim, pal.dim, false)
    } else if now >= ev.start {
        (pal.ok, pal.ok, true)
    } else if ev.start - now <= ChronoDuration::minutes(15) {
        (pal.warn, pal.warn, false)
    } else {
        (pal.fg, pal.accent, false)
    };
    let time_c = if ongoing { pal.ok } else { pal.dim };
    let when = format!("{} – {}", ev.start.format("%H:%M"), ev.end.format("%H:%M"));

    // Right-aligned countdown: time until start, or time left while ongoing. Kept
    // muted (metadata, not a headline) so only the time range + title carry the
    // event's state colour.
    let trailing: Element<ModMsg> = if ev.start > now {
        text(rel(ev.start - now)).size(12).color(pal.dim).into()
    } else if ongoing {
        text(format!("ends {}", rel(ev.end - now)))
            .size(12)
            .color(pal.dim)
            .into()
    } else {
        Space::new().into()
    };

    let content = row![
        dot(dot_c, 8.0),
        text(when)
            .size(13)
            .color(time_c)
            .width(Length::Fixed(104.0)),
        text(truncate(&ev.title, 36))
            .size(13)
            .color(title_c)
            .width(Length::Fill),
        trailing,
    ]
    .spacing(10)
    .align_y(Vertical::Center);

    let (lead, bg) = if ongoing {
        (pal.ok, Some(Background::Color(Color { a: 0.10, ..pal.ok })))
    } else {
        (Color::TRANSPARENT, None)
    };
    card(lead, bg, content.into())
}

/// All-day events as a labelled chip row (reuses the agenda card geometry).
fn chip_row<'a>(time: &str, title: &str, accent: Color, pal: Pal) -> Element<'a, ModMsg> {
    let content = row![
        dot(accent, 8.0),
        text(time.to_string())
            .size(13)
            .color(pal.dim)
            .width(Length::Fixed(104.0)),
        text(truncate(title, 40)).size(13).color(pal.fg),
    ]
    .spacing(10)
    .align_y(Vertical::Center);
    card(Color::TRANSPARENT, None, content.into())
}

/// The "now" line that separates finished events from upcoming ones — the popup's
/// hero element, so it's a filled accent time-chip plus a solid accent rule (not a
/// faint hairline). Shares the card geometry so it lines up with the rows.
fn now_marker<'a>(now: DateTime<Local>, pal: Pal) -> Element<'a, ModMsg> {
    // Dark text on the accent fill reads regardless of the theme's accent hue.
    let ink = Color::from_rgb(0.10, 0.10, 0.12);
    let chip = container(
        text(format!("now  {}", now.format("%H:%M")))
            .size(11)
            .color(ink),
    )
    .padding([1, 7])
    .style(move |_| container::Style {
        background: Some(Background::Color(pal.accent)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 7.0.into(),
        },
        text_color: Some(ink),
        ..Default::default()
    });
    let content = row![
        chip,
        rule(
            Color {
                a: 0.6,
                ..pal.accent
            },
            2.0
        )
    ]
    .spacing(8)
    .align_y(Vertical::Center);
    card(Color::TRANSPARENT, None, content.into())
}

/// An agenda row drawn as a card: a 3px inset colored left edge (used to flag the
/// current event) plus uniform inner padding, so every row's dot column lines up.
fn card<'a>(
    lead: Color,
    bg: Option<Background>,
    content: Element<'a, ModMsg>,
) -> Element<'a, ModMsg> {
    let edge = container(Space::new())
        .width(Length::Fixed(3.0))
        .height(Length::Fixed(18.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(lead)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: 1.5.into(),
            },
            ..Default::default()
        });
    container(
        row![
            edge,
            container(content).padding([4, 10]).width(Length::Fill)
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: bg,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn empty_state<'a>(pal: Pal, unconfigured: bool) -> Element<'a, ModMsg> {
    let (line, hint) = if unconfigured {
        (
            "Calendar not set up",
            Some("Save your secret iCal URL to\n~/.config/ezbar/calendar_url"),
        )
    } else {
        ("No meetings today", None)
    };
    let mut inner = column![
        text("\u{f133}").size(30).color(pal.dim),
        text(line.to_string()).size(14).color(pal.fg),
    ]
    .spacing(10)
    .align_x(Horizontal::Center);
    if let Some(h) = hint {
        inner = inner.push(text(h.to_string()).size(12).color(pal.dim));
    }
    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// A small filled, rounded-rect badge — the bar-chip countdown.
fn pill<'a>(label: String, color: Color) -> Element<'a, ModMsg> {
    let bg = Color { a: 0.16, ..color };
    container(text(label).color(color))
        .padding([0, 6])
        .style(move |_| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: Color { a: 0.35, ..color },
                width: 1.0,
                radius: 6.0.into(),
            },
            text_color: Some(color),
            ..Default::default()
        })
        .into()
}

/// A filled circle of diameter `d`.
fn dot<'a>(color: Color, d: f32) -> Element<'a, ModMsg> {
    container(Space::new())
        .width(Length::Fixed(d))
        .height(Length::Fixed(d))
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: (d / 2.0).into(),
            },
            ..Default::default()
        })
        .into()
}

/// A horizontal hairline that fills the remaining width.
fn rule<'a>(color: Color, h: f32) -> Element<'a, ModMsg> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            ..Default::default()
        })
        .into()
}

/// Human "in 9m" / "in 2h" / "in 1h30m" for a positive duration.
fn rel(d: ChronoDuration) -> String {
    let mins = d.num_minutes().max(0);
    if mins < 60 {
        format!("in {mins}m")
    } else {
        let (h, m) = (mins / 60, mins % 60);
        if m > 0 {
            format!("in {h}h{m}m")
        } else {
            format!("in {h}h")
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars[..max.saturating_sub(1)].iter().collect();
    out.push('\u{2026}');
    out
}

fn cal_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let d = match calendar::get_events().await {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("calendar: {e}");
                        CalendarData {
                            display_text: "Setup: ~/.config/ezbar/calendar_url".to_string(),
                            ..Default::default()
                        }
                    }
                };
                if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        },
    )
}
