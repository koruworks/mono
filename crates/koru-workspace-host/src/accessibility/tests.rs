// SPDX-License-Identifier: MPL-2.0

//! Identity and projection: what must stay true while everything a person can
//! see changes underneath it.

use std::collections::HashSet;

use accesskit::{Action, Role};
use koru_draw::geometry::Rect;
use koru_i18n::Lang;
use uuid::Uuid;

use super::identity::{ControlPart, LOCATOR_PREFIX, ObjectKey, Occurrence, Slots, ViewSlot, part};
use super::projection::{SemanticNode, WindowSemantics};
use super::{CaptionGeometry, WindowSlot, window_commands};

const LABEL: ControlPart = ControlPart::new("label");

fn doc() -> Uuid {
    Uuid::parse_str("6f9619ff-8b86-d011-b42d-00cf4fc964ff").expect("a fixed fixture uuid")
}

fn semantics(window: WindowSlot, label: &str, children: Vec<SemanticNode>) -> WindowSemantics {
    WindowSemantics {
        window,
        label: label.to_string(),
        scale: 1.0,
        client: Rect::new(0.0, 0.0, 1200.0, 800.0),
        focus: None,
        children,
        scope: super::projection::Scope::Whole,
        retired: Vec::new(),
    }
}

/// `build` with nothing already published, which is what a whole snapshot
/// always means.
fn build(semantics: &WindowSemantics) -> super::projection::Projection {
    super::projection::build(semantics, &std::collections::HashMap::new())
}

// ---------------------------------------------------------------- identity

#[test]
fn a_locator_names_the_four_things_that_can_differ() {
    let occurrence =
        Occurrence::model(WindowSlot::from_raw(3), ViewSlot::from_raw(7), doc(), LABEL);
    assert_eq!(
        occurrence.locator(),
        "koru:w3/v7/n:6f9619ff8b86d011b42d00cf4fc964ff#label"
    );
    assert!(occurrence.locator().starts_with(LOCATOR_PREFIX));
}

#[test]
fn the_same_block_in_two_windows_is_two_occurrences() {
    let left = Occurrence::model(WindowSlot::from_raw(1), ViewSlot::from_raw(1), doc(), LABEL);
    let right = Occurrence::model(WindowSlot::from_raw(2), ViewSlot::from_raw(1), doc(), LABEL);
    assert_ne!(left, right);
    assert_ne!(left.locator(), right.locator());
    assert_ne!(left.node_id(), right.node_id());
}

#[test]
fn the_same_block_in_two_tabs_of_one_window_is_two_occurrences() {
    let first = Occurrence::model(WindowSlot::from_raw(1), ViewSlot::from_raw(1), doc(), LABEL);
    let second = Occurrence::model(WindowSlot::from_raw(1), ViewSlot::from_raw(2), doc(), LABEL);
    assert_ne!(first.node_id(), second.node_id());
}

#[test]
fn two_parts_of_one_object_are_two_occurrences() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let label = Occurrence::model(window, view, doc(), LABEL);
    let checkbox = Occurrence::model(window, view, doc(), ControlPart::new("checkbox"));
    assert_ne!(label.node_id(), checkbox.node_id());
}

#[test]
fn a_slot_is_never_reused() {
    let mut slots = Slots::default();
    let first = slots.window();
    let second = slots.window();
    let third = slots.window();
    assert_ne!(first, second);
    assert_ne!(second, third);
    // The first window closing changes nothing: the allocator only ever counts
    // up, which is what lets a request for a window that is gone be refused
    // rather than land on a survivor.
    let after_a_close = slots.window();
    assert!(after_a_close.raw() > third.raw());

    // Views start above the reserved window slot.
    assert_ne!(slots.view(), ViewSlot::WINDOW);
}

