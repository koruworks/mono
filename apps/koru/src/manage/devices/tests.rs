// SPDX-License-Identifier: MPL-2.0

//! The *Linked devices* page: what it lists and in what words, what a press
//! or a key there asks of Agent, the presence deadline, the history page,
//! what the inspector sees and what a screen reader is offered.
use super::history::device_event_label;
use super::*;
use koru_agent_wire::DeviceStateEventView;
use koru_core::ProfileSummary;
use koru_workspace_host::accessibility::{ViewSlot, WindowSlot};

fn device(id: &str) -> LinkedDeviceView {
    LinkedDeviceView {
        device_id: id.into(),
        display_name: id.into(),
        linked_at_millis: 0,
        connected: false,
        last_seen_millis: None,
        introduced_by: None,
        build_label: None,
        build_drift: None,
        build_severity: None,
        update_needed_on: None,
        connection: None,
        last_sync_millis: None,
        history: Vec::new(),
    }
}

fn relay(enabled: bool) -> RelayView {
    RelayView {
        enabled,
        state: "connected".into(),
        host: Some("dev1.relay.koru.do".into()),
    }
}

fn snapshot(
    this: Option<LinkedDeviceView>,
    linked: Vec<LinkedDeviceView>,
    relay: Option<RelayView>,
) -> LinkedDevices {
    let mut model = LinkedDevices::default();
    model.set_linked(this, linked, relay);
    model
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create")
}

/// A Manage tab with one Profile, sized `viewport`, showing *Linked devices*
/// over `model`. The requests showing it raised are drained.
fn devices_page(viewport: (f32, f32), model: &LinkedDevices) -> ManageView {
    let mut view = ManageView::new();
    let personal = ProfileSummary {
        id: ProfileId::new(),
        name: "Personal".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    };
    view.delegate(|c| {
        c.take_request();
        c.set_profiles(vec![personal.clone()], personal.id, &[])
    });
    View::set_viewport(&mut view, viewport.0, viewport.1);
    view.set_linked_devices(model);
    view.show(Page::Devices);
    requests(&mut view);
    view
}

fn requests(view: &mut ManageView) -> Vec<Request> {
    std::iter::from_fn(|| view.take_request()).collect()
}

