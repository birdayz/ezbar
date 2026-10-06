//! Run explicitly under a private bus, never against the user's tray watcher:
//! EZBAR_TRAY_TEST_BUS=1 dbus-run-session -- cargo test --locked isolated_bus_protocols -- --ignored
use super::*;

struct TestItem(Arc<Mutex<Vec<String>>>);
#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl TestItem {
    #[zbus(property)]
    fn title(&self) -> &str {
        "Test tray icon"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        vec![(1, 1, vec![255, 12, 34, 56])]
    }
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::ObjectPath<'_> {
        "/Menu".try_into().unwrap()
    }
    fn activate(&self, x: i32, y: i32) {
        self.0.lock().unwrap().push(format!("activate {x} {y}"));
    }
    fn secondary_activate(&self, x: i32, y: i32) {
        self.0.lock().unwrap().push(format!("secondary {x} {y}"));
    }
    fn context_menu(&self, x: i32, y: i32) {
        self.0.lock().unwrap().push(format!("context {x} {y}"));
    }
    fn scroll(&self, delta: i32, orientation: &str) {
        self.0
            .lock()
            .unwrap()
            .push(format!("scroll {delta} {orientation}"));
    }
}
struct TestMenu(Arc<Mutex<Vec<String>>>);
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl TestMenu {
    fn about_to_show(&self, id: i32) -> bool {
        self.0.lock().unwrap().push(format!("about {id}"));
        false
    }
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> (u32, Layout) {
        assert_eq!(
            (parent_id, recursion_depth, property_names.len()),
            (0, 8, 0)
        );
        let props = HashMap::from([
            (
                "label".into(),
                OwnedValue::try_from(Value::from("_Open")).unwrap(),
            ),
            ("enabled".into(), OwnedValue::from(true)),
            (
                "toggle-type".into(),
                OwnedValue::try_from(Value::from("checkmark")).unwrap(),
            ),
            ("toggle-state".into(), OwnedValue::from(1i32)),
        ]);
        let child: Layout = (42, props, vec![]);
        (
            1,
            (
                0,
                HashMap::new(),
                vec![OwnedValue::try_from(Value::from(child)).unwrap()],
            ),
        )
    }
    fn event(&self, id: i32, event_id: &str, data: Value<'_>, timestamp: u32) {
        assert_eq!(i32::try_from(data).unwrap(), 0);
        assert_eq!(timestamp, 0);
        self.0
            .lock()
            .unwrap()
            .push(format!("event {id} {event_id}"));
    }
}

