// SPDX-License-Identifier: MPL-2.0

//! The window's life, driven against a recorded provider.
//!
//! The real adapter needs an event loop, a window and a screen reader; the
//! [`Provider`] seam is what lets the rules above it — when a tree is worth
//! building, and which requests are still about something that exists — be
//! asserted in `cargo test --workspace` instead of by hand on a desktop.

use std::sync::{Arc, Mutex};

use accesskit::{Action, ActionRequest, NodeId, TreeId, TreeUpdate};
use koru_draw::geometry::Rect;
use koru_window::{AccessibilityPending as Pending, AccessibilityProvider as Provider};
use uuid::Uuid;

use super::{PaneAccessibility, Stage};
use crate::accessibility::CaptionGeometry;
use crate::accessibility::identity::{ControlPart, Occurrence, ViewSlot, WindowSlot, part};
use crate::accessibility::projection::{Scope, SemanticNode, WindowSemantics};
use crate::accessibility::window_commands;

/// What a screen reader has done, and whether one is attached at all.
#[derive(Default)]
struct Client {
    pending: Pending,
    /// The adapter only calls the build closure once a client has asked; this
    /// is that state, and it is what makes "nothing is built while nobody is
    /// listening" testable rather than asserted in a comment.
    attached: bool,
    published: Vec<TreeUpdate>,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Client>>);

impl Recorder {
    fn attach(&self) {
        let mut client = self.0.lock().expect("no test poisons this");
        client.attached = true;
        client.pending.initial_tree = true;
    }

    fn detach(&self) {
        let mut client = self.0.lock().expect("no test poisons this");
        client.attached = false;
        client.pending.deactivated = true;
    }

    fn ask(&self, node: NodeId, action: Action) {
        self.0
            .lock()
            .expect("no test poisons this")
            .pending
            .actions
            .push(ActionRequest {
                action,
                target_tree: TreeId::ROOT,
                target_node: node,
                data: None,
            });
    }

    fn publishes(&self) -> usize {
        self.0.lock().expect("no test poisons this").published.len()
    }
}

impl Provider for Recorder {
    fn process_event(
        &mut self,
        _window: &winit::window::Window,
        _event: &winit::event::WindowEvent,
    ) {
    }

    fn take_pending(&mut self) -> Pending {
        std::mem::take(&mut self.0.lock().expect("no test poisons this").pending)
    }

    fn publish(&mut self, build: &mut dyn FnMut() -> TreeUpdate) -> bool {
        let attached = self.0.lock().expect("no test poisons this").attached;
        if !attached {
            return false;
        }
        let update = build();
        self.0
            .lock()
            .expect("no test poisons this")
            .published
            .push(update);
        true
    }
}

fn window() -> WindowSlot {
    WindowSlot::from_raw(1)
}

fn pane(recorder: &Recorder) -> PaneAccessibility {
    PaneAccessibility::new(
        window(),
        ViewSlot::from_raw(1),
        Some(Box::new(recorder.clone())),
    )
}

fn snapshot(slot: WindowSlot, children: Vec<SemanticNode>) -> WindowSemantics {
    WindowSemantics {
        window: slot,
        label: "Notes".to_string(),
        scale: 1.0,
        client: Rect::new(0.0, 0.0, 1200.0, 800.0),
        focus: None,
        children,
        scope: Scope::Whole,
        retired: Vec::new(),
    }
}

fn commands(slot: WindowSlot) -> Vec<SemanticNode> {
    window_commands(slot, false, koru_i18n::Lang::En, CaptionGeometry::default())
}

/// One pass of the loop's drain: poll, publish if owed, resolve.
fn service(access: &mut PaneAccessibility, children: Vec<SemanticNode>) -> Vec<super::Resolved> {
    let slot = access.slot();
    if access.poll().snapshot {
        access.publish(&snapshot(slot, children));
    }
    access.resolve()
}

// ------------------------------------------------------------ the two gates