fn device_requests(view: &mut ManageView) -> Vec<DeviceRequest> {
    requests(view)
        .into_iter()
        .filter_map(|request| match request {
            Request::Device(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// Where `control` is on screen: its laid-out rect, scrolled.
fn on_screen(view: &ManageView, control: Control) -> Rect {
    let rect = view
        .laid()
        .placed(control)
        .unwrap_or_else(|| panic!("{control:?} is on the page"))
        .rect;
    Rect::new(rect.x, rect.y - view.page_scroll, rect.w, rect.h)
}

fn press(view: &mut ManageView, control: Control) -> bool {
    let (x, y) = on_screen(view, control).centre();
    View::mouse_down(view, &mut doc(), x, y, false)
}

fn key(view: &mut ManageView, key: NamedKey) -> bool {
    View::key(
        view,
        &mut doc(),
        &Key::Named(key),
        None,
        ModifiersState::empty(),
    )
}

// ------------------------------------------------------------- the list

#[test]
fn the_page_lists_this_computer_the_relay_and_every_linked_device() {
    let mut phone = device("phone");
    phone.display_name = "Pixel".into();
    phone.build_label = Some("0.0.4 \u{b7} nightly \u{b7} Android 14".into());
    let mut laptop = device("laptop");
    laptop.introduced_by = Some("Pixel".into());
    let model = snapshot(Some(device("this")), vec![phone, laptop], Some(relay(true)));
    let view = devices_page((1000.0, 700.0), &model);
    assert_eq!(view.page_title(), "Linked devices");
    let laid = view.laid().devices.as_ref().expect("the devices page");
    assert!(laid.this_row.is_some() && laid.relay.is_some());
    assert_eq!(laid.rows.len(), 2);
    assert_eq!(laid.rows[0].badge_text, "AND");
    // Every row's controls, in order: Link a device…, this computer, the
    // relay, the phone and its Unlink…, the laptop — which arrived through
    // the phone and so has no Unlink of its own.
    let controls: Vec<Control> = view.laid().controls.iter().map(|p| p.control).collect();
    assert_eq!(
        controls,
        vec![
            Control::LinkDevice,
            Control::ThisDevice,
            Control::Relay,
            Control::DeviceRow(0),
            Control::UnlinkDevice(0),
            Control::DeviceRow(1),
        ]
    );
    // The nav counts the linked devices.
    assert_eq!(view.linked_count(), 2);
}

#[test]
fn link_a_device_opens_the_dialog_through_the_window() {
    let mut view = devices_page((1000.0, 700.0), &snapshot(None, Vec::new(), None));
    assert!(press(&mut view, Control::LinkDevice));
    assert_eq!(device_requests(&mut view), vec![DeviceRequest::LinkDevice]);
    // With nothing linked the page says so.
    let laid = view.laid().devices.as_ref().unwrap();
    assert!(laid.empty.is_some() && laid.list_card.is_none());
}

#[test]
fn a_new_snapshot_relays_out_and_an_identical_one_changes_nothing() {
    let model = snapshot(None, vec![device("phone")], None);
    let mut view = devices_page((1000.0, 700.0), &model);
    assert!(!view.set_linked_devices(&model));
    let more = snapshot(None, vec![device("phone"), device("tablet")], None);
    assert!(view.set_linked_devices(&more));
    assert_eq!(view.laid().devices.as_ref().unwrap().rows.len(), 2);
    assert_eq!(view.linked().devices().len(), 2);
}

// ------------------------------------------------------------- rename

#[test]
fn double_clicking_this_computer_renames_it() {
    let mut this = device("mine");
    this.display_name = "WIN10".into();
    let mut view = devices_page((1000.0, 700.0), &snapshot(Some(this), Vec::new(), None));
    let (x, y) = on_screen(&view, Control::ThisDevice).centre();

    // A single click selects nothing; the rename opens on the double.
    assert!(View::mouse_down(&mut view, &mut doc(), x, y, false));
    assert!(view.device_rename.is_none());
    assert!(View::mouse_double_click(&mut view, &doc(), x, y));
    assert!(view.device_rename.is_some());
    assert_eq!(
        view.laid()
            .devices
            .as_ref()
            .unwrap()
            .this_row
            .as_ref()
            .unwrap()
            .sub_text,
        RENAMING
    );

    // Escape cancels without leaving the page; Enter commits.
    assert!(key(&mut view, NamedKey::Escape));
    assert!(view.device_rename.is_none());
    assert_eq!(view.page(), Some(Page::Devices));
    assert!(device_requests(&mut view).is_empty());
    View::mouse_double_click(&mut view, &doc(), x, y);
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.device_rename.is_none());
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::RenameThisDevice("WIN10".into())]
    );

    // Clicking anywhere else commits, as a workspace rename does; a click
    // inside the field only moves the caret.
    View::mouse_double_click(&mut view, &doc(), x, y);
    let field = view.device_rename_rect().unwrap();
    let (fx, fy) = field.centre();
    assert!(View::mouse_down(&mut view, &mut doc(), fx, fy, false));
    assert!(view.device_rename.is_some());
    View::mouse_down(&mut view, &mut doc(), 900.0, 650.0, false);
    assert!(view.device_rename.is_none());
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::RenameThisDevice("WIN10".into())]
    );

    // The keyboard's way in is Enter on the row.
    view.page_focus = Some(Control::ThisDevice);
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.device_rename.is_some());
}

#[test]
fn this_computer_is_tagged_and_never_unlinkable() {
    let mut this = device("mine");
    this.build_label = Some("0.0.4 \u{b7} nightly \u{b7} Windows 11".into());
    let view = devices_page((1000.0, 700.0), &snapshot(Some(this), Vec::new(), None));
    let row = view
        .laid()
        .devices
        .as_ref()
        .unwrap()
        .this_row
        .clone()
        .unwrap();
    assert!(row.tag.is_some(), "the *This computer* tag");
    assert_eq!(row.badge_text, "WIN");
    assert!(
        !view
            .laid()
            .controls
            .iter()
            .any(|placed| matches!(placed.control, Control::UnlinkDevice(_)))
    );
}

// ------------------------------------------------------------- presence