#[test]
fn node_ids_do_not_collide_across_a_realistic_tree() {
    let mut ids = HashSet::new();
    let mut locators = HashSet::new();
    for window in 1..=8u64 {
        for view in 0..=32u64 {
            for object in 0..64u64 {
                for part in ["root", "label", "checkbox", "expand"] {
                    let occurrence = Occurrence {
                        window: WindowSlot::from_raw(window),
                        view: ViewSlot::from_raw(view),
                        object: ObjectKey::Model(Uuid::from_u128(u128::from(object))),
                        part: ControlPart::new(part),
                    };
                    assert!(locators.insert(occurrence.locator()));
                    assert!(
                        ids.insert(occurrence.node_id()),
                        "two locators hashed to one AccessKit node id"
                    );
                }
            }
        }
    }
    assert_eq!(ids.len(), 8 * 33 * 64 * 4);
}

#[test]
fn a_node_id_is_never_the_adapters_placeholder_root() {
    // `accesskit_windows` serves `NodeId(0)` as its own placeholder root until
    // our first full update lands; publishing a node with that id would make
    // two different things share one.
    for window in 1..2000u64 {
        let occurrence = Occurrence::window_root(WindowSlot::from_raw(window));
        assert_ne!(occurrence.node_id().0, 0);
    }
    assert_ne!(super::identity::node_id_of(""), 0);
}

// -------------------------------------------------------- label independence

#[test]
fn identity_is_the_same_in_every_language() {
    let window = WindowSlot::from_raw(1);
    let mut labels = HashSet::new();
    let mut locators: Option<Vec<String>> = None;
    for lang in Lang::ALL {
        let commands = window_commands(window, false, lang, CaptionGeometry::default());
        for command in &commands {
            labels.insert(command.label.clone().expect("a window command is labelled"));
        }
        let seen: Vec<String> = commands
            .iter()
            .map(|command| command.occurrence.locator())
            .collect();
        match &locators {
            None => locators = Some(seen),
            Some(first) => assert_eq!(first, &seen, "a locator changed with the drawn language"),
        }
    }
    let locators = locators.expect("eight languages");
    assert_eq!(
        locators,
        vec![
            "koru:w1/v0/s:window#minimize",
            "koru:w1/v0/s:window#maximize",
            "koru:w1/v0/s:window#close",
        ]
    );
    // And the labels really did differ, or the assertion above proves nothing.
    assert!(labels.len() > 3, "the eight languages drew the same words");
}

#[test]
fn renaming_a_window_does_not_change_any_identity() {
    let window = WindowSlot::from_raw(4);
    let before = build(&semantics(
        window,
        "Notes",
        window_commands(window, false, Lang::En, CaptionGeometry::default()),
    ));
    let after = build(&semantics(
        window,
        "Renamed â€” Notes",
        window_commands(window, false, Lang::Ja, CaptionGeometry::default()),
    ));
    let mut before_ids: Vec<_> = before.occurrences.keys().copied().collect();
    let mut after_ids: Vec<_> = after.occurrences.keys().copied().collect();
    before_ids.sort_by_key(|id| id.0);
    after_ids.sort_by_key(|id| id.0);
    assert_eq!(before_ids, after_ids);
}

#[test]
fn a_window_command_is_published_where_the_view_draws_it() {
    let window = WindowSlot::from_raw(1);
    let geometry = CaptionGeometry {
        minimize: Some(Rect::new(1100.0, 0.0, 36.0, 36.0)),
        maximize: Some(Rect::new(1136.0, 0.0, 36.0, 36.0)),
        // A view that draws no close box â€” an onboarding window, a probe â€” is
        // a legal answer, and AccessKit serves it as an empty rectangle.
        close: None,
    };
    let commands = window_commands(window, false, Lang::En, geometry);
    assert_eq!(commands[0].bounds, geometry.minimize);
    assert_eq!(commands[1].bounds, geometry.maximize);
    assert_eq!(commands[2].bounds, None);

    // â€¦and the scale is still applied exactly once, by `build` and nowhere
    // else â€” which is the whole reason the view publishes logical rectangles.
    let mut snapshot = semantics(window, "Notes", commands);
    snapshot.scale = 2.0;
    let projection = build(&snapshot);
    let minimize = Occurrence::window_command(window, part::MINIMIZE).node_id();
    let node = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == minimize)
        .map(|(_, node)| node)
        .expect("the minimise command is published");
    assert_eq!(
        node.bounds(),
        Some(accesskit::Rect::new(2200.0, 0.0, 2272.0, 72.0))
    );
}