#[test]
fn overlay_structure_change_republishes_root_only_for_an_active_client() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    access.invalidate_structure();
    assert!(!access.poll().snapshot);
    recorder.attach();
    service(&mut access, commands(window()));
    assert!(!access.poll().snapshot);
    access.invalidate_structure();
    let poll = access.poll();
    assert!(poll.snapshot && poll.full);
    service(&mut access, commands(window()));
    assert_eq!(recorder.publishes(), 2);
    assert!(!access.poll().snapshot);
}

#[test]
fn no_screen_reader_means_no_tree_is_ever_built() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    for _ in 0..100 {
        // Everything an ordinary session does: the window changes, frames go
        // by, the loop drains.
        access.invalidate();
        assert!(service(&mut access, commands(window())).is_empty());
    }
    assert_eq!(access.counters().rebuilds, 0);
    assert_eq!(recorder.publishes(), 0);
}

#[test]
fn a_query_or_a_caret_blink_does_not_rebuild_the_tree() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().rebuilds, 1);

    // A hundred idle passes: no invalidation, so nothing changed and nothing
    // is rebuilt however often a client inspects the tree it already has.
    for _ in 0..100 {
        service(&mut access, commands(window()));
    }
    assert_eq!(access.counters().rebuilds, 1);

    // One real change earns exactly one rebuild.
    access.invalidate();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().rebuilds, 2);

    // Two changes between drains are coalesced into one.
    access.invalidate();
    access.invalidate();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().rebuilds, 3);
}

#[test]
fn a_view_revision_that_has_not_moved_rebuilds_nothing() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();

    // The loop's pass: ask the view for its revision, then drain. A view whose
    // answer is the same number — a scroll, a caret blink, a frame — must not
    // cost a tree.
    let mut revision = 7;
    access.observe(revision);
    service(&mut access, commands(window()));
    assert_eq!(access.counters().rebuilds, 1);

    for _ in 0..100 {
        access.observe(revision);
        service(&mut access, commands(window()));
    }
    assert_eq!(
        access.counters().rebuilds,
        1,
        "an unchanged view revision rebuilt the tree"
    );

    // …and one that has moved earns exactly one.
    revision += 1;
    access.observe(revision);
    service(&mut access, commands(window()));
    assert_eq!(access.counters().rebuilds, 2);

    // Two changes between drains coalesce, the way two `invalidate`s do.
    access.observe(revision + 1);
    access.observe(revision + 2);
    service(&mut access, commands(window()));
    assert_eq!(access.counters().rebuilds, 3);
}

#[test]
fn nobody_is_listening_until_a_client_asks() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    // The gate in front of `observe`: an ordinary session never even asks a
    // view what its revision is, which is what makes a fingerprint over the
    // rows affordable in the sessions that do.
    assert!(!access.listening());
    for _ in 0..10 {
        service(&mut access, commands(window()));
        assert!(!access.listening());
    }
    recorder.attach();
    service(&mut access, commands(window()));
    assert!(access.listening());
    access.retire();
    assert!(!access.listening(), "a retired window is still listening");
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn a_window_starts_hidden_with_a_provider_and_is_retired_when_it_goes() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    assert_eq!(access.stage(), Stage::Hidden);
    assert!(access.has_provider());

    access.revealed();
    assert_eq!(access.stage(), Stage::Revealed);
    // A reveal that happens again — a restore from the tray, a re-show from
    // search — is not a second first reveal.
    access.revealed();
    assert_eq!(access.stage(), Stage::Revealed);

    access.retire();
    assert_eq!(access.stage(), Stage::Retired);
    assert!(!access.has_provider());
}

#[test]
fn a_window_with_no_provider_still_works_and_never_asks_for_a_snapshot() {
    let mut access = PaneAccessibility::new(window(), ViewSlot::from_raw(1), None);
    access.revealed();
    access.invalidate();
    assert!(!access.poll().snapshot);
    assert!(!access.publish(&snapshot(window(), commands(window()))));
    assert!(access.resolve().is_empty());
    assert_eq!(access.counters(), Default::default());
}