#[test]
fn presence_is_refreshed_only_while_the_page_is_showing() {
    let model = snapshot(None, vec![device("phone")], None);
    let mut view = ManageView::new();
    View::set_viewport(&mut view, 1000.0, 700.0);
    view.set_linked_devices(&model);
    // Not on the page: no deadline and no wake.
    assert_eq!(view.devices_deadline(), None);

    view.show(Page::Devices);
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::Refresh],
        "showing the page asks for the list"
    );
    let deadline = view.devices_deadline().expect("armed while showing");
    assert!(View::maintenance_deadline(&view).is_some_and(|at| at <= deadline));
    // Before the deadline nothing is asked; at it, one refresh.
    View::maintain(
        &mut view,
        &mut doc(),
        deadline - std::time::Duration::from_millis(1),
    );
    assert!(device_requests(&mut view).is_empty());
    View::maintain(&mut view, &mut doc(), deadline);
    assert_eq!(device_requests(&mut view), vec![DeviceRequest::Refresh]);
    assert!(view.devices_deadline().is_some_and(|next| next > deadline));

    // Another page stops it.
    view.show(Page::Cloud);
    assert_eq!(view.devices_deadline(), None);
    assert_eq!(view.linked().presence_deadline(), None);
    View::maintain(&mut view, &mut doc(), deadline + PRESENCE);
    assert!(device_requests(&mut view).is_empty());
}

const PRESENCE: std::time::Duration = crate::mainview::linked_devices::PRESENCE_REFRESH;

// ------------------------------------------------------------- relay

#[test]
fn the_relay_switch_sets_an_exact_state_and_names_the_host() {
    let model = snapshot(None, Vec::new(), Some(relay(true)));
    let mut view = devices_page((1000.0, 760.0), &model);
    let laid = view.laid().devices.clone().unwrap();
    assert_eq!(
        laid.relay.as_ref().unwrap().status_text,
        "dev1.relay.koru.do \u{2014} connected"
    );
    assert!(press(&mut view, Control::Relay));
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::SetRelay(false)]
    );
    assert_eq!(
        view.laid()
            .devices
            .as_ref()
            .unwrap()
            .relay
            .as_ref()
            .unwrap()
            .status_text,
        "Relay off"
    );
    assert!(press(&mut view, Control::Relay));
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::SetRelay(true)]
    );
    assert_eq!(
        relay_status_label(&RelayView {
            enabled: true,
            state: "connecting".into(),
            host: None,
        }),
        "connecting"
    );
}

// ------------------------------------------------------------- unlink

#[test]
fn unlink_asks_first_and_then_unlinks_that_device() {
    let model = snapshot(None, vec![device("phone"), device("tablet")], None);
    let mut view = devices_page((1000.0, 700.0), &model);
    assert!(press(&mut view, Control::UnlinkDevice(1)));
    assert_eq!(view.dialog().map(|d| d.title()), Some("Unlink tablet?"));
    assert!(
        device_requests(&mut view).is_empty(),
        "nothing before the answer"
    );
    // Enter is the safe answer.
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.dialog().is_none());
    assert!(device_requests(&mut view).is_empty());
    press(&mut view, Control::UnlinkDevice(1));
    // Tab moves to the red button, and Enter then answers it.
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.dialog().is_none());
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::Unlink("tablet".into())]
    );
}

// ------------------------------------------------------------- scrolling

#[test]
fn every_linked_device_is_reachable_when_the_list_is_taller_than_the_page() {
    let devices: Vec<_> = (0..24).map(|i| device(&format!("peer-{i}"))).collect();
    let model = snapshot(Some(device("this")), devices, Some(relay(true)));
    let mut view = devices_page((900.0, 500.0), &model);
    assert!(view.page_content_height() > 500.0, "the list overflows");
    // No paging line: the list scrolls with the page, under the pointer.
    View::mouse_move(&mut view, 600.0, 250.0);
    View::scroll_by(
        &mut view,
        winit::event::MouseScrollDelta::LineDelta(0.0, -1000.0),
    );
    let last = on_screen(&view, Control::DeviceRow(23));
    assert!(
        last.y + last.h <= 500.0 + 0.5,
        "the last row scrolled into view"
    );
    assert!(press(&mut view, Control::DeviceRow(23)));
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::LoadHistory {
            device_id: "peer-23".into(),
            before: None
        }]
    );
}

// ------------------------------------------------------------- history