#[test]
fn an_instance_is_its_own_object() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(2);
    let first = Occurrence::instance(window, view, "tab", 7, ControlPart::ROOT);
    let second = Occurrence::instance(window, view, "tab", 8, ControlPart::ROOT);
    assert_eq!(first.locator(), "koru:w1/v2/tab:7#root");
    assert_ne!(first.node_id(), second.node_id());
    // A different kind at the same number is a different thing, so a menu and
    // a tab cannot collide however their two owners number themselves.
    let menu = Occurrence::instance(window, view, "menu", 7, ControlPart::ROOT);
    assert_ne!(first.node_id(), menu.node_id());
    // â€¦and an instance is never a model object, whatever the numbers say.
    let model = Occurrence::model(window, view, Uuid::from_u128(7), ControlPart::ROOT);
    assert_ne!(first.locator(), model.locator());
}

#[test]
fn instance_locators_do_not_collide_across_a_realistic_window() {
    let mut ids = HashSet::new();
    let mut locators = HashSet::new();
    for view in 0..8u64 {
        for kind in ["tab", "menu", "popover"] {
            for instance in 0..256u64 {
                for part in ["root", "close", "item"] {
                    let occurrence = Occurrence::instance(
                        WindowSlot::from_raw(1),
                        ViewSlot::from_raw(view),
                        kind,
                        instance,
                        ControlPart::new(part),
                    );
                    assert!(locators.insert(occurrence.locator()));
                    assert!(
                        ids.insert(occurrence.node_id()),
                        "two instance locators hashed to one AccessKit node id"
                    );
                }
            }
        }
    }
}

#[test]
fn a_maximised_window_relabels_its_button_without_renaming_it() {
    let window = WindowSlot::from_raw(1);
    let restored = window_commands(window, false, Lang::En, CaptionGeometry::default());
    let maximised = window_commands(window, true, Lang::En, CaptionGeometry::default());
    assert_eq!(restored[1].occurrence, maximised[1].occurrence);
    assert_ne!(restored[1].label, maximised[1].label);
}

#[test]
fn reordering_the_children_does_not_change_any_identity() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let rows: Vec<SemanticNode> = (0..4u128)
        .map(|n| {
            SemanticNode::new(
                Occurrence::model(window, view, Uuid::from_u128(n), ControlPart::ROOT),
                Role::ListItem,
            )
            .with_label(format!("row {n}"))
        })
        .collect();
    let forward = build(&semantics(window, "Notes", rows.clone()));
    let mut reversed = rows;
    reversed.reverse();
    let backward = build(&semantics(window, "Notes", reversed));

    let mut a: Vec<u64> = forward.occurrences.keys().map(|id| id.0).collect();
    let mut b: Vec<u64> = backward.occurrences.keys().map(|id| id.0).collect();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
    // The order the client walks did change, which is what makes the test
    // about reordering rather than about nothing happening.
    assert_ne!(forward.update.nodes[0].0, backward.update.nodes[0].0);
}

// -------------------------------------------------------------- projection

#[test]
fn the_root_carries_the_window_locator_and_a_findable_class_name() {
    let window = WindowSlot::from_raw(2);
    let projection = build(&semantics(window, "Notes", Vec::new()));
    let root_id = Occurrence::window_root(window).node_id();
    assert_eq!(
        projection.update.tree.as_ref().expect("a tree").root,
        root_id
    );
    let (_, root) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == root_id)
        .expect("the root is published");
    assert_eq!(root.role(), Role::Window);
    assert_eq!(root.author_id(), Some("koru:w2/v0/s:window#window"));
    assert_eq!(root.class_name(), Some("KoruWindow"));
    assert_eq!(root.label(), Some("Notes"));
    assert_eq!(
        projection
            .update
            .tree
            .as_ref()
            .and_then(|tree| tree.toolkit_name.as_deref()),
        Some("Koru")
    );
}

