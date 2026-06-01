//! GitHub notifications via the REST API. Port of pkg/datasource/github.go.
//! Uses conditional requests (If-Modified-Since), honours X-Poll-Interval,
//! filters by reason, and prunes merged/closed review-requested PRs.

use std::process::Command;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use ezbar_plugin::icons;
use reqwest::Client;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct GitHubNotification {
    pub id: String,
    pub reason: String,
    pub title: String,
    pub type_: String,
    pub repo_name: String,
    pub html_url: String,
    pub updated_at: DateTime<Utc>,
    pub unread: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GitHubData {
    pub notifications: Vec<GitHubNotification>,
    pub count: usize,
    pub display_text: String,
}

#[derive(Debug, Clone)]
pub struct GitHubConfig {
    pub reasons: Vec<String>,
    pub exclude_repos: Vec<String>,
}

impl Default for GitHubConfig {
    fn default() -> Self {
        GitHubConfig {
            reasons: vec![
                "review_requested".to_string(),
                "mention".to_string(),
                "assign".to_string(),
                "author".to_string(),
            ],
            exclude_repos: Vec::new(),
        }
    }
}

pub fn load_config() -> GitHubConfig {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return GitHubConfig::default(),
    };
    let path = format!("{home}/.config/ezbar/github_config.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return GitHubConfig::default(),
    };
    serde_json::from_str::<Value>(&data)
        .ok()
        .map(|v| GitHubConfig {
            reasons: v["reasons"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| GitHubConfig::default().reasons),
            exclude_repos: v["exclude_repos"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .unwrap_or_default()
}

pub fn find_token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if out.status.success() {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

fn client(token: &str) -> Result<Client, String> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("ezbar"));
    Client::builder()
        .default_headers(headers)
        .timeout(StdDuration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

fn api_to_html(api_url: &str, subject_type: &str) -> String {
    if api_url.is_empty() {
        return String::new();
    }
    let mut html = api_url.replacen("https://api.github.com/repos/", "https://github.com/", 1);
    if subject_type == "PullRequest" {
        html = html.replacen("/pulls/", "/pull/", 1);
    }
    html
}

/// Result of a fetch: either fresh data, or NotModified.
pub enum FetchResult {
    Data(GitHubData),
    NotModified,
}

pub struct GitHub {
    pub token: String,
    pub config: GitHubConfig,
    pub last_modified: Option<String>,
    pub poll_interval: u64,
}

impl GitHub {
    pub fn new(token: String) -> Self {
        GitHub {
            token,
            config: load_config(),
            last_modified: None,
            poll_interval: 60,
        }
    }

    pub async fn fetch(&mut self) -> Result<FetchResult, String> {
        let client = client(&self.token)?;
        let mut all: Vec<GitHubNotification> = Vec::new();
        let mut page = 1u32;
        let mut first = true;
        loop {
            let url = format!("https://api.github.com/notifications?per_page=50&page={page}");
            let mut req = client.get(&url);
            if first {
                if let Some(lm) = &self.last_modified {
                    req = req.header("If-Modified-Since", lm);
                }
            }
            let resp = req
                .send()
                .await
                .map_err(|e| format!("listing notifications: {e}"))?;

            if first {
                if let Some(pi) = resp.headers().get("X-Poll-Interval") {
                    if let Ok(secs) = pi.to_str().unwrap_or("").parse::<u64>() {
                        if secs > 0 {
                            self.poll_interval = secs;
                        }
                    }
                }
                if resp.status().as_u16() == 304 {
                    return Ok(FetchResult::NotModified);
                }
                if let Some(lm) = resp.headers().get("Last-Modified") {
                    self.last_modified = lm.to_str().ok().map(String::from);
                }
            }

            if !resp.status().is_success() {
                return Err(format!("github API {}", resp.status().as_u16()));
            }

            let next_link = resp
                .headers()
                .get("Link")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.contains("rel=\"next\""))
                .unwrap_or(false);

            let body: Value = resp.json().await.map_err(|e| e.to_string())?;
            if let Some(arr) = body.as_array() {
                for n in arr {
                    let subject = &n["subject"];
                    let type_ = subject["type"].as_str().unwrap_or("").to_string();
                    let updated_at = n["updated_at"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or_else(Utc::now);
                    all.push(GitHubNotification {
                        id: n["id"].as_str().unwrap_or("").to_string(),
                        reason: n["reason"].as_str().unwrap_or("").to_string(),
                        title: subject["title"].as_str().unwrap_or("").to_string(),
                        html_url: api_to_html(subject["url"].as_str().unwrap_or(""), &type_),
                        type_,
                        repo_name: n["repository"]["full_name"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        updated_at,
                        unread: n["unread"].as_bool().unwrap_or(true),
                    });
                }
            }

            first = false;
            if !next_link {
                break;
            }
            page += 1;
        }

        let all = self.prune_merged_prs(&client, all).await;
        let mut filtered = self.filter(all);
        filtered.sort_by_key(|n| std::cmp::Reverse(n.updated_at));

        let count = filtered.len();
        let data = GitHubData {
            notifications: filtered,
            count,
            display_text: format!("{} {count}", icons::GITHUB),
        };
        Ok(FetchResult::Data(data))
    }

    fn filter(&self, ns: Vec<GitHubNotification>) -> Vec<GitHubNotification> {
        ns.into_iter()
            .filter(|n| n.unread)
            .filter(|n| self.config.reasons.is_empty() || self.config.reasons.contains(&n.reason))
            .filter(|n| !self.config.exclude_repos.contains(&n.repo_name))
            .collect()
    }

    /// Drop review_requested PR notifications whose PR is merged/closed.
    async fn prune_merged_prs(
        &self,
        client: &Client,
        ns: Vec<GitHubNotification>,
    ) -> Vec<GitHubNotification> {
        static PR_URL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"github\.com/([^/]+)/([^/]+)/pull/(\d+)$").unwrap()
        });
        let re = &*PR_URL_RE;
        let checks = ns.iter().enumerate().map(|(i, n)| {
            let is_pr_review = n.reason == "review_requested" && n.type_ == "PullRequest";
            let caps = if is_pr_review {
                re.captures(&n.html_url)
            } else {
                None
            };
            let client = client.clone();
            async move {
                if let Some(c) = caps {
                    let url = format!(
                        "https://api.github.com/repos/{}/{}/pulls/{}",
                        &c[1], &c[2], &c[3]
                    );
                    if let Ok(resp) = client.get(&url).send().await {
                        if let Ok(pr) = resp.json::<Value>().await {
                            let merged = pr["merged"].as_bool().unwrap_or(false);
                            let closed = pr["state"].as_str() == Some("closed");
                            if merged || closed {
                                return Some(i);
                            }
                        }
                    }
                }
                None
            }
        });
        let excluded: Vec<Option<usize>> = iced::futures::future::join_all(checks).await;
        let excluded: std::collections::HashSet<usize> = excluded.into_iter().flatten().collect();
        ns.into_iter()
            .enumerate()
            .filter(|(i, _)| !excluded.contains(i))
            .map(|(_, n)| n)
            .collect()
    }
}

pub async fn mark_as_read(token: &str, id: &str) {
    if let Ok(c) = client(token) {
        let _ = c
            .patch(format!("https://api.github.com/notifications/threads/{id}"))
            .send()
            .await;
    }
}

pub async fn mark_all_as_read(token: &str) {
    if let Ok(c) = client(token) {
        let body = serde_json::json!({ "last_read_at": Utc::now().to_rfc3339() });
        let _ = c
            .put("https://api.github.com/notifications")
            .json(&body)
            .send()
            .await;
    }
}

pub fn reason_display_name(reason: &str) -> &str {
    match reason {
        "review_requested" => "Review Requested",
        "mention" => "Mentioned",
        "assign" => "Assigned",
        "author" => "Your PRs/Issues",
        "comment" => "Comments",
        "state_change" => "State Changes",
        "manual" => "Subscribed (manual)",
        "subscribed" => "Watching",
        other => other,
    }
}

pub fn time_ago(t: DateTime<Utc>) -> String {
    let d = Utc::now().signed_duration_since(t);
    if d.num_minutes() < 1 {
        "now".to_string()
    } else if d.num_hours() < 1 {
        format!("{}m", d.num_minutes())
    } else if d.num_hours() < 24 {
        format!("{}h", d.num_hours())
    } else {
        format!("{}d", d.num_hours() / 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_to_html() {
        assert_eq!(
            api_to_html("https://api.github.com/repos/o/r/pulls/5", "PullRequest"),
            "https://github.com/o/r/pull/5"
        );
        assert_eq!(
            api_to_html("https://api.github.com/repos/o/r/issues/3", "Issue"),
            "https://github.com/o/r/issues/3"
        );
        assert_eq!(api_to_html("", "Issue"), "");
    }

    #[test]
    fn reason_names() {
        assert_eq!(reason_display_name("review_requested"), "Review Requested");
        assert_eq!(reason_display_name("mention"), "Mentioned");
        assert_eq!(reason_display_name("weird_unknown"), "weird_unknown");
    }

    #[test]
    fn time_ago_buckets() {
        assert_eq!(time_ago(Utc::now()), "now");
        assert_eq!(time_ago(Utc::now() - chrono::Duration::minutes(5)), "5m");
        assert_eq!(time_ago(Utc::now() - chrono::Duration::hours(3)), "3h");
        assert_eq!(time_ago(Utc::now() - chrono::Duration::days(2)), "2d");
    }

    fn notif(id: &str, reason: &str, repo: &str, unread: bool) -> GitHubNotification {
        GitHubNotification {
            id: id.into(),
            reason: reason.into(),
            title: "t".into(),
            type_: "Issue".into(),
            repo_name: repo.into(),
            html_url: String::new(),
            updated_at: Utc::now(),
            unread,
        }
    }

    #[test]
    fn filter_unread_allowed_reasons_excludes_repos() {
        let gh = GitHub {
            token: String::new(),
            config: GitHubConfig {
                reasons: vec!["review_requested".into()],
                exclude_repos: vec!["o/skip".into()],
            },
            last_modified: None,
            poll_interval: 60,
        };
        let out = gh.filter(vec![
            notif("a", "review_requested", "o/keep", true), // kept
            notif("b", "review_requested", "o/keep", false), // dropped: read
            notif("c", "mention", "o/keep", true),          // dropped: reason
            notif("d", "review_requested", "o/skip", true), // dropped: excluded repo
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "a");
    }

    #[test]
    fn empty_reasons_allows_any_reason() {
        let gh = GitHub {
            token: String::new(),
            config: GitHubConfig {
                reasons: vec![],
                exclude_repos: vec![],
            },
            last_modified: None,
            poll_interval: 60,
        };
        let out = gh.filter(vec![notif("a", "anything", "o/r", true)]);
        assert_eq!(out.len(), 1);
    }
}
