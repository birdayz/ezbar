//! Sway IPC: workspace list + focused window title.
//! Port of the sway goroutine in cmd/ezbar/main.go. Uses the blocking swayipc
//! client on dedicated blocking threads, bridged into an iced subscription.

use std::time::Duration;

use iced::futures::{SinkExt, Stream};
use iced::stream;
use swayipc::{Connection, Event, EventType, Node};

#[derive(Debug, Clone)]
pub struct Workspace {
    pub name: String,
    /// focused on the focused output (the active workspace)
    pub focused: bool,
    /// visible on *some* output (focused, or active on another monitor)
    pub visible: bool,
    /// flagged urgent by a client
    pub urgent: bool,
}

#[derive(Debug, Clone)]
pub enum SwayUpdate {
    Workspaces(Vec<Workspace>),
    Title(String),
}

/// Long-lived subscription stream emitting workspace + title updates.
pub fn sway_stream() -> impl Stream<Item = SwayUpdate> {
    stream::channel(
        50,
        |mut output: iced::futures::channel::mpsc::Sender<SwayUpdate>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<SwayUpdate>(50);

            let tx_ws = tx.clone();
            tokio::task::spawn_blocking(move || loop {
                if let Err(e) = run_workspaces(&tx_ws) {
                    log::warn!("sway workspace error: {e}");
                }
                std::thread::sleep(Duration::from_secs(1));
            });

            let tx_title = tx.clone();
            tokio::task::spawn_blocking(move || loop {
                if let Err(e) = run_title(&tx_title) {
                    log::warn!("sway title error: {e}");
                }
                std::thread::sleep(Duration::from_secs(1));
            });

            while let Some(update) = rx.recv().await {
                if output.send(update).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn sort_workspaces(ws: &mut [Workspace]) {
    ws.sort_by(
        |a, b| match (a.name.parse::<i32>(), b.name.parse::<i32>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => a.name.cmp(&b.name),
        },
    );
}

/// Snapshot the full workspace list (name + focused/visible/urgent state).
fn fetch_workspaces(conn: &mut Connection) -> swayipc::Fallible<Vec<Workspace>> {
    let mut state: Vec<Workspace> = conn
        .get_workspaces()?
        .into_iter()
        .map(|w| Workspace {
            name: w.name,
            focused: w.focused,
            visible: w.visible,
            urgent: w.urgent,
        })
        .collect();
    sort_workspaces(&mut state);
    Ok(state)
}

fn run_workspaces(tx: &tokio::sync::mpsc::Sender<SwayUpdate>) -> swayipc::Fallible<()> {
    let mut conn = Connection::new()?;
    let state = fetch_workspaces(&mut conn)?;
    let _ = tx.blocking_send(SwayUpdate::Workspaces(state));

    // Re-snapshot on every workspace event. Sway workspace events are infrequent
    // (focus/init/empty/urgent), so a full re-query is cheaper than tracking
    // visible/urgent deltas by hand — and it can never drift out of sync.
    let mut q = Connection::new()?;
    let events = Connection::new()?.subscribe([EventType::Workspace])?;
    for event in events {
        let _we = match event? {
            Event::Workspace(we) => *we,
            _ => continue,
        };
        let state = fetch_workspaces(&mut q)?;
        if tx.blocking_send(SwayUpdate::Workspaces(state)).is_err() {
            break;
        }
    }
    Ok(())
}

fn run_title(tx: &tokio::sync::mpsc::Sender<SwayUpdate>) -> swayipc::Fallible<()> {
    let mut conn = Connection::new()?;
    loop {
        let tree = conn.get_tree()?;
        if let Some(name) = focused_name(&tree) {
            if tx.blocking_send(SwayUpdate::Title(name)).is_err() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn focused_name(node: &Node) -> Option<String> {
    if node.focused {
        return Some(node.name.clone().unwrap_or_default());
    }
    for c in &node.nodes {
        if let Some(n) = focused_name(c) {
            return Some(n);
        }
    }
    for c in &node.floating_nodes {
        if let Some(n) = focused_name(c) {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.into(),
            focused: false,
            visible: false,
            urgent: false,
        }
    }

    #[test]
    fn sorts_numerically_not_lexically() {
        let mut ws = vec![ws("10"), ws("2"), ws("1")];
        sort_workspaces(&mut ws);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["1", "2", "10"]);
    }

    #[test]
    fn named_workspaces_sort_as_strings() {
        let mut ws = vec![ws("web"), ws("code")];
        sort_workspaces(&mut ws);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["code", "web"]);
    }
}