#[test]
#[ignore = "requires a private dbus-run-session and EZBAR_TRAY_TEST_BUS=1"]
fn isolated_bus_protocols() {
    assert_eq!(std::env::var("EZBAR_TRAY_TEST_BUS").as_deref(), Ok("1"));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(20), async {
            let state = Arc::new(Mutex::new(State::default()));
            let watcher = zbus::connection::Builder::session()
                .unwrap()
                .name(WATCHER)
                .unwrap()
                .serve_at(WATCH_PATH, Watcher(state.clone()))
                .unwrap()
                .build()
                .await
                .unwrap();
            let original_owner = watcher.unique_name().unwrap().to_string();
            let events = Arc::new(Mutex::new(Vec::new()));
            let app = zbus::connection::Builder::session()
                .unwrap()
                .name("org.ezbar.TrayTest")
                .unwrap()
                .serve_at("/StatusNotifierItem", TestItem(events.clone()))
                .unwrap()
                .serve_at("/Menu", TestMenu(events.clone()))
                .unwrap()
                .build()
                .await
                .unwrap();
            let observer = Connection::session().await.unwrap();
            let app_name = app.unique_name().unwrap().to_string();
            let proxy = Proxy::new(&app, WATCHER, WATCH_PATH, WATCHER)
                .await
                .unwrap();
            let _: () = proxy
                .call("RegisterStatusNotifierItem", &("org.ezbar.TrayTest",))
                .await
                .unwrap();
            let _: () = proxy
                .call("RegisterStatusNotifierItem", &("/StatusNotifierItem",))
                .await
                .unwrap();
            let names: Vec<String> = proxy
                .get_property("RegisteredStatusNotifierItems")
                .await
                .unwrap();
            assert_eq!(names, vec![format!("{app_name}/StatusNotifierItem")]);
            let spoof: zbus::Result<()> = proxy
                .call("RegisterStatusNotifierItem", &(original_owner.as_str(),))
                .await;
            assert!(spoof.is_err());
            let invalid: zbus::Result<()> = proxy
                .call("RegisterStatusNotifierItem", &("/invalid path",))
                .await;
            assert!(invalid.is_err());
            let spoof: zbus::Result<()> = proxy
                .call("RegisterStatusNotifierHost", &(original_owner.as_str(),))
                .await;
            assert!(spoof.is_err());

            let (tx, rx) = mpsc::channel(64);
            let (updates, mut snapshots) = watch::channel(Snapshot::default());
            let host = tokio::spawn(run(rx, updates));
            let item = loop {
                snapshots.changed().await.unwrap();
                if let Some(item) = snapshots.borrow_and_update().sni.first().cloned() {
                    break item;
                }
            };
            assert_eq!(
                owner(&observer, WATCHER).await.unwrap(),
                original_owner,
                "must not steal watcher"
            );
            assert!(!state.lock().unwrap().hosts.is_empty());
            assert_eq!(item.title, "Test tray icon");
            assert!(
                matches!(item.icon, Some(icon::Icon::Pixels(ref p)) if *p.rgba == [12,34,56,255])
            );
            for action in [
                Action::Activate,
                Action::Secondary,
                Action::ContextMenu,
                Action::Scroll(120, true),
                Action::MenuClick(42),
            ] {
                tx.send(Command {
                    id: item.id.clone(),
                    action,
                    position: (-12, 34),
                    menu: item.menu.clone(),
                    reply: None,
                })
                .await
                .unwrap();
            }
            let (reply, nodes) = tokio::sync::oneshot::channel();
            tx.send(Command {
                id: item.id.clone(),
                action: Action::ContextMenu,
                position: (0, 0),
                menu: item.menu.clone(),
                reply: Some(reply),
            })
            .await
            .unwrap();
            let nodes = nodes.await.unwrap();
            assert_eq!(nodes.len(), 1);
            assert_eq!(
                (nodes[0].id, nodes[0].label.as_str(), nodes[0].checked),
                (42, "Open", Some(true))
            );
            loop {
                if events.lock().unwrap().len() >= 6 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            for expected in [
                "activate -12 34",
                "secondary -12 34",
                "context -12 34",
                "scroll 120 horizontal",
                "event 42 clicked",
                "about 0",
            ] {
                assert!(
                    events.lock().unwrap().iter().any(|event| event == expected),
                    "missing {expected}"
                );
            }
            // A disconnected client is unregistered; other hosts remain intact.
            drop(proxy);
            app.close().await.unwrap();
            prune(&watcher, &state).await;
            assert!(state.lock().unwrap().items.is_empty());
            assert!(!state.lock().unwrap().hosts.is_empty());
            // Watcher loss: running host takes ownership rather than disappearing.
            watcher.close().await.unwrap();
            loop {
                if owner(&observer, WATCHER)
                    .await
                    .is_ok_and(|name| name != original_owner)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let own = Proxy::new(&observer, WATCHER, WATCH_PATH, WATCHER)
                .await
                .unwrap();
            let _: () = own
                .call("RegisterStatusNotifierItem", &("/StatusNotifierItem",))
                .await
                .unwrap();
            let names: Vec<String> = own
                .get_property("RegisteredStatusNotifierItems")
                .await
                .unwrap();
            assert_eq!(names.len(), 1);
            drop(own);
            drop(tx);
            host.await.unwrap();
            assert!(
                owner(&observer, WATCHER).await.is_err(),
                "host must release watcher on shutdown"
            );
        })
        .await
        .expect("private bus test timed out");
    });
}
