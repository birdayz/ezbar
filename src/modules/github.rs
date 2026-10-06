//! GitHub module: count label + click-toggle interactive popup (grouped
//! notifications; rows open/dismiss, "clear all" marks read; then the user's own
//! open PRs, click to open).
//! Validates: click popup (PopupMode::Click) with input routed back to the module.

use std::path::PathBuf;
use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{column, mouse_area, row, scrollable, text, Space};
use ezbar_plugin::iced::{Color, Element, Length, Subscription, Task};
use ezbar_plugin::icons::Icon;
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::github::{self, FetchResult, GitHubData, GitHubNotification, MyPr};

/// How often to retry token discovery while none is found (no `gh`, not logged in,
/// no token file), so setting one up needs no restart.
const NO_TOKEN_RETRY: Duration = Duration::from_secs(60);
/// Popup cap for the "My open PRs" list.
const MAX_PRS: usize = 30;

enum Msg {
    /// Token discovery result from the stream; `None` = nothing found (yet).
    Token(Option<String>),
    /// Fresh notifications, `Ok(None)` = 304 Not Modified, or a fetch error.
    Notifications(Result<Option<GitHubData>, String>),
    Prs(Result<Vec<MyPr>, String>),
    TogglePopup,
    Open(String, String), // url, id
    OpenPr(String),       // url
    MarkRead(String),
    MarkAll,
    Done,
}

pub struct GitHub {
    instance: u64,
    data: GitHubData,
    token: Option<String>,
    /// Token discovery ran and found nothing: the popup shows setup instructions.
    no_token: bool,
    token_file: Option<PathBuf>,
    show_prs: bool,
    /// `None` until the first PR search returns.
    prs: Option<Vec<MyPr>>,
    notif_error: Option<String>,
    prs_error: Option<String>,
}

impl GitHub {
    /// `[modules.github]`: `token_file` (default `~/.config/ezbar/github_token`) and
    /// `my_prs` (default `true`).
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        GitHub {
            instance,
            data: GitHubData {
                display_text: "…".to_string(),
                ..Default::default()
            },
            token: None,
            no_token: false,
            token_file: cfg
                .get("token_file")
                .and_then(|v| v.as_str())
                .map(super::expand_tilde)
                .or_else(github::default_token_file),
            show_prs: cfg.get("my_prs").and_then(|v| v.as_bool()).unwrap_or(true),
            prs: None,
            notif_error: None,
            prs_error: None,
        }
    }

    /// The token file path for the setup hint, `~`-abbreviated.
    fn token_file_hint(&self) -> String {
        let Some(p) = &self.token_file else {
            return "~/.config/ezbar/github_token".to_string();
        };
        if let Some(home) = std::env::var_os("HOME") {
            if let Ok(rest) = p.strip_prefix(&home) {
                return format!("~/{}", rest.display());
            }
        }
        p.display().to_string()
    }

    fn remove(&mut self, id: &str) {
        self.data.notifications.retain(|n| n.id != id);
        self.data.count = self.data.notifications.len();
        self.data.display_text = self.data.count.to_string();
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
        // bake the config into the recipe so a config change re-rolls the stream
        Subscription::run_with(
            (self.instance, self.token_file.clone(), self.show_prs),
            gh_stream,
        )
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Token(t)) => {
                self.token = t.clone();
                self.no_token = t.is_none();
                if self.no_token {
                    self.data.display_text = "?".to_string();
                } else if self.data.display_text == "?" {
                    self.data.display_text = "…".to_string();
                }
                Response::none()
            }
            Some(Msg::Notifications(r)) => {
                match r {
                    Ok(Some(d)) => {
                        self.data = d.clone();
                        self.notif_error = None;
                    }
                    Ok(None) => self.notif_error = None,
                    Err(e) => {
                        self.notif_error = Some(e.clone());
                        if self.data.display_text == "…" {
                            self.data.display_text = "!".to_string();
                        }
                    }
                }
                Response::none()
            }
            Some(Msg::Prs(r)) => {
                match r {
                    Ok(p) => {
                        self.prs = Some(p.clone());
                        self.prs_error = None;
                    }
                    Err(e) => self.prs_error = Some(e.clone()),
                }
                Response::none()
            }
            Some(Msg::OpenPr(url)) => {
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                Response::none()
            }
            Some(Msg::TogglePopup) => Response::request(HostRequest::OpenPopup(PopupMode::Click)),
            Some(Msg::MarkAll) => {
                self.data = GitHubData {
                    display_text: "0".to_string(),
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

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let color = if self.data.count > 0 {
            Color::from_rgb(0.345, 0.65, 1.0)
        } else {
            Color::WHITE
        };
        mouse_area(
            row(vec![
                Icon::Github.view(ctx.theme.text_size, color),
                text(self.data.display_text.clone()).color(color).into(),
            ])
            .spacing(5)
            .align_y(Vertical::Center),
        )
        .on_press(ModMsg::new(Msg::TogglePopup))
        .into()
    }

    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        if self.no_token {
            return Some(
                column![
                    text("GitHub not set up").size(15),
                    text(format!(
                        "Install gh and run `gh auth login`,\nor save a token to {}",
                        self.token_file_hint()
                    ))
                    .color(ctx.fg_dim()),
                ]
                .spacing(4)
                .into(),
            );
        }
        let mut col: Vec<Element<ModMsg>> = Vec::new();

        let mut header: Vec<Element<ModMsg>> =
            vec![text(format!("GitHub Notifications ({})", self.data.count))
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
            "review_requested",
            "mention",
            "assign",
            "author",
            "comment",
            "state_change",
            "manual",
            "subscribed",
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
                text(format!(
                    "{} ({})",
                    github::reason_display_name(reason),
                    group.len()
                ))
                .color(Color::from_rgb(0.345, 0.65, 1.0))
                .into(),
            );
            for n in group.iter().take(10) {
                col.push(notification_row(n));
            }
        }
        if let Some(e) = &self.notif_error {
            col.push(text(e.clone()).color(ctx.warn()).into());
        } else if self.data.notifications.is_empty() {
            col.push(text("No notifications").into());
        }

        if self.show_prs {
            col.push(Space::new().height(Length::Fixed(6.0)).into());
            col.push(
                text(match &self.prs {
                    Some(prs) => format!("My open PRs ({})", prs.len()),
                    None => "My open PRs".to_string(),
                })
                .size(15)
                .into(),
            );
            if let Some(e) = &self.prs_error {
                col.push(text(e.clone()).color(ctx.warn()).into());
            }
            match &self.prs {
                Some(prs) if prs.is_empty() => col.push(text("No open PRs").into()),
                Some(prs) => {
                    for pr in prs.iter().take(MAX_PRS) {
                        col.push(pr_row(pr));
                    }
                }
                None if self.prs_error.is_none() => {
                    col.push(text("…").color(ctx.fg_dim()).into());
                }
                None => {}
            }
        }
        Some(scrollable(column(col).spacing(4)).into())
    }
}

