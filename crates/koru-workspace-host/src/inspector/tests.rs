// SPDX-License-Identifier: MPL-2.0

use super::*;
use winit::event::DeviceId;
use winit::keyboard::ModifiersState;

#[test]
fn double_clicking_same_hierarchy_row_opens_its_details() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.activate(8);
    let now = std::time::Instant::now();
    assert!(!inspector.click_hierarchy_row(1, now));
    assert!(inspector.show_hierarchy);
    assert!(inspector.click_hierarchy_row(1, now + std::time::Duration::from_millis(100)));
    assert!(!inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(1));
    assert!(!inspector.capture_live());
    assert!(inspector.lines.iter().any(|s| s == "Parent: KoruButton"));
    inspector.activate(8);
    inspector.activate(4); // Enter/Space while the hierarchy owns focus.
    assert!(!inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(1));
}

#[test]
fn hierarchy_double_click_requests_source_reveal_without_losing_selection() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(520.0, 720.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.pinned = true;
    inspector.activate(8);
    inspector.pointer = (PAD + 20.0, TREE_TOP + ROW * 1.5);
    let press = inspector.panel_event(&mouse(ElementState::Pressed), 1.0, ModifiersState::empty());
    assert!(!press.reveal_source);
    inspector.panel_event(&mouse(ElementState::Released), 1.0, ModifiersState::empty());
    // Seed the previous click's timestamp so timing doesn't depend on test speed.
    inspector.hierarchy_click.as_mut().unwrap().2 = std::time::Instant::now();
    let double = inspector.panel_event(&mouse(ElementState::Pressed), 1.0, ModifiersState::empty());
    assert!(double.reveal_source && double.structure_changed && double.redraw);
    assert!(!inspector.show_hierarchy && !inspector.capture_live());
    assert_eq!(inspector.selected, Some(1));
    inspector.panel_event(&WindowEvent::Focused(false), 1.0, ModifiersState::empty());
    assert!(inspector.pinned);
    assert!(inspector.highlighted_record().is_some());
    let rect = inspector.control_rect(7);
    inspector.pointer = (rect.x + 10.0, rect.y + 10.0);
    let tab = inspector.panel_event(&mouse(ElementState::Pressed), 1.0, ModifiersState::empty());
    assert!(!tab.reveal_source);
}

#[test]
fn separate_hierarchy_clicks_do_not_open_details() {
    let now = std::time::Instant::now();
    for (second_index, elapsed, movement, revision) in [
        (0, 100, 0.0, 1),
        (1, 600, 0.0, 1),
        (1, 100, 10.0, 1),
        (1, 100, 0.0, 2),
    ] {
        let mut inspector = Inspector::new(900.0, 700.0);
        inspector.snapshot = scene_snapshot(1);
        inspector.activate(8);
        inspector.click_hierarchy_row(1, now);
        inspector.pointer.0 += movement;
        inspector.snapshot.revision = revision;
        inspector.click_hierarchy_row(
            second_index,
            now + std::time::Duration::from_millis(elapsed),
        );
        assert!(inspector.show_hierarchy);
    }
}

#[test]
fn compact_header_and_tabs_keep_hit_areas_in_visual_order() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(360.0, 360.0);
    for index in [PICK, COPY, RAYS, PIN, CLOSE] {
        let rect = inspector.control_rect(index);
        assert!(rect.y >= 0.0 && rect.y + rect.h <= HEADER);
    }
    assert!(inspector.control_rect(8).x < inspector.control_rect(7).x);
    assert_eq!(inspector.control_rect(8).y, HEADER);
    assert!(inspector.control_rect(4).h > 0.0 && inspector.control_rect(5).h > 0.0);
    let order = inspector.focus_order();
    assert!(order.iter().position(|i| *i == 8) < order.iter().position(|i| *i == 7));
    let nodes = inspector.semantics(WindowSlot::from_raw(1), 1.0);
    assert!(
        nodes
            .iter()
            .position(|n| n.occurrence.part == ControlPart::new("inspect.tab.hierarchy"))
            < nodes
                .iter()
                .position(|n| n.occurrence.part == ControlPart::new("inspect.tab.details"))
    );
    assert!(
        koru_keys::registry::get(ShortcutId::ToggleInspectorSpacingRays)
            .chord
            .matches(&Key::Named(NamedKey::F11), None, ModifiersState::empty())
    );
}