#[test]
fn every_published_node_carries_its_own_locator() {
    let window = WindowSlot::from_raw(1);
    let projection = build(&semantics(
        window,
        "Notes",
        window_commands(window, false, Lang::De, CaptionGeometry::default()),
    ));
    assert_eq!(projection.nodes, 4);
    assert_eq!(projection.duplicate_ids, 0);
    for (id, node) in &projection.update.nodes {
        let occurrence = projection.occurrences.get(id).expect("every id is mapped");
        assert_eq!(node.author_id(), Some(occurrence.locator().as_str()));
    }
}

#[test]
fn a_control_published_twice_is_counted_rather_than_silently_reparented() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let twice = Occurrence::model(window, view, doc(), ControlPart::ROOT);
    let projection = build(&semantics(
        window,
        "Notes",
        vec![
            SemanticNode::new(twice, Role::ListItem),
            SemanticNode::new(twice, Role::ListItem),
        ],
    ));
    assert_eq!(projection.duplicate_ids, 1);
}

#[test]
fn logical_bounds_are_scaled_to_physical_client_pixels_exactly_once() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let row = SemanticNode::new(
        Occurrence::model(window, view, doc(), ControlPart::ROOT),
        Role::ListItem,
    )
    .with_bounds(Rect::new(10.0, 20.0, 100.0, 40.0));
    let mut snapshot = semantics(window, "Notes", vec![row]);
    snapshot.scale = 1.5;
    let projection = build(&snapshot);

    let root_id = Occurrence::window_root(window).node_id();
    let (_, root) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == root_id)
        .expect("the root");
    let client = root.bounds().expect("the root has the client area");
    assert_eq!((client.x0, client.y0), (0.0, 0.0));
    assert_eq!((client.x1, client.y1), (1800.0, 1200.0));

    let (_, node) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id != root_id)
        .expect("the row");
    let bounds = node.bounds().expect("the row has bounds");
    assert_eq!((bounds.x0, bounds.y0), (15.0, 30.0));
    assert_eq!((bounds.x1, bounds.y1), (165.0, 90.0));
}

#[test]
fn a_window_command_offers_only_the_action_something_performs() {
    let commands = window_commands(
        WindowSlot::from_raw(1),
        false,
        Lang::Fr,
        CaptionGeometry::default(),
    );
    for command in commands {
        assert_eq!(command.actions, vec![Action::Click]);
    }
}

#[test]
fn focus_falls_back_to_the_window_when_it_names_something_that_is_gone() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let mut snapshot = semantics(
        window,
        "Notes",
        window_commands(window, false, Lang::En, CaptionGeometry::default()),
    );
    snapshot.focus = Some(Occurrence::model(window, view, doc(), ControlPart::ROOT));
    let projection = build(&snapshot);
    assert!(projection.unresolved_focus);
    assert_eq!(
        projection.update.focus,
        Occurrence::window_root(window).node_id()
    );

    // And a focus that is present is published as itself.
    let mut snapshot = semantics(
        window,
        "Notes",
        window_commands(window, false, Lang::En, CaptionGeometry::default()),
    );
    let close = Occurrence::window_command(window, part::CLOSE);
    snapshot.focus = Some(close);
    let projection = build(&snapshot);
    assert!(!projection.unresolved_focus);
    assert_eq!(projection.update.focus, close.node_id());
}