#[test]
fn late_activation_publishes_a_coherent_full_snapshot_of_what_is_on_screen_now() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();

    // The window lives for a while with no screen reader, changing as it goes.
    for _ in 0..5 {
        access.invalidate();
        service(&mut access, commands(window()));
    }
    assert_eq!(access.counters().rebuilds, 0);

    // Now one attaches. What it is handed is built at this moment, from the
    // children it is given now — not a stale tree kept from before.
    recorder.attach();
    let now = vec![
        SemanticNode::new(
            Occurrence::model(
                window(),
                ViewSlot::from_raw(1),
                Uuid::from_u128(9),
                ControlPart::ROOT,
            ),
            accesskit::Role::ListItem,
        )
        .with_label("opened after the reader attached"),
    ];
    service(&mut access, now);
    assert_eq!(access.counters().rebuilds, 1);
    assert_eq!(access.counters().activations, 1);

    let published = &recorder.0.lock().expect("no test poisons this").published[0];
    // A full tree: the root, its tree data, and the child that exists now.
    assert!(published.tree.is_some());
    assert_eq!(published.nodes.len(), 2);
}

#[test]
fn a_reader_detaching_forgets_the_tree_so_the_next_attach_is_a_full_snapshot() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));
    let close = Occurrence::window_command(window(), part::CLOSE);

    recorder.detach();
    service(&mut access, commands(window()));
    // Nothing is published to a client that has gone, and the tree that was on
    // screen is forgotten — so an action replayed against it is refused.
    recorder.ask(close.node_id(), Action::Click);
    assert!(service(&mut access, commands(window())).is_empty());
    assert_eq!(access.counters().actions_refused, 1);

    recorder.attach();
    let resolved = service(&mut access, commands(window()));
    assert!(resolved.is_empty());
    assert_eq!(access.counters().activations, 2);
    assert_eq!(access.counters().rebuilds, 2);
}

// ------------------------------------------------------------------ actions

#[test]
fn an_action_is_routed_to_the_control_the_client_actually_named() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));

    let close = Occurrence::window_command(window(), part::CLOSE);
    recorder.ask(close.node_id(), Action::Click);
    let resolved = service(&mut access, commands(window()));
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence, close);
    assert_eq!(resolved[0].action, Action::Click);
    assert_eq!(access.counters().actions_routed, 1);
    assert_eq!(access.counters().actions_refused, 0);
}

#[test]
fn an_action_against_a_retired_window_is_refused_rather_than_routed() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));

    // A request that has reached the pane but not yet been resolved when the
    // window goes. It is refused, and counted — a client that asked and saw
    // nothing happen is exactly the report this feature will get.
    let close = Occurrence::window_command(window(), part::CLOSE);
    recorder.ask(close.node_id(), Action::Click);
    access.poll();
    access.retire();
    assert_eq!(access.counters().actions_refused, 1);
    assert!(access.resolve().is_empty());

    // And one that arrives afterwards is refused outright rather than routed
    // to whatever the process still has open.
    recorder.ask(close.node_id(), Action::Click);
    assert!(service(&mut access, commands(window())).is_empty());
    assert_eq!(access.counters().actions_routed, 0);
}

#[test]
fn a_second_window_never_answers_the_first_windows_ids() {
    let first = Recorder::default();
    let second = Recorder::default();
    let mut one = PaneAccessibility::new(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        Some(Box::new(first.clone())),
    );
    let mut two = PaneAccessibility::new(
        WindowSlot::from_raw(2),
        ViewSlot::from_raw(2),
        Some(Box::new(second.clone())),
    );
    one.revealed();
    two.revealed();
    first.attach();
    second.attach();
    service(&mut one, commands(WindowSlot::from_raw(1)));
    service(&mut two, commands(WindowSlot::from_raw(2)));

    // The id of window one's Close button, replayed at window two.
    let close_of_one = Occurrence::window_command(WindowSlot::from_raw(1), part::CLOSE);
    second.ask(close_of_one.node_id(), Action::Click);
    assert!(service(&mut two, commands(WindowSlot::from_raw(2))).is_empty());
    assert_eq!(two.counters().actions_refused, 1);

    // And window one still answers its own.
    first.ask(close_of_one.node_id(), Action::Click);
    assert_eq!(
        service(&mut one, commands(WindowSlot::from_raw(1))).len(),
        1
    );
}