#[test]
fn spacing_toggle_preserves_snapshot_and_exposes_parent_and_toggle_state() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.select(Some(1));
    inspector.pause_refresh();
    assert!(inspector.lines.iter().any(|s| s == "Parent: KoruButton"));
    assert!(!inspector.spacing_rays);
    inspector.activate(RAYS);
    assert!(inspector.spacing_rays);
    assert!(!inspector.capture_live());
    assert_eq!(inspector.selected, Some(1));
    let nodes = inspector.semantics(WindowSlot::from_raw(1), 1.0);
    let toggle = nodes
        .iter()
        .find(|n| n.occurrence.part == ControlPart::new("inspect.spacing-rays"))
        .unwrap();
    assert_eq!(toggle.toggled, Some(accesskit::Toggled::True));
    assert!(toggle.actions.contains(&Action::Click));
}

#[test]
fn header_pick_icon_activates_without_starting_window_drag() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(520.0, 720.0);
    let rect = inspector.control_rect(PICK);
    assert!(rect.y + rect.h <= HEADER);
    assert!(inspector.control_rect(COPY).y + inspector.control_rect(COPY).h <= HEADER);
    inspector.pointer = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    let press = inspector.panel_event(&mouse(ElementState::Pressed), 1.0, ModifiersState::empty());
    assert!(press.consumed && !press.close && !press.drag_window);
    assert!(!inspector.picking);
}

#[test]
fn f12_resumes_a_paused_snapshot_then_closes_when_live() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.select(Some(1));
    inspector.pause_refresh();
    inspector.show_hierarchy = true;
    inspector.pinned = true;
    inspector.dirty_semantics = false;

    assert!(!inspector.shortcut_requests_close());
    assert!(inspector.picking && inspector.capture_live());
    assert!(inspector.dirty_semantics);
    assert_eq!(inspector.selected, Some(1));
    assert_eq!(inspector.snapshot.revision, 1);
    assert!(inspector.show_hierarchy && inspector.pinned);
    assert!(inspector.shortcut_requests_close());
}

#[test]
fn f12_focus_transfer_does_not_close_or_reopen_the_inspector() {
    use ElementState::{Pressed, Released};
    let mut open = false;
    // Source press opens; focus releases the source's key and replays the held
    // press in the inspector. Neither replay nor auto-repeat is another toggle.
    // Closing transfers focus back and must not immediately reopen it either.
    for (state, repeat, synthetic, expected, context) in [
        (Pressed, false, false, true, "source: deliberate open"),
        (Released, false, true, true, "source: focus lost"),
        (
            Pressed,
            false,
            true,
            true,
            "inspector: focus gained with F12 held",
        ),
        (Pressed, true, false, true, "inspector: held key repeat"),
        (Released, false, false, true, "inspector: real release"),
        (Pressed, false, false, false, "inspector: deliberate close"),
        (
            Pressed,
            false,
            true,
            false,
            "source: focus restored with F12 held",
        ),
        (Released, false, false, false, "source: real release"),
        (Pressed, false, false, true, "source: deliberate reopen"),
    ] {
        if shortcut_pressed(state, repeat, synthetic) {
            open = !open;
        }
        assert_eq!(open, expected, "{context}");
    }
}

#[test]
fn detached_panel_resize_does_not_change_source_viewport_or_selection() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.select(Some(1));
    inspector.pause_refresh();
    inspector.panel_viewport(520.0, 720.0);
    inspector.constrain(400.0, 300.0);
    assert_eq!((inspector.bounds.w, inspector.bounds.h), (520.0, 720.0));
    assert_eq!(inspector.viewport, (400.0, 300.0));
    inspector.panel_viewport(700.0, 900.0);
    assert_eq!(inspector.viewport, (400.0, 300.0));
    inspector.panel_event(
        &WindowEvent::Resized(winit::dpi::PhysicalSize::new(1400, 1800)),
        2.0,
        ModifiersState::empty(),
    );
    assert!(!inspector.stale);
    assert_eq!(inspector.highlighted_record().unwrap().0, 1);
}

