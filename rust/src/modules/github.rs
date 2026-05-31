//! GitHub module: count label + click-toggle interactive popup (grouped
//! notifications; rows open/dismiss, "clear all" marks read).
//! Validates: click popup (PopupMode::Click) with input routed back to the module.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{column, mouse_area, row, scrollable, text};
use ezbar_plugin::iced::{Color, Element, Length, Subscription, Task};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::github::{self, GitHubData, GitHubNotification};

enum Msg {
    Loaded(GitHubData),
    TogglePopup,
    Open(String, String), // url, id
    MarkRead(String),
    MarkAll,
    Done,
}

pub struct GitHub {
    instance: u64,
    data: GitHubData,
    token: Option<String>,
}

impl GitHub {
    pub fn new(instance: u64) -> Self {
        GitHub {
            instance,
            data: GitHubData {
                display_text: "GH …".to_string(),
                ..Default::default()
            },
            token: github::find_token(),
        }
    }

    fn remove(&mut self, id: &str) {
        self.data.notifications.retain(|n| n.id != id);
        self.data.count = self.data.notifications.len();
        self.data.display_text = format!("GH {}", self.data.count);
    }

    fn mark_task(&self, id: &str) -> Response {
        if let Some(token) = self.token.clone() {
            let id = id.to_string();
            Response::task(Task::perform(
                async move { github::mark_as_read(&token, &id).await },
                |_| ModMsg::new(Msg::Done),
            ))
        } else {
            Response::none()
        }
    }
}

impl Module for GitHub {
    fn id(&self) -> &str {
        "github"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, gh_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Loaded(d)) => {
                self.data = d.clone();
                Response::none()
            }
            Some(Msg::TogglePopup) => Response::request(HostRequest::OpenPopup(PopupMode::Click)),
            Some(Msg::MarkAll) => {
                self.data = GitHubData {
                    display_text: "GH 0".to_string(),
                    ..Default::default()
                };
                let mut resp = Response::request(HostRequest::ClosePopup);
                if let Some(token) = self.token.clone() {
                    resp.task = Task::perform(
                        async move { github::mark_all_as_read(&token).await },
                        |_| ModMsg::new(Msg::Done),
                    );
                }
                resp
            }
            Some(Msg::MarkRead(id)) => {
                let t = self.mark_task(id);
                self.remove(id);
                t
            }
            Some(Msg::Open(url, id)) => {
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                let t = self.mark_task(id);
                self.remove(id);
                t
            }
            _ => Response::none(),
        }
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        let color = if self.data.count > 0 {
            Color::from_rgb(0.345, 0.65, 1.0)
        } else {
            Color::WHITE
        };
        mouse_area(text(self.data.display_text.clone()).color(color))
            .on_press(ModMsg::new(Msg::TogglePopup))
            .into()
    }

    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let mut col: Vec<Element<ModMsg>> = Vec::new();

        let mut header: Vec<Element<ModMsg>> = vec![text(format!(
            "GitHub Notifications ({})",
            self.data.count
        ))
        .size(15)
        .width(Length::Fill)
        .into()];
        header.push(
            mouse_area(text("[clear all]").color(Color::from_rgb(0.55, 0.65, 0.8)))
                .on_press(ModMsg::new(Msg::MarkAll))
                .into(),
        );
        col.push(row(header).spacing(8).align_y(Vertical::Center).into());

        let order = [
            "review_requested", "mention", "assign", "author", "comment",
            "state_change", "manual", "subscribed",
        ];
        for reason in order {
            let group: Vec<&GitHubNotification> = self
                .data
                .notifications
                .iter()
                .filter(|n| n.reason == reason)
                .collect();
            if group.is_empty() {
                continue;
            }
            col.push(
                text(format!("{} ({})", github::reason_display_name(reason), group.len()))
                    .color(Color::from_rgb(0.345, 0.65, 1.0))
                    .into(),
            );
            for n in group.iter().take(10) {
                col.push(notification_row(n));
            }
        }
        if self.data.notifications.is_empty() {
            col.push(text("No notifications").into());
        }
        Some(scrollable(column(col).spacing(4)).into())
    }
}

fn notification_row<'a>(n: &GitHubNotification) -> Element<'a, ModMsg> {
    let icon = match n.type_.as_str() {
        "PullRequest" => "PR",
        "Issue" => "IS",
        "Release" => "RE",
        _ => "  ",
    };
    let repo = n.repo_name.rsplit('/').next().unwrap_or(&n.repo_name).to_string();
    let r = row(vec![
        text(icon).color(Color::from_rgb(0.55, 0.58, 0.6)).into(),
        text(trunc(&repo, 15))
            .color(Color::from_rgb(0.55, 0.58, 0.6))
            .width(Length::Fixed(110.0))
            .into(),
        text(trunc(&n.title, 45)).width(Length::Fill).into(),
        text(github::time_ago(n.updated_at))
            .color(Color::from_rgb(0.55, 0.58, 0.6))
            .into(),
    ])
    .spacing(8)
    .align_y(Vertical::Center);

    let id = n.id.clone();
    if n.html_url.is_empty() {
        mouse_area(r).on_press(ModMsg::new(Msg::MarkRead(id))).into()
    } else {
        let url = n.html_url.clone();
        mouse_area(r)
            .on_press(ModMsg::new(Msg::Open(url, id.clone())))
            .on_right_press(ModMsg::new(Msg::MarkRead(id)))
            .into()
    }
}

fn trunc(s: &str, max: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= max {
        return s.to_string();
    }
    let mut out: String = c[..max.saturating_sub(2)].iter().collect();
    out.push_str("..");
    out
}

fn gh_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            let token = match github::find_token() {
                Some(t) => t,
                None => {
                    let _ = out
                        .send(ModMsg::new(Msg::Loaded(GitHubData {
                            display_text: "GH ?".to_string(),
                            ..Default::default()
                        })))
                        .await;
                    return;
                }
            };
            let mut gh = github::GitHub::new(token);
            loop {
                if let Ok(github::FetchResult::Data(d)) = gh.fetch().await {
                    let _ = out.send(ModMsg::new(Msg::Loaded(d))).await;
                }
                tokio::time::sleep(Duration::from_secs(gh.poll_interval.max(1))).await;
            }
        },
    )
}