/// **A difference that retires the focused control does not name it.** The
/// client held it before this update and will not after; AccessKit panics on
/// a focus outside its tree, which is how switching profile once closed the
/// window. A control the client holds and this update leaves alone is still
/// published as itself.
#[test]
fn a_delta_never_focuses_a_control_it_retires() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let row = Occurrence::model(window, view, doc(), ControlPart::ROOT);
    let known = std::collections::HashMap::from([(row.node_id(), row)]);

    let mut delta = semantics(window, "Notes", Vec::new()).as_delta(vec![row]);
    delta.focus = Some(row);
    let projection = super::projection::build(&delta, &known);
    assert!(projection.unresolved_focus);
    assert_eq!(
        projection.update.focus,
        Occurrence::window_root(window).node_id()
    );

    let mut delta = semantics(window, "Notes", Vec::new()).as_delta(Vec::new());
    delta.focus = Some(row);
    let projection = super::projection::build(&delta, &known);
    assert!(!projection.unresolved_focus);
    assert_eq!(projection.update.focus, row.node_id());
}

// --------------------------------------------------- the one creation site

/// **The reveal audit, kept true by a test rather than by a memory.**
///
/// Plan 1a.1 had to audit every `set_visible(true)` in the host and register
/// the bridge before the first one. Every reveal path in `app.rs` â€”
/// `reveal_pane`, a restore from the tray, search bringing a window forward,
/// a background surface asking to be shown â€” acts on a `Pane` that already
/// exists, and a `Pane` cannot be built without a `PaneAccessibility`. So the
/// audit reduces to a single question: **how many places create a window?**
///
/// Two, and they are `App::open_pane` and `OnboardingApp::open`. Both seat a
/// provider immediately after `create_window`, while the window is still
/// hidden. The first-run window was left out by Plan 1a.1 â€” a provider with
/// nothing to publish advertises an empty tree, which is worse than the
/// window's own native fallback â€” and Plan U0.1 filled it in, which is why
/// there are now two seat sites rather than one.
///
/// The first-run window's own reveal is a single `set_visible(true)` inside
/// `OnboardingApp::redraw`, guarded by `is_visible`, and `open` runs once
/// behind `needs_onboarding_window`. A third creation site is somebody adding
/// a window that reveals without a provider â€” the exact defect this plan
/// exists to prevent â€” so it fails here and asks the question out loud
/// instead.
#[test]
fn every_window_the_host_creates_is_accounted_for() {
    let app = include_str!("../app.rs");
    assert_eq!(
        app.matches("event_loop.create_window(").count(),
        2,
        "a window creation site was added or removed in app.rs. Every covered window must be \
         handed a provider by `seat_accessibility` while it is still hidden â€” see \
         `crate::accessibility` and `koru_window::accessibility`."
    );
    assert_eq!(
        app.matches("self.seat_accessibility(").count(),
        2,
        "`seat_accessibility` is what installs the provider before the first reveal; there is \
         one call per window creation site â€” `App::open_pane` and `OnboardingApp::open`."
    );
}

/// **The slot allocator survives the handover, and only two places could start
/// it again from zero.**
///
/// One process shows two windows in sequence: the first-run window, and the
/// workspace window that replaces it. Both take their numbers from the same
/// [`Slots`], because a `WindowSlot` handed out twice is the end of
/// stale-request refusal â€” a request queued against the first-run window would
/// land on the workspace window that replaced it, and every locator an
/// inspector recorded during setup would name a control in the wrong window.
///
/// `ContentState::new_with_ack` takes the allocator as an argument for exactly
/// that reason, and the transition destructures `slots` out of the going
/// `OnboardingApp` and hands it over. So the audit reduces to: **how many places
/// mint a fresh one?** Two, and both are the start of a cold process â€”
/// `ContentState::new`, and the `NeedsProfile` arm that opens onboarding. A
/// third would be somebody re-creating one across a transition, which is the
/// defect, so it fails here and asks the question out loud.
#[test]
fn the_slot_allocator_is_carried_across_the_first_run_handover() {
    let app = include_str!("../app.rs");
    assert_eq!(
        app.matches("crate::accessibility::Slots::default()")
            .count(),
        2,
        "a fresh slot allocator was minted somewhere new in app.rs. A window slot is handed out \
         once and never reused; onboarding hands its allocator on to the workspace window rather \
         than letting it start again from zero â€” see `crate::accessibility::identity`."
    );
    // â€¦and the transition really does take it out of the window that is going,
    // rather than dropping it with the rest of the first-run state.
    let handover = app
        .split_once("let OnboardingApp {")
        .and_then(|(_, rest)| rest.split_once("} = *app;"))
        .map(|(fields, _)| fields)
        .expect("the first-run handover destructures `OnboardingApp`");
    assert!(
        handover.contains("slots,"),
        "the first-run handover no longer carries `slots` over to the workspace window"
    );
}