#[test]
fn detached_pick_uses_source_dpi_and_zoom_and_consumes_both_click_edges() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(520.0, 720.0);
    inspector.snapshot = scene_snapshot(1);
    let moved = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(160.0, 120.0),
    };
    assert!(inspector.source_event(&moved, 2.0, 2.0).consumed);
    assert_eq!(inspector.selected, Some(1));
    assert!(
        inspector
            .source_event(&mouse(ElementState::Pressed), 2.0, 2.0)
            .consumed
    );
    assert!(!inspector.capture_live());
    assert!(
        inspector
            .source_event(&mouse(ElementState::Released), 2.0, 2.0)
            .consumed
    );
    inspector.panel_event(&moved, 1.0, ModifiersState::empty());
    assert_eq!(inspector.highlighted_record().unwrap().0, 1);
    assert!(
        !inspector
            .source_event(&mouse(ElementState::Pressed), 2.0, 2.0)
            .consumed
    );
}

#[test]
fn panel_pointer_cannot_repick_source_and_native_drag_keeps_client_origin() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(520.0, 720.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.select(Some(1));
    inspector.panel_event(
        &WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(18.0, 15.0),
        },
        1.0,
        ModifiersState::empty(),
    );
    inspector.accept(scene_snapshot(2));
    assert_eq!(inspector.snapshot.revision, 1);
    assert_eq!(inspector.selected, Some(1));
    let press = inspector.panel_event(&mouse(ElementState::Pressed), 1.0, ModifiersState::empty());
    assert!(press.drag_window);
    assert!(inspector.drag.is_none());
    assert_eq!((inspector.bounds.x, inspector.bounds.y), (0.0, 0.0));
}

#[test]
fn pin_is_a_named_keyboard_reachable_accessible_toggle() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(520.0, 720.0);
    let slot = WindowSlot::from_raw(17);
    let part = ControlPart::new("inspect.pin");
    assert!(inspector.focus_order().contains(&PIN));
    assert!(!inspector.pinned);
    assert_eq!(
        inspector.accessibility_action(part, Action::Click),
        Some(false)
    );
    assert!(inspector.pinned);
    let nodes = inspector.semantics(slot, 1.0);
    let pin = nodes.iter().find(|n| n.occurrence.part == part).unwrap();
    assert_eq!(pin.role, Role::Button);
    assert_eq!(pin.toggled, Some(accesskit::Toggled::True));
    assert!(pin.label.as_ref().is_some_and(|s| !s.is_empty()));
    inspector.activate(PIN);
    assert!(!inspector.pinned);
}

#[test]
fn selection_hint_only_uses_a_slash_for_a_distinct_component_owner() {
    let mut snapshot = scene_snapshot(1);
    assert_eq!(selection_name(&snapshot, 0), "KoruButton");
    assert_eq!(selection_name(&snapshot, 1), "KoruButton / label");
    snapshot.records[0].kind = "container";
    snapshot.records[1].name = "text run";
    assert_eq!(selection_name(&snapshot, 1), "Text");
}

#[test]
fn dragging_only_requests_position_storage_when_the_move_finishes() {
    let mut inspector = Inspector::new(1400.0, 900.0);
    inspector.pointer = (inspector.bounds.x + 5.0, inspector.bounds.y + 7.0);
    let pressed = inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(!pressed.placement_changed);
    let moved = inspector.event(
        &WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(445.0, 125.0),
        },
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(!moved.placement_changed);
    let released = inspector.event(
        &mouse(ElementState::Released),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(released.placement_changed);
    assert_eq!((inspector.bounds.x, inspector.bounds.y), (440.0, 118.0));
}

fn scene_snapshot(revision: u64) -> Snapshot {
    let frame = inspect::Frame::begin(
        revision,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    {
        let _component = inspect::scope(
            "KoruButton",
            "component",
            Affine::IDENTITY,
            scene::rect(20.0, 20.0, 100.0, 32.0),
            |_| {},
        );
        inspect::leaf(
            "label",
            Affine::IDENTITY,
            scene::rect(30.0, 24.0, 60.0, 20.0),
            |_| {},
        );
    }
    frame.finish()
}

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

#[test]
fn picking_consumes_both_halves_of_click_and_pins_without_app_activation() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.pointer = (40.0, 30.0);
    inspector.accept(scene_snapshot(1));
    assert_eq!(inspector.selected, Some(1));
    let result = inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(result.consumed);
    assert!(!inspector.picking);
    assert!(
        inspector
            .event(
                &mouse(ElementState::Released),
                1.0,
                1.0,
                ModifiersState::empty()
            )
            .consumed
    );
    assert_eq!(inspector.selected, Some(1));
    assert!(
        inspector.engine.is_none(),
        "input must not load fonts or shape text"
    );
}

#[test]
fn ordinary_app_click_retires_pinned_selection_and_passes_through() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    inspector.select(Some(1));
    inspector.picking = false;
    inspector.pointer = (40.0, 30.0);
    let result = inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(!result.consumed);
    assert!(inspector.stale);
    assert_eq!(inspector.selected, None);
}

