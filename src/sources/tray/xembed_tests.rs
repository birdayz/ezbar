//! Requires a disposable X server; never run on the user's DISPLAY.
use super::*;

#[test]
#[ignore = "requires private X server and EZBAR_TRAY_TEST_X11=1"]
fn isolated_xembed_protocols() {
    assert_eq!(std::env::var("EZBAR_TRAY_TEST_X11").as_deref(), Ok("1"));
    let _ = env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .try_init();
    let mut host = Host::open().unwrap();
    assert!(
        Host::open().is_err(),
        "must not steal another tray selection"
    );
    let (app, screen) = x11rb::connect(None).unwrap();
    let root = app.setup().roots[screen].root;
    let window = app.generate_id().unwrap();
    app.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        SIZE,
        SIZE,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0xff0000)
            .event_mask(
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::STRUCTURE_NOTIFY,
            ),
    )
    .unwrap()
    .check()
    .unwrap();
    host.embed(window).unwrap();
    assert!(host.icons.is_empty(), "reject non-XEmbed windows");
    app.change_property32(
        PropMode::REPLACE,
        window,
        host.atoms.info,
        host.atoms.info,
        &[0, 1],
    )
    .unwrap();
    app.change_property8(
        PropMode::REPLACE,
        window,
        host.atoms.name,
        host.atoms.utf8,
        b"Test X11 icon",
    )
    .unwrap();
    app.send_event(
        false,
        host.manager,
        EventMask::NO_EVENT,
        ClientMessageEvent::new(
            32,
            host.manager,
            host.atoms.opcode,
            [CURRENT_TIME, 0, window, 0, 0],
        ),
    )
    .unwrap();
    app.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !host.icons.contains_key(&window) {
        assert!(Instant::now() < deadline, "dock timed out");
        host.events().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(host.icons[&window].item.title, "Test X11 icon");
    assert_eq!(
        app.query_tree(window).unwrap().reply().unwrap().parent,
        host.icons[&window].container
    );
    app.clear_area(false, window, 0, 0, SIZE, SIZE)
        .unwrap()
        .check()
        .unwrap();
    let icon::Icon::Pixels(pixels) = host.capture(window).unwrap().unwrap();
    assert_eq!(&pixels.rgba[..4], &[255, 0, 0, 255]);
    for (action, button) in [
        (Action::Activate, 1),
        (Action::Secondary, 2),
        (Action::ContextMenu, 3),
        (Action::Scroll(120, false), 4),
    ] {
        host.action(Command {
            id: ItemId::Xembed(window),
            action,
            position: (200, 100),
            menu: None,
            reply: None,
        })
        .unwrap();
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while received.len() < 2 {
            assert!(Instant::now() < deadline, "button event timed out");
            match app.poll_for_event().unwrap() {
                Some(Event::ButtonPress(e)) | Some(Event::ButtonRelease(e)) => {
                    assert_eq!((e.detail, e.root_x, e.root_y), (button, 200, 100));
                    received.push(e.response_type & 0x7f);
                }
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(received, [BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT]);
        let pos = app
            .translate_coordinates(window, root, 12, 12)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!((pos.dst_x, pos.dst_y), (200, 100));
    }
    // Client reparents elsewhere: removal must not yank it back to root.
    // Use an unmanaged parent so the test WM cannot reposition it mid-assertion.
    let elsewhere = app.generate_id().unwrap();
    app.create_window(
        COPY_DEPTH_FROM_PARENT,
        elsewhere,
        root,
        0,
        0,
        50,
        50,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().override_redirect(1),
    )
    .unwrap()
    .check()
    .unwrap();
    app.reparent_window(window, elsewhere, 5, 6)
        .unwrap()
        .check()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while host.icons.contains_key(&window) {
        assert!(Instant::now() < deadline);
        host.events().unwrap();
    }
    let geometry = app.get_geometry(window).unwrap().reply().unwrap();
    assert_eq!((geometry.x, geometry.y), (5, 6));
    host.embed(window).unwrap();
    let selection = host.atoms.selection;
    drop(host);
    assert_eq!(
        app.get_selection_owner(selection)
            .unwrap()
            .reply()
            .unwrap()
            .owner,
        NONE
    );
    assert_eq!(
        app.query_tree(window).unwrap().reply().unwrap().parent,
        root,
        "client survives host shutdown"
    );
    let host = Host::open().unwrap();
    assert_eq!(
        app.get_selection_owner(selection)
            .unwrap()
            .reply()
            .unwrap()
            .owner,
        host.manager
    );
}