/// **"No I/O and no reflow from an accessibility callback", asserted at the
/// seam rather than with a stopwatch.**
///
/// Two halves make it true. `View::semantics` takes `&self`, and
/// `Editing::reflow` takes `&mut` â€” so a view physically cannot lay anything
/// out from a semantic query, and the compiler is the check. And this module
/// is handed a `WindowSemantics` value: no workspace, no store session, no
/// editor, no filesystem. What that leaves is a rule about *imports*, which is
/// exactly the thing a later edit would break by reaching for the obvious
/// thing, so it is pinned here rather than left as a paragraph.
#[test]
fn the_projection_cannot_reach_the_store_the_editor_or_the_disk() {
    const SOURCES: [(&str, &str); 4] = [
        ("accessibility.rs", include_str!("../accessibility.rs")),
        ("identity.rs", include_str!("identity.rs")),
        ("projection.rs", include_str!("projection.rs")),
        ("lifecycle.rs", include_str!("lifecycle.rs")),
    ];
    // Every owner of a read that would block the thread answering a person, or
    // of a layout pass. Matched against `use` lines only, so the module docs
    // stay free to name what they forbid.
    const FORBIDDEN: [&str; 8] = [
        "koru_workspace",
        "koru_docstore",
        "koru_editor",
        "koru_store",
        "koru_projection",
        "koru_search",
        "std::fs",
        "std::io",
    ];
    for (name, source) in SOURCES {
        for line in source
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("use "))
        {
            for forbidden in FORBIDDEN {
                assert!(
                    !line.contains(forbidden),
                    "{name} imports {forbidden}: a semantic query reads prepared in-memory \
                     state and must not read the Store, touch the disk, or lay anything out. \
                     If a projection needs more, the window's owner puts it in the \
                     `WindowSemantics` snapshot before the query arrives."
                );
            }
        }
    }
}

/// A locator is read out of an inspector, typed into a scenario file and
/// compared byte for byte, so its vocabulary is deliberately narrow: lowercase
/// ASCII, digits and `-`. This pins the parts this crate defines and states the
/// convention for the ones later plans add.
#[test]
fn a_control_part_is_a_lowercase_ascii_keyword() {
    for part in [
        part::WINDOW,
        part::MINIMIZE,
        part::MAXIMIZE,
        part::CLOSE,
        ControlPart::ROOT,
    ] {
        let name = part.name();
        assert!(!name.is_empty());
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{name} is not a lowercase ASCII keyword; a locator must survive being typed \
             by hand and compared byte for byte"
        );
        // The separators the locator itself uses must not appear inside a part.
        assert!(!name.contains(['#', '/', ':']));
    }
}

// -------------------------------------------------------------------- text

/// A text run in the shape a text control's owner hands one over: a value, one
/// length per character, and geometry in logical pixels.
fn text_run(id: u64, value: &str) -> (accesskit::NodeId, accesskit::Node) {
    let mut node = accesskit::Node::new(Role::TextRun);
    node.set_value(value.to_string());
    node.set_character_lengths(
        value
            .chars()
            .map(|c| c.len_utf8() as u8)
            .collect::<Vec<_>>(),
    );
    node.set_character_positions(
        (0..value.chars().count())
            .map(|i| i as f32 * 10.0)
            .collect::<Vec<_>>(),
    );
    node.set_character_widths(vec![10.0; value.chars().count()]);
    node.set_bounds(accesskit::Rect::new(4.0, 8.0, 44.0, 24.0));
    node.set_text_direction(accesskit::TextDirection::LeftToRight);
    (accesskit::NodeId(id), node)
}