#[test]
fn domain_revision_retires_a_reused_visual_slot() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.observe_view_revision(1);
    inspector.accept(scene_snapshot(1));
    inspector.select(Some(1));
    inspector.picking = false;
    inspector.observe_view_revision(2);
    inspector.accept(scene_snapshot(2));
    assert!(inspector.stale);
    assert_eq!(inspector.selected, None);
}

#[test]
fn hierarchy_navigation_selects_parent_and_child() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    inspector.select(Some(1));
    inspector.navigate(NamedKey::ArrowLeft);
    assert_eq!(inspector.selected, Some(0));
    inspector.navigate(NamedKey::ArrowRight);
    assert_eq!(inspector.selected, Some(1));
}

#[test]
fn clicking_a_tree_row_updates_target_and_details_without_a_host_callback() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let mut snapshot = scene_snapshot(1);
    snapshot.records[1].property("font size", 18, "rendered");
    inspector.accept(snapshot);
    inspector.select(Some(0));
    inspector.activate(8);
    let body = inspector.control_rect(4);
    inspector.pointer = (body.x + 8.0, body.y + ROW + 5.0);
    let result = inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(result.consumed && result.redraw);
    assert_eq!(inspector.selected, Some(1));
    assert!(!inspector.stale);
    assert_eq!(
        inspector.snapshot.records[inspector.selected.unwrap()].bounds,
        [30.0, 24.0, 90.0, 44.0]
    );
    assert!(
        inspector
            .lines
            .iter()
            .any(|line| line == "Font size: 18 px (logical)")
    );
    assert!(
        inspector
            .hierarchy_description()
            .contains("label (primitive) — selected")
    );
    assert!(inspector.refresh_paused && inspector.engine.is_none());
}

#[test]
fn pinned_highlight_survives_wakes_revision_changes_and_pointer_motion() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.pointer = (40.0, 30.0);
    inspector.accept(scene_snapshot(1));
    inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    inspector.event(
        &mouse(ElementState::Released),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    inspector.observe_view_revision(1);
    inspector.host_changed();
    inspector.observe_view_revision(2);
    inspector.event(
        &WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(180.0, 150.0),
        },
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(inspector.stale && inspector.refresh_paused);
    let (index, record) = inspector
        .highlighted_record()
        .expect("pinned selection stays painted");
    assert_eq!(index, 1);
    assert_eq!(record.bounds, [30.0, 24.0, 90.0, 44.0]);
}

#[test]
fn hierarchy_click_keeps_a_highlight_even_after_the_snapshot_becomes_stale() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.pointer = (40.0, 30.0);
    inspector.accept(scene_snapshot(1));
    inspector.activate(8);
    inspector.host_changed();
    let body = inspector.control_rect(4);
    inspector.pointer = (body.x + 8.0, body.y + 5.0);
    inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    let (index, record) = inspector
        .highlighted_record()
        .expect("tree selection stays painted");
    assert_eq!(index, 0);
    assert_eq!(record.bounds, [20.0, 20.0, 120.0, 52.0]);
    inspector.host_changed();
    assert_eq!(inspector.highlighted_record().unwrap().0, 0);
}

#[test]
fn app_pick_reveals_the_tree_row_and_refreshes_properties_at_the_same_index() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let mut snapshot = scene_snapshot(1);
    let mut text = snapshot.records[1].clone();
    text.bounds = [200.0, 200.0, 230.0, 220.0];
    text.properties.clear();
    snapshot
        .records
        .extend(std::iter::repeat_n(text.clone(), 50));
    snapshot.records[51].property("font size", 18, "rendered");
    inspector.accept(snapshot.clone());
    inspector.activate(8);
    inspector.resume_refresh();
    inspector.pointer = (210.0, 210.0);
    // Clicking pins what is under the pointer, even after tree navigation.
    inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert_eq!(inspector.selected, Some(51));
    let visible = (inspector.control_rect(4).h / ROW).floor() as usize;
    assert!(51 >= inspector.hierarchy_scroll && 51 < inspector.hierarchy_scroll + visible);
    assert!(
        inspector
            .lines
            .iter()
            .any(|line| line == "Font size: 18 px (logical)")
    );
    inspector.resume_refresh();
    snapshot.records[51].properties[0].value = "24".into();
    inspector.accept(snapshot);
    assert_eq!(inspector.selected, Some(51));
    assert!(
        inspector
            .lines
            .iter()
            .any(|line| line == "Font size: 24 px (logical)")
    );
}