#[test]
fn a_menu_that_closed_does_not_hand_its_action_to_the_one_that_replaced_it() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();

    // A transient surface is a view instance, so the menu that opens second is
    // a different instance of the same menu with the same rows.
    let item = |view: ViewSlot| {
        SemanticNode::new(
            Occurrence::surface(window(), view, "menu", ControlPart::new("item-0")),
            accesskit::Role::MenuItem,
        )
        .with_label("Delete")
        .with_action(Action::Click)
    };
    let first_menu = ViewSlot::from_raw(10);
    let second_menu = ViewSlot::from_raw(11);
    service(&mut access, vec![item(first_menu)]);

    // The client asks for the row of the menu that has since closed, while a
    // fresh menu with an identical row is on screen.
    let stale = Occurrence::surface(window(), first_menu, "menu", ControlPart::new("item-0"));
    recorder.ask(stale.node_id(), Action::Click);
    access.invalidate();
    let resolved = service(&mut access, vec![item(second_menu)]);
    assert!(
        resolved.is_empty(),
        "a queued action reached the menu that replaced the one it was for"
    );
    assert_eq!(access.counters().actions_refused, 1);
}

#[test]
fn an_action_for_another_tree_is_refused() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));

    let close = Occurrence::window_command(window(), part::CLOSE);
    recorder
        .0
        .lock()
        .expect("no test poisons this")
        .pending
        .actions
        .push(ActionRequest {
            action: Action::Click,
            target_tree: TreeId(Uuid::from_u128(7)),
            target_node: close.node_id(),
            data: None,
        });
    assert!(service(&mut access, commands(window())).is_empty());
    assert_eq!(access.counters().actions_refused, 1);
}

#[test]
fn dropped_requests_are_counted_rather_than_lost_quietly() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    recorder
        .0
        .lock()
        .expect("no test poisons this")
        .pending
        .dropped_actions = 3;
    service(&mut access, commands(window()));
    assert_eq!(access.counters().actions_dropped, 3);
}

// ------------------------------------------------- the tree became a delta

/// One pass of the loop's drain, publishing a **difference** rather than a
/// whole window.
fn service_delta(
    access: &mut PaneAccessibility,
    children: Vec<SemanticNode>,
    retired: Vec<Occurrence>,
) -> Vec<super::Resolved> {
    let slot = access.slot();
    if access.poll().snapshot {
        access.publish(&snapshot(slot, children).as_delta(retired));
    }
    access.resolve()
}

/// **The latent bug Plan 1b.1 named, and the reason it was named first.**
///
/// Before the fix, `publish` stored the occurrence map *of the update it had
/// just built* and `resolve` refused anything not in it. With a whole tree that
/// map was every control, so nothing noticed. The moment an update carries only
/// what changed — which is the whole of this plan — a client clicking a control
/// that did not happen to change is refused, `actions_refused` climbs, and
/// every other counter says the window is healthy.
///
/// This test fails on the previous implementation: the delta below carries only
/// the close button, so Minimize is not in the map the old code kept, and the
/// request is refused instead of routed.
#[test]
fn a_delta_does_not_make_an_untouched_control_unreachable() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();

    // Activation: the whole window, all three commands.
    service(&mut access, commands(window()));
    let minimize = Occurrence::window_command(window(), part::MINIMIZE);

    // An ordinary update afterwards: only the close button changed.
    let close_only: Vec<SemanticNode> = commands(window())
        .into_iter()
        .filter(|node| node.occurrence.part == part::CLOSE)
        .collect();
    assert_eq!(close_only.len(), 1);
    access.invalidate();
    service_delta(&mut access, close_only, Vec::new());
    assert_eq!(access.counters().deltas, 1);

    // And now the client clicks Minimize, which no update since the first has
    // mentioned.
    recorder.ask(minimize.node_id(), Action::Click);
    let resolved = service_delta(&mut access, Vec::new(), Vec::new());
    assert_eq!(
        access.counters().actions_refused,
        0,
        "a control that did not change was refused after an incremental update"
    );
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence, minimize);
}