/// One of the user's open PRs; click opens it in the browser. Drafts are dimmed.
fn pr_row<'a>(pr: &MyPr) -> Element<'a, ModMsg> {
    let dim = Color::from_rgb(0.55, 0.58, 0.6);
    let repo = pr
        .repo_name
        .rsplit('/')
        .next()
        .unwrap_or(&pr.repo_name)
        .to_string();
    let title = text(trunc(&format!("#{} {}", pr.number, pr.title), 45)).width(Length::Fill);
    let r = row(vec![
        text(if pr.draft { "DR" } else { "PR" }).color(dim).into(),
        text(trunc(&repo, 15))
            .color(dim)
            .width(Length::Fixed(110.0))
            .into(),
        if pr.draft { title.color(dim) } else { title }.into(),
        text(github::time_ago(pr.updated_at)).color(dim).into(),
    ])
    .spacing(8)
    .align_y(Vertical::Center);
    mouse_area(r)
        .on_press(ModMsg::new(Msg::OpenPr(pr.html_url.clone())))
        .into()
}

fn notification_row<'a>(n: &GitHubNotification) -> Element<'a, ModMsg> {
    let icon = match n.type_.as_str() {
        "PullRequest" => "PR",
        "Issue" => "IS",
        "Release" => "RE",
        _ => "  ",
    };
    let repo = n
        .repo_name
        .rsplit('/')
        .next()
        .unwrap_or(&n.repo_name)
        .to_string();
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
        mouse_area(r)
            .on_press(ModMsg::new(Msg::MarkRead(id)))
            .into()
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

fn gh_stream(key: &(u64, Option<PathBuf>, bool)) -> impl Stream<Item = ModMsg> {
    let (_, token_file, show_prs) = key.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            let token = loop {
                let tf = token_file.clone();
                // `gh auth token` is a blocking subprocess: keep it off the runtime threads.
                let t = tokio::task::spawn_blocking(move || github::find_token(tf.as_deref()))
                    .await
                    .ok()
                    .flatten();
                let _ = out.send(ModMsg::new(Msg::Token(t.clone()))).await;
                if let Some(t) = t {
                    break t;
                }
                tokio::time::sleep(NO_TOKEN_RETRY).await;
            };
            let mut gh = github::GitHub::new(token);
            loop {
                let notifs = match gh.fetch().await {
                    Ok(FetchResult::Data(d)) => Ok(Some(d)),
                    Ok(FetchResult::NotModified) => Ok(None),
                    Err(e) => Err(e),
                };
                let _ = out.send(ModMsg::new(Msg::Notifications(notifs))).await;
                if show_prs {
                    let prs = gh.fetch_my_prs().await;
                    let _ = out.send(ModMsg::new(Msg::Prs(prs))).await;
                }
                tokio::time::sleep(Duration::from_secs(gh.poll_interval.max(1))).await;
            }
        },
    )
}