fn document_event(id: i64, document: DocumentId) -> DeviceStateEventView {
    DeviceStateEventView {
        event_id: Some(id),
        at_millis: id,
        kind: "document_updated".into(),
        profile_id: None,
        workspace_id: Some("workspace".into()),
        document_id: Some(document.to_storage()),
        document_name: Some("Roadmap".into()),
        blob_hash: None,
        detail: None,
        items: Some(3),
    }
}

#[test]
fn a_device_row_opens_its_history_which_pages_older_and_opens_documents() {
    let mut phone = device("phone");
    phone.display_name = "Phone".into();
    phone.connected = true;
    phone.last_sync_millis = Some(42);
    let model = snapshot(None, vec![phone], None);
    let mut view = devices_page((1000.0, 700.0), &model);
    assert!(press(&mut view, Control::DeviceRow(0)));
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::LoadHistory {
            device_id: "phone".into(),
            before: None
        }]
    );
    // The page is the history now: its heading is the device.
    assert_eq!(view.page_title(), "Phone");
    let history = view
        .laid()
        .devices
        .as_ref()
        .unwrap()
        .history
        .clone()
        .unwrap();
    assert_eq!(history.back_text, "\u{2039} Phone");
    assert_eq!(
        history.message.as_ref().map(|(_, text)| text.as_str()),
        Some(LOADING_TEXT)
    );

    let document = DocumentId::new();
    let page: Vec<_> = (2..10).rev().map(|i| document_event(i, document)).collect();
    assert!(view.set_device_history("phone", page, Some(2)));
    let history = view
        .laid()
        .devices
        .as_ref()
        .unwrap()
        .history
        .clone()
        .unwrap();
    assert_eq!(history.events.len(), 8);
    assert_eq!(
        history.events[0].label,
        "Updated \u{201c}Roadmap\u{201d} \u{b7} 3 ops"
    );
    // A page for another device is stale and changes nothing.
    assert!(!view.set_device_history("tablet", Vec::new(), None));

    // Load older asks once for the next page, by its cursor.
    assert!(press(&mut view, Control::OlderHistory));
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::LoadHistory {
            device_id: "phone".into(),
            before: Some(2)
        }]
    );
    assert!(view.laid().placed(Control::OlderHistory).is_none());
    view.set_device_history("phone", vec![document_event(1, document)], None);
    assert_eq!(
        view.laid()
            .devices
            .as_ref()
            .unwrap()
            .history
            .as_ref()
            .unwrap()
            .events
            .len(),
        9
    );

    // A document event opens the document.
    assert!(press(&mut view, Control::HistoryEvent(0)));
    assert_eq!(
        device_requests(&mut view),
        vec![DeviceRequest::OpenDocument(document)]
    );

    // The back row returns to the list; so does Escape.
    assert!(press(&mut view, Control::HistoryBack));
    assert!(!view.linked().history_open());
    assert_eq!(view.page_title(), "Linked devices");
    press(&mut view, Control::DeviceRow(0));
    assert!(view.linked().history_open());
    assert!(key(&mut view, NamedKey::Escape));
    assert!(!view.linked().history_open());
}

const LOADING_TEXT: &str = "Loading device history\u{2026}";

#[test]
fn event_labels_keep_todays_words() {
    let catalog = DeviceStateEventView {
        event_id: Some(9),
        at_millis: 0,
        kind: "catalog_reconciled".into(),
        profile_id: None,
        workspace_id: None,
        document_id: None,
        document_name: None,
        blob_hash: None,
        detail: Some("1 profile change, 2 document changes".into()),
        items: Some(3),
    };
    assert_eq!(
        device_event_label(&catalog),
        "Reconciled 1 profile change, 2 document changes"
    );
    let blob = DeviceStateEventView {
        kind: "blob_stored".into(),
        detail: None,
        blob_hash: Some("abcdef0123456789".into()),
        ..catalog
    };
    assert_eq!(
        device_event_label(&blob),
        "File downloaded \u{b7} abcdef012345"
    );
}

// ------------------------------------------------------------- words