#[test]
fn phosphor_hint_and_details_name_the_recorded_icon_and_respect_boundaries() {
    let mut snapshot = scene_snapshot(1);
    snapshot.records[0].name = "KoruIconButton";
    snapshot.records[0].property("icon name", "PLUS", "icons::ALL");
    snapshot.records[1].name = "text run";
    snapshot.records[1].property("rendered family", "Phosphor", "rendered");
    assert_eq!(selection_name(&snapshot, 1), "KoruIconButton / PLUS");
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(snapshot.clone());
    inspector.select(Some(1));
    assert_eq!(inspector.lines[0], "Phosphor icon: PLUS");
    snapshot.records[1].kind = "unregistered";
    assert_eq!(icon_name(&snapshot, 1), None);
    assert_eq!(selection_name(&snapshot, 1), "Phosphor icon");
}

#[test]
fn panel_remains_reachable_after_resize() {
    let mut inspector = Inspector::new(1400.0, 1000.0);
    inspector.bounds.x = 1200.0;
    inspector.bounds.y = 900.0;
    inspector.constrain(720.0, 600.0);
    assert!(inspector.bounds.x + inspector.bounds.w <= 720.0);
    assert!(inspector.bounds.y + inspector.bounds.h <= 600.0);
    assert!(inspector.bounds.x >= 0.0 && inspector.bounds.y >= 0.0);
}

#[test]
fn changed_ancestor_retires_identical_child() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    inspector.select(Some(1));
    inspector.picking = false;
    let mut next = scene_snapshot(2);
    next.records[0].name = "Different owner";
    inspector.accept(next);
    assert!(inspector.stale);
    assert_eq!(inspector.selected, None);
}

#[test]
fn physical_pointer_is_converted_by_both_dpi_and_zoom() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    let event = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(120.0, 90.0),
    };
    inspector.event(&event, 2.0, 1.5, ModifiersState::empty());
    assert_eq!(inspector.pointer, (40.0, 30.0));
    assert_eq!(inspector.selected, Some(1));
}

#[test]
fn unknown_and_explicitly_unregistered_are_distinct() {
    let mut snapshot = scene_snapshot(1);
    snapshot.records[0].kind = "container";
    assert!(!is_unregistered(&snapshot, 1));
    assert_eq!(snapshot.owner(1), None);
    snapshot.records[0].kind = "unregistered";
    assert!(is_unregistered(&snapshot, 1));
    assert_eq!(snapshot.owner(1), None);
}

#[test]
fn paused_control_retains_keyboard_focus_until_app_navigation() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.activate(0);
    assert!(!inspector.picking);
    assert!(inspector.focus);
}

#[test]
fn pause_keeps_snapshot_selection_and_details_during_redraws_and_document_changes() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.pointer = (40.0, 30.0);
    inspector.accept(scene_snapshot(1));
    let lines = inspector.lines.clone();
    inspector.activate(PICK);
    assert!(!inspector.picking && inspector.refresh_paused);
    inspector.host_changed();
    inspector.accept(Snapshot::default());
    assert_eq!(inspector.selected, Some(1));
    assert_eq!(inspector.snapshot.revision, 1);
    assert_eq!(inspector.lines, lines);
    assert!(
        inspector.stale,
        "the retained highlight still describes a saved snapshot"
    );
    inspector.activate(PICK);
    inspector.accept(Snapshot::default());
    assert!(inspector.selected.is_none());
}

#[test]
fn resume_keeps_the_previous_target_until_the_pointer_leaves_the_panel() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.pointer = (40.0, 30.0);
    inspector.accept(scene_snapshot(1));
    inspector.pause_refresh();
    inspector.pointer = (inspector.bounds.x + PAD, inspector.bounds.y + PAD);
    inspector.resume_refresh();
    inspector.accept(Snapshot::default());
    assert_eq!(inspector.selected, Some(1));
    assert_eq!(inspector.snapshot.revision, 1);
    inspector.pointer = (0.0, 0.0);
    inspector.accept(Snapshot::default());
    assert_eq!(inspector.selected, None);
}

