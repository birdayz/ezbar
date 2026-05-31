//! Sway IPC: workspace list + focused window title.
//! Port of the sway goroutine in cmd/ezbar/main.go. Uses the blocking swayipc
//! client on dedicated blocking threads, bridged into an iced subscription.

use std::time::Duration;

use iced::futures::{SinkExt, Stream};
use iced::stream;
use swayipc::{Connection, Event, EventType, Node, WorkspaceChange};

#[derive(Debug, Clone)]
pub struct Workspace {
    pub name: String,
    pub focused: bool,
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

fn run_workspaces(tx: &tokio::sync::mpsc::Sender<SwayUpdate>) -> swayipc::Fallible<()> {
    let mut conn = Connection::new()?;
    let mut state: Vec<Workspace> = conn
        .get_workspaces()?
        .into_iter()
        .map(|w| Workspace {
            name: w.name,
            focused: w.focused,
        })
        .collect();
    sort_workspaces(&mut state);
    let _ = tx.blocking_send(SwayUpdate::Workspaces(state.clone()));

    let conn = Connection::new()?;
    let events = conn.subscribe([EventType::Workspace])?;
    for event in events {
        let we = match event? {
            Event::Workspace(we) => *we,
            _ => continue,
        };
        match we.change {
            WorkspaceChange::Init => {
                if let Some(c) = we.current {
                    state.push(Workspace {
                        name: c.name.unwrap_or_default(),
                        focused: c.focused,
                    });
                }
            }
            WorkspaceChange::Empty => {
                if let Some(c) = we.current {
                    let name = c.name.unwrap_or_default();
                    state.retain(|w| w.name != name);
                }
            }
            WorkspaceChange::Focus => {
                let cur = we.current.and_then(|c| c.name).unwrap_or_default();
                let old = we.old.and_then(|c| c.name).unwrap_or_default();
                for w in state.iter_mut() {
                    if w.name == cur {
                        w.focused = true;
                    }
                    if w.name == old {
                        w.focused = false;
                    }
                }
            }
            _ => {}
        }
        sort_workspaces(&mut state);
        if tx
            .blocking_send(SwayUpdate::Workspaces(state.clone()))
            .is_err()
        {
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

    #[test]
    fn sorts_numerically_not_lexically() {
        let mut ws = vec![
            Workspace {
                name: "10".into(),
                focused: false,
            },
            Workspace {
                name: "2".into(),
                focused: false,
            },
            Workspace {
                name: "1".into(),
                focused: true,
            },
        ];
        sort_workspaces(&mut ws);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["1", "2", "10"]);
    }

    #[test]
    fn named_workspaces_sort_as_strings() {
        let mut ws = vec![
            Workspace {
                name: "web".into(),
                focused: false,
            },
            Workspace {
                name: "code".into(),
                focused: false,
            },
        ];
        sort_workspaces(&mut ws);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["code", "web"]);
    }
}