/// The other half of an accumulating map: something that is genuinely gone has
/// to stop being routable, and in a delta nothing else says so.
#[test]
fn a_delta_that_retires_a_control_stops_routing_it() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));

    let minimize = Occurrence::window_command(window(), part::MINIMIZE);
    access.invalidate();
    service_delta(&mut access, Vec::new(), vec![minimize]);

    recorder.ask(minimize.node_id(), Action::Click);
    assert!(service_delta(&mut access, Vec::new(), Vec::new()).is_empty());
    assert_eq!(access.counters().actions_refused, 1);
}

/// A whole snapshot is authoritative: whatever the map held before it, what it
/// holds after is exactly what the snapshot said.
#[test]
fn a_whole_snapshot_replaces_the_map_rather_than_adding_to_it() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));

    // A second activation — a client detached and another attached — carrying
    // one command instead of three.
    recorder.detach();
    access.poll();
    recorder.attach();
    let close_only: Vec<SemanticNode> = commands(window())
        .into_iter()
        .filter(|node| node.occurrence.part == part::CLOSE)
        .collect();
    service(&mut access, close_only);

    let minimize = Occurrence::window_command(window(), part::MINIMIZE);
    recorder.ask(minimize.node_id(), Action::Click);
    assert!(service(&mut access, commands(window())).is_empty());
    assert_eq!(
        access.counters().actions_refused,
        1,
        "a control left out of a full snapshot is not on screen and must not route"
    );
}

/// The caller has to know *before* it prepares the semantics, because preparing
/// a delta is what makes an edit cheap and preparing a whole window is what
/// makes an activation correct.
#[test]
fn a_poll_says_whether_a_whole_snapshot_is_owed() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();

    let first = access.poll();
    assert!(
        first.snapshot && first.full,
        "an attaching client is owed all of it"
    );
    access.publish(&snapshot(window(), commands(window())));

    access.invalidate();
    let second = access.poll();
    assert!(
        second.snapshot && !second.full,
        "an ordinary change is a difference"
    );

    // A client going away and another arriving owes a whole one again: the tree
    // the first held is gone, and a difference against it would be orphans.
    recorder.detach();
    access.poll();
    recorder.attach();
    let third = access.poll();
    assert!(third.snapshot && third.full);
}

// ------------------------------------------------------------- the dump

/// **Zero serialization and zero content-copy work when off.** No dump was
/// asked for, so none was written and nothing was copied — which the counter
/// says, because a comment could not.
#[test]
fn a_dump_costs_nothing_when_nobody_asked_for_one() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    for _ in 0..50 {
        access.invalidate();
        service(&mut access, commands(window()));
    }
    assert_eq!(access.counters().dumps, 0);
    assert!(!access.dump_armed());
    assert_eq!(access.take_dump(), None);
}

/// One request, one dump. Not one per publish, and not one per frame.
#[test]
fn a_dump_is_taken_once_by_the_next_publish_and_never_again() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    recorder.attach();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().dumps, 0);

    access.request_dump(crate::accessibility::Redaction::Structure);
    assert!(access.dump_armed());
    access.invalidate();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().dumps, 1);
    let text = access
        .take_dump()
        .expect("the dump was kept for its caller");
    assert!(text.contains("[koru:a11y] dump window=w1"));
    assert!(text.contains("counters rebuilds="));

    for _ in 0..20 {
        access.invalidate();
        service(&mut access, commands(window()));
    }
    assert_eq!(access.counters().dumps, 1, "a dump repeated itself");
    assert_eq!(access.take_dump(), None);
}

/// A request made while nobody is listening is not lost — there is no tree to
/// describe yet, and the person asking wants the first one.
#[test]
fn a_dump_asked_for_before_a_client_attached_waits_for_the_first_tree() {
    let recorder = Recorder::default();
    let mut access = pane(&recorder);
    access.revealed();
    access.request_dump(crate::accessibility::Redaction::Structure);

    access.invalidate();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().dumps, 0);
    assert!(access.dump_armed(), "the request was dropped on the floor");

    recorder.attach();
    service(&mut access, commands(window()));
    assert_eq!(access.counters().dumps, 1);
    assert!(!access.dump_armed());
}