#[test]
fn linked_devices_distinguish_relationship_from_presence() {
    let mut device = device("phone");
    device.display_name = "Android".into();
    device.connected = true;
    device.last_seen_millis = Some(0);
    assert_eq!(
        device_status_label(&device),
        "Linked (online) \u{b7} no state changes received yet"
    );
    device.connection = Some("relay".into());
    assert_eq!(
        device_status_label(&device),
        "Linked (online, relay) \u{b7} no state changes received yet"
    );
    device.connected = false;
    assert!(device_status_label(&device).starts_with("Linked (offline) \u{b7} last seen "));
    device.last_seen_millis = None;
    assert!(device_status_label(&device).starts_with("Linked (offline) \u{b7} linked "));
    device.introduced_by = Some("Desktop".into());
    assert!(device_status_label(&device).contains("\u{b7} via Desktop"));
    // The build comes last, after everything about the relationship.
    device.build_label = Some("0.0.4 \u{b7} nightly \u{b7} Android 14".into());
    assert!(
        device_status_label(&device)
            .contains("\u{b7} via Desktop \u{b7} 0.0.4 \u{b7} nightly \u{b7} Android 14")
    );
    // This computer's own line leads with the build and ends with the
    // gesture; *This computer* is its tag.
    assert_eq!(
        this_device_hint(&device),
        "0.0.4 \u{b7} nightly \u{b7} Android 14 \u{b7} double-click to rename"
    );
    device.build_label = None;
    assert_eq!(this_device_hint(&device), "Double-click to rename");

    // Two devices on different protocol numbers do not sync: that leads
    // the line and names the side the update is on.
    device.update_needed_on = Some("peer".into());
    assert!(
        device_status_label(&device).starts_with("Not syncing \u{b7} update Android \u{b7} Linked")
    );
    device.update_needed_on = Some("this_device".into());
    assert!(
        device_status_label(&device)
            .starts_with("Not syncing \u{b7} update this device \u{b7} Linked")
    );
}

#[test]
fn installations_are_told_apart_by_their_platform_badge() {
    let mut windows = device("a");
    windows.build_label = Some("0.0.4 \u{b7} nightly \u{b7} main \u{b7} Windows 11".into());
    let mut mac = device("b");
    mac.build_label = Some("0.0.4 \u{b7} stable \u{b7} macOS 15".into());
    let mut iphone = device("c");
    iphone.build_label = Some("0.1.0 (1) \u{b7} nightly \u{b7} iOS 18".into());
    let mut unknown = device("d");
    unknown.display_name = "studio pc".into();
    assert_eq!(platform_badge(&windows), "WIN");
    assert_eq!(platform_badge(&mac), "MAC");
    assert_eq!(platform_badge(&iphone), "IOS");
    // A device that never said what it runs gets its name's initial.
    assert_eq!(platform_badge(&unknown), "S");
}

#[test]
fn long_statuses_wrap_inside_their_row() {
    let mut phone = device("phone");
    phone.introduced_by = Some("A device with a deliberately long name".into());
    phone.build_label = Some(
        "0.0.3-nightly.20260821+5c47d45 \u{b7} nightly \u{b7} a-very-long-platform-label".into(),
    );
    phone.build_severity = Some("danger".into());
    phone.update_needed_on = Some("peer".into());
    phone.last_seen_millis = Some(0);
    let mut view = devices_page((700.0, 700.0), &snapshot(None, vec![phone], None));
    let row = view.laid().devices.as_ref().unwrap().rows[0].clone();
    let layout = view
        .engine
        .simple_layout_wrapped(&row.sub_text, theme::role::meta(), row.sub.w);
    assert!(layout.height() > theme::role::meta().metrics.line_height);
    assert!(layout.width() <= row.sub.w + 0.01);
    assert!(row.sub.x + row.sub.w <= row.rect.x + row.rect.w - CardRow::PAD_X);
    assert!(
        row.sub.y + row.sub.h <= row.rect.y + row.rect.h,
        "the row grew to hold it"
    );
}

#[test]
fn drift_severity_picks_the_notice_tone_then_the_destructive_red() {
    let p = theme::p();
    assert_eq!(status_color(None, p), p.text_dim);
    assert_eq!(status_color(Some("warning"), p), p.notice_fg);
    assert_eq!(status_color(Some("danger"), p), p.close_hover);
    // An unknown word is not an excuse to shout.
    assert_eq!(status_color(Some("purple"), p), p.text_dim);
}

// ------------------------------------------------------------- inspection

fn capture(view: &mut ManageView) -> inspect::Snapshot {
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, w, h));
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    frame.finish()
}