fn paragraph_with_text(window: WindowSlot, view: ViewSlot) -> SemanticNode {
    SemanticNode::new(
        Occurrence::model(window, view, doc(), ControlPart::ROOT),
        Role::MultilineTextInput,
    )
    .with_text(super::projection::TextBody {
        runs: super::projection::Runs::Published(vec![text_run(7001, "abcd")]),
        selection: Some(accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: accesskit::NodeId(7001),
                character_index: 1,
            },
            focus: accesskit::TextPosition {
                node: accesskit::NodeId(7001),
                character_index: 3,
            },
        }),
        read_only: false,
    })
}

/// A text run is published as a child of the control that carries it, under the
/// id its owner chose â€” the id the owner's own position map is keyed by.
#[test]
fn a_text_control_publishes_its_runs_as_children_under_the_owners_ids() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let paragraph = paragraph_with_text(window, view);
    let paragraph_id = paragraph.occurrence.node_id();
    let projection = build(&semantics(window, "Notes", vec![paragraph]));

    let (_, node) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == paragraph_id)
        .expect("the paragraph");
    assert_eq!(node.children(), &[accesskit::NodeId(7001)]);
    assert!(
        node.text_selection().is_some(),
        "the editor's selection has to reach the node a client queries"
    );

    let (_, run) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == accesskit::NodeId(7001))
        .expect("the run");
    assert_eq!(run.value(), Some("abcd"));
    // A run is not a control, so it has no occurrence and is not routable.
    assert!(
        !projection
            .occurrences
            .contains_key(&accesskit::NodeId(7001))
    );
}

/// The run's box, the character offsets inside it and the character advances
/// are three geometries in one space, and all three have to be scaled or a
/// magnifier highlights the wrong glyph on any display above 100%.
#[test]
fn a_text_runs_character_geometry_is_scaled_with_its_box() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let mut snapshot = semantics(window, "Notes", vec![paragraph_with_text(window, view)]);
    snapshot.scale = 2.0;
    let projection = build(&snapshot);

    let (_, run) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == accesskit::NodeId(7001))
        .expect("the run");
    let bounds = run.bounds().expect("the run has a box");
    assert_eq!((bounds.x0, bounds.y0), (8.0, 16.0));
    assert_eq!(
        run.character_positions(),
        Some([0.0, 20.0, 40.0, 60.0].as_slice())
    );
    assert_eq!(run.character_widths(), Some([20.0; 4].as_slice()));
}

/// Read-only is a fact a screen reader announces; it is stated rather than
/// inferred from the absence of an action.
#[test]
fn a_read_only_text_control_says_so() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let mut paragraph = paragraph_with_text(window, view);
    if let Some(text) = paragraph.text.as_mut() {
        text.read_only = true;
    }
    let paragraph_id = paragraph.occurrence.node_id();
    let projection = build(&semantics(window, "Notes", vec![paragraph]));
    let (_, node) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == paragraph_id)
        .expect("the paragraph");
    assert!(node.is_read_only());
}

// ------------------------------------------------------------- announcements

