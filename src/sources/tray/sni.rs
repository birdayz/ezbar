//! StatusNotifierWatcher/Host and dbusmenu. Peer addresses are canonical unique
//! bus names, never commands. Slow/broken peers have bounded calls and queues.
use super::{icon, publish, Action, Command, Item, ItemId, Snapshot, MAX_ITEMS};
use iced::futures::{stream, StreamExt};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use zbus::{
    message::Header,
    object_server::SignalEmitter,
    zvariant::{OwnedValue, Value},
    Connection, Proxy,
};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCH_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const DBUS: &str = "org.freedesktop.DBus";
const DEADLINE: Duration = Duration::from_millis(750);

type Properties = HashMap<String, OwnedValue>;
type Layout = (i32, Properties, Vec<OwnedValue>);

#[derive(Default)]
struct State {
    items: Vec<ItemId>,
    hosts: Vec<String>,
}
struct Watcher(Arc<Mutex<State>>);

fn address(id: &ItemId) -> String {
    match id {
        ItemId::Sni { owner, path } => format!("{owner}{path}"),
        _ => String::new(),
    }
}
fn parse_address(s: &str) -> Option<(String, String)> {
    let (owner, path) = match s.find('/') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, "/StatusNotifierItem"),
    };
    zbus::names::BusName::try_from(owner).ok()?;
    zbus::zvariant::ObjectPath::try_from(path).ok()?;
    Some((owner.to_owned(), path.to_owned()))
}
async fn owner(conn: &Connection, name: &str) -> zbus::Result<String> {
    conn.call_method(
        Some(DBUS),
        "/org/freedesktop/DBus",
        Some(DBUS),
        "GetNameOwner",
        &(name,),
    )
    .await?
    .body()
    .deserialize()
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("Missing sender".into()))?
            .to_string();
        let (name, path) = if service.starts_with('/') {
            (sender.clone(), service.to_owned())
        } else {
            parse_address(service)
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Invalid item address".into()))?
        };
        zbus::zvariant::ObjectPath::try_from(path.as_str())
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let resolved = tokio::time::timeout(DEADLINE, owner(conn, &name))
            .await
            .map_err(|_| zbus::fdo::Error::Failed("Owner lookup timed out".into()))?
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        if resolved != sender {
            return Err(zbus::fdo::Error::AccessDenied(
                "Register only your own items".into(),
            ));
        }
        let item = ItemId::Sni {
            owner: resolved,
            path,
        };
        let added = {
            let mut state = self.0.lock().unwrap();
            if state.items.contains(&item) {
                false
            } else {
                if state.items.len() >= MAX_ITEMS {
                    return Err(zbus::fdo::Error::LimitsExceeded("Tray full".into()));
                }
                state.items.push(item.clone());
                true
            }
        };
        if added {
            Self::status_notifier_item_registered(&emitter, &address(&item)).await?;
        }
        Ok(())
    }

    async fn register_status_notifier_host(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("Missing sender".into()))?
            .to_string();
        let resolved = tokio::time::timeout(DEADLINE, owner(conn, service))
            .await
            .map_err(|_| zbus::fdo::Error::Failed("Owner lookup timed out".into()))?
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        if sender != resolved {
            return Err(zbus::fdo::Error::AccessDenied(
                "Register only your own host".into(),
            ));
        }
        let added = {
            let mut state = self.0.lock().unwrap();
            if state.hosts.contains(&resolved) {
                false
            } else {
                if state.hosts.len() >= MAX_ITEMS {
                    return Err(zbus::fdo::Error::LimitsExceeded("Too many hosts".into()));
                }
                state.hosts.push(resolved);
                true
            }
        };
        if added {
            Self::status_notifier_host_registered(&emitter).await?;
        }
        Ok(())
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.lock().unwrap().items.iter().map(address).collect()
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn is_status_notifier_host_registered(&self) -> bool {
        !self.0.lock().unwrap().hosts.is_empty()
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

fn string(props: &Properties, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("")
        .chars()
        .take(512)
        .collect()
}
fn boolean(props: &Properties, key: &str, default: bool) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(default)
}
fn pixmaps(props: &Properties, key: &str) -> Option<icon::Icon> {
    let value = props.get(key)?.try_clone().ok()?;
    icon::best_pixmap(Vec::<(i32, i32, Vec<u8>)>::try_from(value).ok()?)
}