#[test]
fn hierarchy_has_all_recorded_levels_and_freezes_when_opened() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let mut snapshot = scene_snapshot(1);
    for index in 2..12 {
        let mut record = snapshot.records[1].clone();
        record.parent = Some(index - 1);
        snapshot.records.push(record);
    }
    inspector.accept(snapshot);
    inspector.select(Some(11));
    inspector.activate(8);
    assert!(inspector.refresh_paused);
    assert_eq!(
        inspector
            .hierarchy_rows()
            .into_iter()
            .map(|row| (row.index, row.depth))
            .collect::<Vec<_>>(),
        (0..12).map(|i| (i, i)).collect::<Vec<_>>()
    );
    assert!(inspector.focus_order().contains(&REFRESH));
    assert!(!inspector.focus_order().contains(&5));
    inspector.navigate(NamedKey::ArrowLeft);
    assert_eq!(inspector.selected, Some(10));
    inspector.accept(Snapshot::default());
    assert_eq!(inspector.selected, Some(10));
    inspector.activate(7);
    assert!(!inspector.focus_order().contains(&REFRESH));
    assert!(inspector.focus_order().contains(&5));
}

#[test]
fn hierarchy_collapse_hides_descendants_and_navigation_uses_visible_rows() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let mut snapshot = scene_snapshot(1);
    let mut grandchild = snapshot.records[1].clone();
    grandchild.parent = Some(1);
    snapshot.records.push(grandchild);
    let mut sibling = snapshot.records[1].clone();
    sibling.parent = Some(0);
    snapshot.records.push(sibling);
    inspector.accept(snapshot);
    inspector.select(Some(1));

    inspector.navigate(NamedKey::ArrowLeft);
    assert_eq!(inspector.selected, Some(1));
    assert!(inspector.collapsed_hierarchy.contains(&1));
    assert_eq!(
        inspector
            .hierarchy_rows()
            .into_iter()
            .map(|row| row.index)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    inspector.navigate(NamedKey::ArrowDown);
    assert_eq!(inspector.selected, Some(3));

    inspector.select(Some(1));
    inspector.navigate(NamedKey::ArrowRight);
    assert!(!inspector.collapsed_hierarchy.contains(&1));
    inspector.navigate(NamedKey::ArrowRight);
    assert_eq!(inspector.selected, Some(2));
}

#[test]
fn selecting_a_hidden_descendant_expands_its_ancestors() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let mut snapshot = scene_snapshot(1);
    let mut grandchild = snapshot.records[1].clone();
    grandchild.parent = Some(1);
    snapshot.records.push(grandchild);
    inspector.accept(snapshot);
    inspector.collapsed_hierarchy.insert(0);
    inspector.collapsed_hierarchy.insert(1);

    inspector.select(Some(2));

    assert!(inspector.collapsed_hierarchy.is_empty());
    assert!(inspector.hierarchy_rows().iter().any(|row| row.index == 2));
}

#[test]
fn clicking_a_disclosure_caret_collapses_without_opening_details() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    inspector.activate(8);
    let tree = inspector.control_rect(4);
    inspector.pointer = (inspector.bounds.x + PAD + 8.0, tree.y + ROW * 0.5);

    let result = inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );

    assert!(result.consumed && result.redraw);
    assert!(inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(0));
    assert_eq!(inspector.hierarchy_rows().len(), 1);
    assert!(inspector.hierarchy_click.is_none());
}

#[test]
fn close_icon_is_not_a_title_drag_and_has_an_accessible_button() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let close = inspector.control_rect(CLOSE);
    inspector.pointer = (close.x + 14.0, close.y + 14.0);
    let result = inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(result.close && result.consumed);
    assert!(inspector.drag.is_none());
    let nodes = inspector.semantics(WindowSlot::from_raw(1), 1.0);
    let close = nodes
        .iter()
        .find(|n| n.occurrence.part == ControlPart::new("inspect.close"))
        .unwrap();
    assert_eq!(close.role, Role::Button);
    assert!(close.actions.contains(&Action::Click));
    assert!(close.label.is_some());
    assert!(
        !nodes
            .iter()
            .any(|n| n.occurrence.part == ControlPart::new("inspect.fixture"))
    );
}

#[test]
fn picking_and_panel_use_arrow_cursor_but_paused_app_keeps_its_own_cursor() {
    let mut inspector = Inspector::new(900.0, 700.0);
    assert!(inspector.wants_arrow_cursor());
    inspector.pause_refresh();
    assert!(!inspector.wants_arrow_cursor());
    inspector.pointer = (inspector.bounds.x + PAD, inspector.bounds.y + PAD);
    assert!(inspector.wants_arrow_cursor());
}