/// **A live setting reaches the adapter, and it reaches only the node that
/// asked for it.**
///
/// `accesskit_consumer::Node::live` falls through to the parent when a node has
/// none of its own, so a container marked live makes every descendant live too
/// â€” and a status that moves with every keystroke, sitting under an
/// announcement that does not, would then be spoken on every keystroke. The
/// projection therefore has to carry the setting exactly where it was put and
/// nowhere else. See [`SemanticNode::live`] and `guide/contributing/deps/ACCESSKIT.md`.
#[test]
fn a_live_setting_is_published_on_the_node_that_asked_for_it_and_no_other() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let quiet = SemanticNode::new(
        Occurrence::surface(window, view, "save", ControlPart::new("document")),
        Role::Label,
    )
    .with_label("Saved.")
    .with_bounds(Rect::new(0.0, 0.0, 10.0, 10.0));
    let announcement = SemanticNode::new(
        Occurrence::surface(window, view, "save", ControlPart::new("news")),
        Role::Alert,
    )
    .with_label("Everything is saved again.")
    .announcing(accesskit::Live::Polite);
    let quiet_id = quiet.occurrence.node_id();
    let announcement_id = announcement.occurrence.node_id();
    let group = SemanticNode::new(
        Occurrence::surface(window, view, "save", ControlPart::ROOT),
        Role::Group,
    )
    .with_label("Save status")
    .with_children(vec![quiet, announcement]);
    let group_id = group.occurrence.node_id();

    let projection = build(&semantics(window, "Notes", vec![group]));
    let live_of = |wanted: accesskit::NodeId| {
        projection
            .update
            .nodes
            .iter()
            .find(|(id, _)| *id == wanted)
            .map(|(_, node)| node.live())
            .expect("a published node")
    };
    assert_eq!(live_of(announcement_id), Some(accesskit::Live::Polite));
    assert_eq!(
        live_of(quiet_id),
        None,
        "the status would be spoken on every keystroke"
    );
    assert_eq!(
        live_of(group_id),
        None,
        "a live container makes every one of its children live"
    );
}

/// A node with no bounds is legal â€” the adapter answers an empty rectangle â€”
/// and it is what status text nothing draws has to publish rather than borrow a
/// neighbour's position. Pinned here because it is the property Plan 1b.4's
/// labels rest on.
#[test]
fn a_node_with_no_bounds_is_published_without_one() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let label = SemanticNode::new(
        Occurrence::surface(window, view, "save", ControlPart::new("document")),
        Role::Label,
    )
    .with_label("Saved.");
    let label_id = label.occurrence.node_id();

    let projection = build(&semantics(window, "Notes", vec![label]));
    let (_, node) = projection
        .update
        .nodes
        .iter()
        .find(|(id, _)| *id == label_id)
        .expect("the label");
    assert_eq!(node.bounds(), None);
}

/// **A `Role::Label` is nameless on Windows unless its text is also its
/// value**, so `build` puts it there.
///
/// `accesskit_windows`' `NodeWrapper::name` reads a label's name out of
/// `write_value` rather than `write_label`, because
/// `accesskit_consumer::label_comes_from_value` is `role() == Role::Label`.
/// Found by the first-run probe: a live status carrying only a label came back
/// from a real UIA client with `Name == ""`. Pinned here so the fix cannot be
/// dropped by somebody tidying the builder, and so the day AccessKit changes
/// this the assertion says so.
#[test]
fn a_label_carries_its_text_as_its_value_so_a_windows_client_can_read_it() {
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let hint = SemanticNode::new(
        Occurrence::surface(window, view, "explore", ControlPart::new("empty")),
        Role::Label,
    )
    .with_label("No documents yet.");
    let button = SemanticNode::new(
        Occurrence::surface(window, view, "explore", ControlPart::new("search")),
        Role::Button,
    )
    .with_label("Search");

    let projection = build(&semantics(
        window,
        "Notes",
        vec![hint.clone(), button.clone()],
    ));
    let node = |occurrence: Occurrence| {
        projection
            .update
            .nodes
            .iter()
            .find(|(id, _)| *id == occurrence.node_id())
            .map(|(_, node)| node.clone())
            .expect("the node was published")
    };
    assert_eq!(
        node(hint.occurrence).value(),
        Some("No documents yet."),
        "a label with no value is a control with no name on Windows"
    );
    // The label is set too, because other platforms read that one.
    assert_eq!(node(hint.occurrence).label(), Some("No documents yet."));
    // And nothing else acquires a value it did not ask for: a button with one
    // would gain a `ValuePattern` and answer `SetValue`.
    assert_eq!(node(button.occurrence).value(), None);
}