async fn read_item(conn: &Connection, name: String) -> Option<Item> {
    let (name, path) = parse_address(&name)?;
    let resolved = owner(conn, &name).await.ok()?;
    let response = conn
        .call_method(
            Some(resolved.as_str()),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(ITEM,),
        )
        .await
        .ok()?;
    let props: Properties = response.body().deserialize().ok()?;
    let status = string(&props, "Status");
    let attention = status == "NeedsAttention";
    let mut icon = if attention {
        pixmaps(&props, "AttentionIconPixmap")
    } else {
        None
    };
    if icon.is_none() {
        icon = pixmaps(&props, "IconPixmap");
    }
    if icon.is_none() {
        let mut name = if attention {
            string(&props, "AttentionIconName")
        } else {
            String::new()
        };
        if name.is_empty() {
            name = string(&props, "IconName");
        }
        let theme_path = string(&props, "IconThemePath");
        // A timed-out filesystem read cannot be cancelled by Tokio. Keep its
        // permit inside the blocking job so retries cannot accumulate workers.
        static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let permit = DECODERS.acquire().await.ok()?;
        icon = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            icon::named(&name, &theme_path)
        })
        .await
        .ok()
        .flatten();
    }
    let mut title = string(&props, "Title");
    if title.is_empty() {
        title = string(&props, "Id");
    }
    if title.is_empty() {
        title = "Tray application".into();
    }
    let menu = props
        .get("Menu")
        .and_then(|v| <&zbus::zvariant::ObjectPath>::try_from(v).ok())
        .map(|p| p.as_str().to_owned())
        .filter(|p| p != "/" && p != "/NO_DBUSMENU");
    Some(Item {
        id: ItemId::Sni {
            owner: resolved,
            path,
        },
        title,
        icon,
        passive: status == "Passive",
        attention,
        is_menu: boolean(&props, "ItemIsMenu", false),
        menu,
    })
}

pub async fn run(mut commands: mpsc::Receiver<Command>, updates: watch::Sender<Snapshot>) {
    loop {
        match session(&mut commands, &updates).await {
            Ok(()) => return,
            Err(e) => {
                log::warn!("tray D-Bus disconnected/unavailable: {e}");
                publish(&updates, Vec::new(), false);
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn session(
    commands: &mut mpsc::Receiver<Command>,
    updates: &watch::Sender<Snapshot>,
) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::session()?
        .method_timeout(DEADLINE)
        .max_queued(64)
        .build()
        .await?;
    let state = Arc::new(Mutex::new(State::default()));
    conn.object_server()
        .at(WATCH_PATH, Watcher(state.clone()))
        .await?;
    let unique = conn.unique_name().unwrap().to_string();
    let discovery = async {
        let mut registered_with = String::new();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            // DoNotQueue: never replace an existing watcher or queue ownership
            // behind it. Retry when it disappears (also covers bar restarts).
            let current = match owner(&conn, WATCHER).await {
                Ok(name) => name,
                Err(_) => {
                    conn.request_name_with_flags(
                        WATCHER,
                        zbus::fdo::RequestNameFlags::DoNotQueue.into(),
                    )
                    .await?;
                    owner(&conn, WATCHER).await?
                }
            };
            if current != registered_with {
                let proxy = Proxy::new(&conn, WATCHER, WATCH_PATH, WATCHER).await?;
                let _: () = proxy
                    .call("RegisterStatusNotifierHost", &(unique.as_str(),))
                    .await?;
                registered_with = current.clone();
            }
            if current == unique {
                prune(&conn, &state).await;
            }
            let proxy = Proxy::new(&conn, WATCHER, WATCH_PATH, WATCHER).await?;
            let names: Vec<String> = match tokio::time::timeout(
                DEADLINE,
                proxy.get_property("RegisteredStatusNotifierItems"),
            )
            .await
            {
                Ok(Ok(names)) => names,
                _ => {
                    registered_with.clear();
                    continue;
                }
            };
            let mut items: Vec<Item> = stream::iter(names.into_iter().take(MAX_ITEMS))
                .map(|name| {
                    let conn = &conn;
                    async move {
                        tokio::time::timeout(DEADLINE, read_item(conn, name))
                            .await
                            .ok()
                            .flatten()
                    }
                })
                .buffer_unordered(8)
                .filter_map(|item| async { item })
                .collect()
                .await;
            items.sort_by(|a, b| a.id.cmp(&b.id));
            items.dedup_by(|a, b| a.id == b.id);
            publish(updates, items, false);
        }
        #[allow(unreachable_code)]
        Ok::<(), zbus::Error>(())
    };
    tokio::pin!(discovery);
    // Discovery and actions are polled independently: slow icon properties must
    // not delay clicks. Dropping the session cancels all outstanding actions.
    let mut actions = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            result = &mut discovery => return result,
            _ = actions.join_next(), if !actions.is_empty() => {}
            command = commands.recv(), if actions.len() < 8 => {
                let Some(command) = command else { return Ok(()); };
                let conn = conn.clone();
                actions.spawn(async move {
                    if tokio::time::timeout(Duration::from_secs(2), execute(&conn, command)).await.is_err() {
                        log::debug!("tray action timed out");
                    }
                });
            }
        }
    }
}