#[test]
fn hint_stays_in_view_and_outside_the_target_at_every_edge() {
    for target in [
        scene::rect(10.0, 0.0, 100.0, 20.0),
        scene::rect(10.0, 680.0, 100.0, 20.0),
        scene::rect(0.0, 0.0, 100.0, 700.0),
        scene::rect(700.0, 0.0, 100.0, 700.0),
    ] {
        let hint = hint_bounds(target, (900.0, 700.0), 440.0, 48.0).unwrap();
        let rect = scene::rect(hint.x, hint.y, hint.w, hint.h);
        assert!(rect.intersect(target).area() <= 0.0);
        assert!(rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= 900.0 && rect.y1 <= 700.0);
    }
    let target = scene::rect(0.0, 0.0, 900.0, 700.0);
    assert!(hint_bounds(target, (900.0, 700.0), 440.0, 48.0).is_none());
}

#[test]
fn text_details_lead_with_actual_family_weight_style_and_size() {
    let mut snapshot = scene_snapshot(1);
    let record = &mut snapshot.records[1];
    record.property("rendered family", "Lato", "rendered");
    record.property("font weight", 700, "rendered");
    record.property("font style", "Italic", "rendered");
    record.property("font size", 18, "rendered");
    assert_eq!(
        font_hint(record).as_deref(),
        Some("Lato · 700 Bold · Italic · 18.0 px")
    );
    let details = typography_details(&snapshot, 1);
    assert_eq!(
        &details[..4],
        &[
            "Font family: Lato",
            "Font weight: 700 Bold",
            "Font style: Italic",
            "Font size: 18 px (logical)"
        ]
    );
    assert!(details.iter().any(|line| line == "Text role: not exposed"));
}

#[test]
fn scrollbar_drag_reaches_both_ends_without_selecting_a_hierarchy_row() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let mut snapshot = scene_snapshot(1);
    let record = snapshot.records[1].clone();
    snapshot.records.extend(std::iter::repeat_n(record, 80));
    inspector.accept(snapshot);
    inspector.select(Some(1));
    inspector.activate(8);
    let bar = inspector.scrollbar().unwrap();
    inspector.pointer = (bar.gutter_x + 1.0, bar.thumb_top + 1.0);
    inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert!(inspector.scrollbar_drag.is_some());
    inspector.pointer.1 = bar.track_bottom;
    inspector.scroll_to_pointer(0.0);
    assert!(inspector.hierarchy_scroll > 50);
    assert_eq!(inspector.selected, Some(1));
    inspector.pointer.1 = bar.track_top;
    inspector.scroll_to_pointer(0.0);
    assert_eq!(inspector.hierarchy_scroll, 0);
}

#[test]
fn ime_commit_is_consumed_while_inspector_owns_input() {
    let mut inspector = Inspector::new(900.0, 700.0);
    let event = WindowEvent::Ime(winit::event::Ime::Commit("synthetic".into()));
    assert!(
        inspector
            .event(&event, 1.0, 1.0, ModifiersState::empty())
            .consumed
    );
    inspector.focus = false;
    inspector.picking = false;
    assert!(
        !inspector
            .event(&event, 1.0, 1.0, ModifiersState::empty())
            .consumed
    );
}

#[test]
fn truncated_frame_retires_an_existing_pin() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    inspector.select(Some(1));
    inspector.picking = false;
    let mut snapshot = scene_snapshot(2);
    snapshot.truncated = true;
    inspector.accept(snapshot);
    assert_eq!(inspector.selected, None);
    assert!(inspector.stale);
}

#[test]
fn inspector_semantic_bounds_apply_app_zoom_once() {
    let inspector = Inspector::new(900.0, 700.0);
    let nodes = inspector.semantics(WindowSlot::from_raw(1), 1.5);
    let rect = inspector.control_rect(0);
    let bounds = nodes[0].bounds.unwrap();
    assert_eq!(bounds.x, rect.x * 1.5);
    assert_eq!(bounds.w, rect.w * 1.5);
    assert!(nodes[0].actions.contains(&Action::Click));
    assert!(
        nodes
            .iter()
            .all(|node| node.actions.contains(&Action::Focus))
    );
}

