//! `calendar` module: next meeting + countdown, blinks when imminent/ongoing;
//! click opens today's meeting list.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{column, mouse_area, row, scrollable, text};
use ezbar_plugin::iced::{Color, Element, Subscription};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::calendar::{self, CalendarData};

enum Msg {
    Data(CalendarData),
    Blink,
    Toggle,
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
                display_text: " \u{2026}".to_string(),
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
            Some(Msg::Toggle) => {
                return Response::request(HostRequest::OpenPopup(PopupMode::Click))
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        let c = &self.data;
        let base = if c.is_overdue {
            Color::from_rgb(1.0, 0.27, 0.27)
        } else if c.is_urgent {
            Color::from_rgb(1.0, 0.67, 0.0)
        } else {
            Color::WHITE
        };
        let blinking = c.is_overdue || c.is_urgent;
        let color = if blinking && !self.blink_on {
            Color { a: 0.4, ..base }
        } else {
            base
        };
        let mut parts: Vec<Element<ModMsg>> = vec![
            text("\u{f133}").into(), // calendar glyph
            text(c.display_text.clone()).color(color).into(),
        ];
        if !c.time_until_next.is_empty() {
            parts.push(text(format!("[{}]", c.time_until_next)).color(color).into());
        }
        mouse_area(row(parts).spacing(4).align_y(Vertical::Center))
            .on_press(ModMsg::new(Msg::Toggle))
            .into()
    }

    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let mut col: Vec<Element<ModMsg>> = vec![text("Today's Meetings").size(15).into()];
        let now = chrono::Local::now();
        let mut any = false;
        for ev in self.data.today_events.iter().filter(|e| e.is_all_day) {
            col.push(cal_row("All day", &ev.title, Color::WHITE));
            any = true;
        }
        for ev in self.data.today_events.iter().filter(|e| !e.is_all_day) {
            let color = if now > ev.end {
                Color::from_rgb(0.4, 0.4, 0.4)
            } else if now > ev.start {
                Color::from_rgb(0.0, 1.0, 0.0)
            } else if (ev.start - now) <= chrono::Duration::minutes(15) {
                Color::from_rgb(1.0, 0.67, 0.0)
            } else {
                Color::WHITE
            };
            col.push(cal_row(
                &ev.start.format("%H:%M").to_string(),
                &ev.title,
                color,
            ));
            any = true;
        }
        if !any {
            col.push(text("No meetings today").into());
        }
        Some(scrollable(column(col).spacing(6)).into())
    }
}

fn cal_row<'a>(time: &str, title: &str, color: Color) -> Element<'a, ModMsg> {
    row(vec![
        text(time.to_string())
            .color(color)
            .width(ezbar_plugin::iced::Length::Fixed(64.0))
            .into(),
        text(title.to_string()).color(color).into(),
    ])
    .spacing(8)
    .align_y(Vertical::Center)
    .into()
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