async fn prune(conn: &Connection, state: &Arc<Mutex<State>>) {
    let (items, hosts) = {
        let s = state.lock().unwrap();
        (s.items.clone(), s.hosts.clone())
    };
    let mut names: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            ItemId::Sni { owner, .. } => Some(owner.clone()),
            _ => None,
        })
        .chain(hosts)
        .collect();
    names.sort();
    names.dedup();
    let dead: Vec<String> = stream::iter(names)
        .map(|name| async move {
            match tokio::time::timeout(DEADLINE, owner(conn, &name)).await {
                Ok(Err(_)) => Some(name),
                _ => None,
            }
        })
        .buffer_unordered(8)
        .filter_map(|n| async { n })
        .collect()
        .await;
    let (removed, removed_host) = {
        let mut s = state.lock().unwrap();
        let removed: Vec<ItemId> = s
            .items
            .iter()
            .filter(|i| matches!(i, ItemId::Sni { owner, .. } if dead.contains(owner)))
            .cloned()
            .collect();
        s.items.retain(|i| !removed.contains(i));
        let count = s.hosts.len();
        s.hosts.retain(|h| !dead.contains(h));
        (removed, count != s.hosts.len())
    };
    if let Ok(emitter) = SignalEmitter::new(conn, WATCH_PATH) {
        for item in removed {
            let _ = Watcher::status_notifier_item_unregistered(&emitter, &address(&item)).await;
        }
        if removed_host {
            let _ = Watcher::status_notifier_host_unregistered(&emitter).await;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuNode {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    pub checked: Option<bool>,
    pub children: Vec<MenuNode>,
}

fn menu_node(layout: Layout, depth: usize, remaining: &mut usize) -> Option<MenuNode> {
    if depth > 8 || *remaining == 0 {
        return None;
    }
    *remaining -= 1;
    let (id, props, children) = layout;
    if !boolean(&props, "visible", true) {
        return None;
    }
    let children = children
        .into_iter()
        .filter_map(|v| {
            let structure = zbus::zvariant::Structure::try_from(v).ok()?;
            let layout = <Layout>::try_from(structure).ok()?;
            menu_node(layout, depth + 1, remaining)
        })
        .collect();
    let toggle = string(&props, "toggle-type");
    Some(MenuNode {
        id,
        label: menu_label(&string(&props, "label")),
        enabled: boolean(&props, "enabled", true),
        separator: string(&props, "type") == "separator",
        checked: (!toggle.is_empty()).then(|| {
            props
                .get("toggle-state")
                .and_then(|v| i32::try_from(v).ok())
                == Some(1)
        }),
        children,
    })
}

fn menu_label(s: &str) -> String {
    let mut chars = s.chars();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c == '_' {
            if let Some(next) = chars.next() {
                result.push(next);
            }
        } else {
            result.push(c);
        }
    }
    result
}

async fn execute(conn: &Connection, command: Command) {
    let ItemId::Sni { owner, path } = command.id else {
        return;
    };
    let (x, y) = command.position;
    let result: zbus::Result<()> = async {
        if let Some(reply) = command.reply {
            let menu = command
                .menu
                .ok_or_else(|| zbus::Error::Failure("No menu path".into()))?;
            let proxy = Proxy::new(
                conn,
                owner.as_str(),
                menu.as_str(),
                "com.canonical.dbusmenu",
            )
            .await?;
            let _: zbus::Result<bool> = proxy.call("AboutToShow", &(0i32,)).await;
            let (_revision, layout): (u32, Layout) = proxy
                .call("GetLayout", &(0i32, 8i32, Vec::<String>::new()))
                .await?;
            let nodes = menu_node(layout, 0, &mut 256)
                .map(|n| n.children)
                .unwrap_or_default();
            let _ = reply.send(nodes);
        } else if let Action::MenuClick(id) = command.action {
            if let Some(menu) = command.menu {
                let proxy = Proxy::new(
                    conn,
                    owner.as_str(),
                    menu.as_str(),
                    "com.canonical.dbusmenu",
                )
                .await?;
                let _: () = proxy
                    .call("Event", &(id, "clicked", Value::from(0i32), 0u32))
                    .await?;
            }
        } else {
            let proxy = Proxy::new(conn, owner.as_str(), path.as_str(), ITEM).await?;
            let _: () = match command.action {
                Action::Activate => proxy.call("Activate", &(x, y)).await?,
                Action::Secondary => proxy.call("SecondaryActivate", &(x, y)).await?,
                Action::ContextMenu => proxy.call("ContextMenu", &(x, y)).await?,
                Action::Scroll(delta, horizontal) => {
                    proxy
                        .call(
                            "Scroll",
                            &(delta, if horizontal { "horizontal" } else { "vertical" }),
                        )
                        .await?
                }
                Action::MenuClick(_) => (),
            };
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        log::debug!("tray action failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_and_labels() {
        assert_eq!(
            parse_address(":1.42"),
            Some((":1.42".into(), "/StatusNotifierItem".into()))
        );
        assert_eq!(
            parse_address(":1.42/Item_2"),
            Some((":1.42".into(), "/Item_2".into()))
        );
        assert!(parse_address("/missing_sender").is_none());
        assert!(parse_address("not a bus name").is_none());
        assert_eq!(menu_label("_Open __file"), "Open _file");
    }
    #[test]
    fn menu_limits_and_visibility() {
        assert!(menu_node((0, HashMap::new(), vec![]), 9, &mut 10).is_none());
        assert!(menu_node((0, HashMap::new(), vec![]), 0, &mut 0).is_none());
        let props = HashMap::from([("visible".into(), OwnedValue::from(false))]);
        assert!(menu_node((0, props, vec![]), 0, &mut 10).is_none());
    }
}

#[cfg(test)]
#[path = "sni_tests.rs"]
mod integration_tests;