#[test]
fn accessibility_sampling_omits_label_values_and_retires_on_host_change() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.accept(scene_snapshot(1));
    inspector.select(Some(1));
    inspector.picking = false;
    let node = SemanticNode::new(
        Occurrence::window_command(WindowSlot::from_raw(1), ControlPart::new("fixture")),
        Role::Button,
    )
    .with_label("Private synthetic label")
    .with_bounds(Rect::new(20.0, 20.0, 100.0, 32.0));
    inspector.observe_semantics(&[node]);
    let details = inspector.semantic_details.join(" ");
    assert!(details.contains("Geometric candidate"));
    assert!(details.contains("role=Button"));
    assert!(!details.contains("Private synthetic label"));
    inspector.host_changed();
    assert!(inspector.selected.is_none());
    assert!(inspector.semantic_candidates.is_empty());
    assert!(inspector.stale);
}

/// F12 then a pick lands on the tree, with the picked element selected in
/// it, rather than on Details.
#[test]
fn a_new_inspector_opens_on_the_hierarchy_and_a_pick_keeps_it_there() {
    let mut inspector = Inspector::new(900.0, 700.0);
    assert!(inspector.show_hierarchy);
    let mut snapshot = scene_snapshot(1);
    snapshot.records[1].bounds = [200.0, 200.0, 230.0, 220.0];
    inspector.accept(snapshot);
    inspector.resume_refresh();
    inspector.pointer = (210.0, 210.0);
    inspector.event(
        &mouse(ElementState::Pressed),
        1.0,
        1.0,
        ModifiersState::empty(),
    );
    assert_eq!(inspector.selected, Some(1));
    assert!(inspector.show_hierarchy);
}

fn thumb(button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Pressed,
        button,
    }
}

/// **The mouse's thumb buttons walk the panel's tabs.** A double-click opens
/// Details; Back returns to the Hierarchy with the same row selected, and
/// Forward returns to Details.
#[test]
fn mouse_back_and_forward_walk_between_hierarchy_and_details() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.panel_viewport(520.0, 720.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.activate(8);
    let now = std::time::Instant::now();
    inspector.click_hierarchy_row(1, now);
    assert!(inspector.click_hierarchy_row(1, now + std::time::Duration::from_millis(100)));
    assert!(!inspector.show_hierarchy);

    inspector.pointer = (PAD + 20.0, DETAILS_TOP + ROW);
    let back = inspector.panel_event(&thumb(MouseButton::Back), 1.0, ModifiersState::empty());
    assert!(back.consumed && back.redraw && back.structure_changed);
    assert!(inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(1));
    assert_eq!(inspector.focus_index, 8);

    inspector.panel_event(&thumb(MouseButton::Forward), 1.0, ModifiersState::empty());
    assert!(!inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(1));

    // Nothing further ahead: Forward is a no-op, not a toggle.
    let nowhere = inspector.panel_event(&thumb(MouseButton::Forward), 1.0, ModifiersState::empty());
    assert!(nowhere.consumed && !nowhere.redraw);
    assert!(!inspector.show_hierarchy);
}

/// **Each view keeps its own selection, and a new step drops what was ahead.**
#[test]
fn history_restores_each_views_selection_and_a_new_step_clears_forward() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.activate(8);
    inspector.select(Some(0));
    inspector.activate(7); // Details of 0.
    inspector.activate(8);
    inspector.select(Some(1));
    inspector.activate(7); // Details of 1.

    assert!(inspector.go(true));
    assert!(inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(1));
    assert!(inspector.go(true));
    assert!(!inspector.show_hierarchy);
    assert_eq!(
        inspector.selected,
        Some(0),
        "Details of the first row again"
    );
    assert!(inspector.go(true));
    assert!(inspector.show_hierarchy);
    assert!(!inspector.go(true), "the start of the history");

    assert!(inspector.go(false));
    assert!(!inspector.show_hierarchy);
    // A tab chosen here is a new step: what was ahead is gone.
    inspector.activate(8);
    assert!(!inspector.go(false));
}

/// **A record index belongs to its capture.** Stepping back across a refresh
/// returns to the tab but keeps today's selection.
#[test]
fn history_keeps_the_current_selection_across_a_newer_capture() {
    let mut inspector = Inspector::new(900.0, 700.0);
    inspector.snapshot = scene_snapshot(1);
    inspector.activate(8);
    inspector.select(Some(0));
    inspector.activate(7);
    inspector.snapshot = scene_snapshot(2);
    inspector.select(Some(1));
    assert!(inspector.go(true));
    assert!(inspector.show_hierarchy);
    assert_eq!(inspector.selected, Some(1));
}