#[test]
fn the_inspector_names_the_page_its_rows_the_relay_and_the_history() {
    let model = snapshot(
        Some(device("this")),
        vec![device("phone"), device("tablet")],
        Some(relay(true)),
    );
    let mut view = devices_page((1000.0, 700.0), &model);
    let named = |snapshot: &inspect::Snapshot, name: &str| {
        snapshot.records.iter().filter(|r| r.name == name).count()
    };
    let snapshot = capture(&mut view);
    assert_eq!(named(&snapshot, "Manage linked devices"), 1);
    assert_eq!(named(&snapshot, "Manage device row"), 3);
    assert_eq!(named(&snapshot, "Manage relay"), 1);
    assert!(named(&snapshot, "KoruToggle") >= 1, "the shared switch");
    assert!(named(&snapshot, "KoruCard") >= 2, "the shared cards");
    press(&mut view, Control::DeviceRow(0));
    let snapshot = capture(&mut view);
    assert_eq!(named(&snapshot, "Manage device history"), 1);
    assert_eq!(named(&snapshot, "Manage device row"), 0);
}

// ------------------------------------------------------------- accessibility

fn context() -> accessibility::Context {
    accessibility::Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(2),
        area: Rect::new(40.0, 60.0, 1000.0, 700.0),
    }
}

fn find<'a>(node: &'a SemanticNodeAlias, role: accesskit::Role) -> Vec<&'a SemanticNodeAlias> {
    let mut found = Vec::new();
    if node.role == role {
        found.push(node);
    }
    for child in &node.children {
        found.extend(find(child, role));
    }
    found
}

type SemanticNodeAlias = koru_workspace_host::accessibility::SemanticNode;

#[test]
fn a_screen_reader_hears_the_devices_the_relay_and_the_back_row() {
    let mut transitive = device("laptop");
    transitive.introduced_by = Some("phone".into());
    let model = snapshot(
        Some(device("this")),
        vec![device("phone"), transitive],
        Some(relay(true)),
    );
    let mut view = devices_page((1000.0, 700.0), &model);
    let cx = context();
    let tree = view.semantics(&cx);
    let switch = find(&tree, accesskit::Role::Switch);
    assert_eq!(switch.len(), 1);
    assert_eq!(switch[0].label.as_deref(), Some("Use relay"));
    assert_eq!(switch[0].toggled, Some(accesskit::Toggled::True));
    let lists = find(&tree, accesskit::Role::ListBox);
    let devices = lists
        .iter()
        .find(|list| list.label.as_deref() == Some("Linked devices") && list.size_of_set == Some(2))
        .expect("the devices list");
    assert!(
        devices.children[0]
            .label
            .as_deref()
            .is_some_and(|label| label.starts_with("phone, Linked (offline)"))
    );
    // The direct device's Unlink… is its child; the transitive one has none.
    // The button reads *Unlink…* alone (the review's note); its spoken name
    // still says which device.
    assert_eq!(devices.children[0].children.len(), 1);
    assert_eq!(
        devices.children[0].children[0].label.as_deref(),
        Some("Unlink phone\u{2026}")
    );
    assert_eq!(
        view.laid()
            .placed(Control::UnlinkDevice(0))
            .map(|placed| placed.label.as_str()),
        Some("Unlink\u{2026}")
    );
    assert!(devices.children[1].children.is_empty());
    let buttons = find(&tree, accesskit::Role::Button);
    assert!(
        buttons
            .iter()
            .any(|b| b.label.as_deref() == Some("Link a device\u{2026}"))
    );
    assert!(buttons.iter().any(|b| {
        b.label
            .as_deref()
            .is_some_and(|l| l.starts_with("this, this computer"))
    }));

    // Pressing a device's option is the click: its history, with a back row.
    let phone = devices.children[0].occurrence;
    assert!(view.accessibility_action(&cx, phone, accesskit::Action::Click));
    assert!(view.linked().history_open());
    let tree = view.semantics(&cx);
    let back = find(&tree, accesskit::Role::Button)
        .into_iter()
        .find(|b| b.label.as_deref() == Some("Back to linked devices from phone"))
        .expect("the back row")
        .occurrence;
    assert_eq!(
        tree.children[1].label.as_deref(),
        Some("phone"),
        "the heading"
    );
    assert!(view.accessibility_action(&cx, back, accesskit::Action::Click));
    assert!(!view.linked().history_open());
}
