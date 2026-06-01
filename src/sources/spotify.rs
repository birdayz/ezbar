//! Spotify Web API: now-playing + playback control + OAuth.
//! Port of pkg/datasource/spotify.go.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration as StdDuration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(dead_code)] // full track metadata mirrors the Go model
#[derive(Debug, Clone)]
pub struct SpotifyData {
    pub track: String,
    pub artist: String,
    pub album: String,
    pub track_string: String,
    pub icon: String,
    pub scroll_text: String,
    pub is_playing: bool,
    pub needs_auth: bool,
}

impl Default for SpotifyData {
    fn default() -> Self {
        SpotifyData {
            track: String::new(),
            artist: String::new(),
            album: String::new(),
            track_string: "--".to_string(),
            icon: "".to_string(),
            scroll_text: "--".to_string(),
            is_playing: false,
            needs_auth: false,
        }
    }
}

fn simple(track_string: &str) -> SpotifyData {
    SpotifyData {
        track_string: track_string.to_string(),
        scroll_text: track_string.to_string(),
        ..Default::default()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct SpotifyConfig {
    client_id: String,
    client_secret: String,
}

#[derive(Serialize, Deserialize, Default)]
struct WebToken {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    expires_at: i64,
}

fn cfg_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .map(|h| format!("{h}/.config/ezbar"))
}

fn read_config() -> Option<SpotifyConfig> {
    let path = format!("{}/spotify_config.json", cfg_dir()?);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn read_token() -> Option<WebToken> {
    let path = format!("{}/spotify_web_token.json", cfg_dir()?);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_token(tok: &WebToken) {
    if let Some(dir) = cfg_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(data) = serde_json::to_string(tok) {
            let _ = std::fs::write(format!("{dir}/spotify_web_token.json"), data);
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

enum Auth {
    Token(String),
    Needed,
    Error,
}

async fn load_access_token() -> Auth {
    if let Ok(t) = std::env::var("SPOTIFY_ACCESS_TOKEN") {
        if !t.is_empty() {
            return Auth::Token(t);
        }
    }
    if let Some(mut tok) = read_token() {
        if now_unix() < tok.expires_at && !tok.access_token.is_empty() {
            return Auth::Token(tok.access_token);
        }
        if !tok.refresh_token.is_empty() && refresh_token(&mut tok).await.is_ok() {
            return Auth::Token(tok.access_token);
        }
    }
    if read_config().is_some() {
        return Auth::Needed;
    }
    Auth::Error
}

fn basic_auth(cfg: &SpotifyConfig) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", cfg.client_id, cfg.client_secret))
}

async fn refresh_token(tok: &mut WebToken) -> Result<(), String> {
    let cfg = read_config().ok_or("no config")?;
    let client = reqwest::Client::new();
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .header("Authorization", format!("Basic {}", basic_auth(&cfg)))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", tok.refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("refresh failed: {}", resp.status()));
    }
    let new: WebToken = resp.json().await.map_err(|e| e.to_string())?;
    tok.access_token = new.access_token;
    if !new.refresh_token.is_empty() {
        tok.refresh_token = new.refresh_token;
    }
    tok.expires_in = new.expires_in;
    tok.expires_at = now_unix() + new.expires_in;
    save_token(tok);
    Ok(())
}

enum SpErr {
    Unauthorized,
    Network,
    Other,
}

async fn get_current_track(token: &str) -> Result<SpotifyData, SpErr> {
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(10))
        .build()
        .map_err(|_| SpErr::Other)?;
    let resp = client
        .get("https://api.spotify.com/v1/me/player/currently-playing")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|_| SpErr::Network)?;
    match resp.status().as_u16() {
        204 => Ok(simple("Nothing playing")),
        401 => Err(SpErr::Unauthorized),
        200 => {
            let v: Value = resp.json().await.map_err(|_| SpErr::Other)?;
            let name = v["item"]["name"].as_str().unwrap_or("").to_string();
            let artist = v["item"]["artists"][0]["name"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let album = v["item"]["album"]["name"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let is_playing = v["is_playing"].as_bool().unwrap_or(false);
            let scroll_text = format!("{} - {}", name, artist);
            Ok(SpotifyData {
                track: name,
                artist,
                album,
                track_string: scroll_text.clone(),
                icon: String::new(),
                scroll_text,
                is_playing,
                needs_auth: false,
            })
        }
        _ => Err(SpErr::Other),
    }
}

/// Polled every few seconds by the subscription.
pub async fn poll() -> SpotifyData {
    let token = match load_access_token().await {
        Auth::Token(t) => t,
        Auth::Needed => {
            let mut d = simple("Click to authorize");
            d.needs_auth = true;
            return d;
        }
        Auth::Error => return SpotifyData::default(),
    };
    match get_current_track(&token).await {
        Ok(d) => d,
        Err(SpErr::Unauthorized) => simple("Token expired - click to reauth"),
        Err(SpErr::Network) => simple("Network error"),
        Err(SpErr::Other) => simple("Error loading"),
    }
}

async fn bearer_token() -> Option<String> {
    match load_access_token().await {
        Auth::Token(t) => Some(t),
        _ => None,
    }
}

/// Toggle play/pause based on the last known state.
pub async fn toggle_playback(is_playing: bool) {
    let token = match bearer_token().await {
        Some(t) => t,
        None => return,
    };
    let endpoint = if is_playing {
        "https://api.spotify.com/v1/me/player/pause"
    } else {
        "https://api.spotify.com/v1/me/player/play"
    };
    let _ = reqwest::Client::new()
        .put(endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await;
}

pub async fn skip(next: bool) {
    let token = match bearer_token().await {
        Some(t) => t,
        None => return,
    };
    let endpoint = if next {
        "https://api.spotify.com/v1/me/player/next"
    } else {
        "https://api.spotify.com/v1/me/player/previous"
    };
    let _ = reqwest::Client::new()
        .post(endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await;
}

/// Blocking OAuth flow: spins up a local server on :8888, opens the browser,
/// waits for the callback, and exchanges the code for a token. Run via spawn_blocking.
pub fn authorize() -> Result<(), String> {
    let cfg = read_config().ok_or("Config file not found")?;
    let redirect = "http://127.0.0.1:8888/callback";
    let scope = "user-read-currently-playing user-read-playback-state user-modify-playback-state";
    let auth_url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&show_dialog=true",
        cfg.client_id,
        urlencode(scope),
        urlencode(redirect),
    );

    let listener = TcpListener::bind("127.0.0.1:8888").map_err(|e| format!("bind :8888: {e}"))?;
    open_browser(&auth_url);

    let code = wait_for_code(&listener)?;
    exchange_code(&cfg, &code, redirect)
}

fn wait_for_code(listener: &TcpListener) -> Result<String, String> {
    let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let first = req.lines().next().unwrap_or("");
    // GET /callback?code=XXX HTTP/1.1
    let code = first
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split("code=").nth(1))
        .map(|s| s.split('&').next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
        .ok_or("no authorization code received")?;

    let body = "<html><head><title>Spotify Authorization</title></head><body style=\"font-family: Arial, sans-serif; text-align: center; margin-top: 100px;\"><h1>Authorization Successful!</h1><p>You can close this window now.</p></body></html>";
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
    Ok(code)
}

fn exchange_code(cfg: &SpotifyConfig, code: &str, redirect: &str) -> Result<(), String> {
    // Blocking exchange using reqwest's blocking client (we are on a blocking thread).
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .header("Authorization", format!("Basic {}", basic_auth(cfg)))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
        ])
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("token exchange failed: {}", resp.status()));
    }
    let mut tok: WebToken = resp.json().map_err(|e| e.to_string())?;
    tok.expires_at = now_unix() + tok.expires_in;
    save_token(&tok);
    Ok(())
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn open_browser(url: &str) {
    for opener in ["xdg-open", "open"] {
        if std::process::Command::new(opener).arg(url).spawn().is_ok() {
            return;
        }
    }
    log::debug!("open this URL to authorize Spotify: {url}");
}
