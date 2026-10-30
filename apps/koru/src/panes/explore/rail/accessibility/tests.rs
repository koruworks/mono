// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::ExploreView;
use koru_core::ids::ListId;
use koru_i18n::Lang;
use koru_workspace_host::accessibility::{ViewSlot, WindowSlot};

fn cx(lang: Lang) -> Context {
    Context {
        window: WindowSlot::from_raw(7),
        view: ViewSlot::from_raw(9),
        area: Rect::new(0.0, 0.0, 260.0, 600.0),
        rail: crate::panes::explore::ExploreView::new().rail_of(Rect::new(0.0, 0.0, 260.0, 600.0)),
        viewport: (1000.0, 600.0),
        lang,
    }
}

#[test]
fn five_queue_ids_and_labels_are_stable_in_every_language() {
    let rail = TasksRail::default();
    let english = rail.queue_semantics(&cx(Lang::En), 100.0);
    assert_eq!(english.size_of_set, Some(5));
    for lang in Lang::ALL {
        let tree = rail.queue_semantics(&cx(lang), 100.0);
        assert_eq!(tree.children.len(), 5);
        for (i, (node, bucket)) in tree.children.iter().zip(ALL).enumerate() {
            assert_eq!(node.occurrence, english.children[i].occurrence);
            assert_eq!(node.label.as_deref(), Some(t(lang, label(bucket))));
            assert_eq!(node.position, Some(i));
            assert_eq!(node.role, Role::Tab);
            assert!(node.actions.contains(&Action::Click));
            assert!(node.actions.contains(&Action::Focus));
        }
    }
    assert_eq!(
        occurrence(&cx(Lang::En), Bucket::Later).locator(),
        "koru:w7/v9/s:tasks-queue#later"
    );
}

#[test]
fn queue_geometry_and_activation_match_the_existing_pointer_path() {
    let context = cx(Lang::En);
    for bucket in ALL {
        let mut rail = TasksRail::default();
        let (_, bounds) = rail
            .rows(context.area, 100.0)
            .into_iter()
            .find(|(row, _)| *row == RailRow::Bucket(bucket))
            .unwrap();
        let projected = rail.queue_semantics(&context, 100.0);
        let node = projected
            .children
            .iter()
            .find(|node| node.occurrence == occurrence(&context, bucket))
            .unwrap();
        assert_eq!(node.bounds, Some(bounds));
        let expected = rail
            .mouse_down(
                context.area,
                100.0,
                bounds.x + bounds.w / 2.0,
                bounds.y + bounds.h / 2.0,
                false,
            )
            .unwrap();
        let actual = rail.queue_action(context.area, 100.0, node.occurrence, Action::Click);
        assert!(matches!(actual, Some(Outcome::Tasks(request)) if request == expected));
        assert_eq!(rail.selected(), Some(RailTarget::Bucket(bucket)));
        assert!(!rail.has_queue_focus());
    }
}

#[test]
fn keyboard_focus_moves_without_opening_then_enter_uses_the_existing_intent() {
    let context = cx(Lang::En);
    let mut view = ExploreView::default();
    view.show_tasks();
    assert!(view.focus_tree(context.area));
    assert!(view.has_focus());
    for _ in 0..3 {
        assert!(matches!(
            view.nav_key(context.area, &WKey::Named(NamedKey::ArrowDown)),
            Some(Outcome::Consumed)
        ));
    }
    assert_eq!(
        view.accessible_focus(&context),
        Some(occurrence(&context, Bucket::Later))
    );
    assert_eq!(view.rail().selected(), None);
    assert!(matches!(
        view.nav_key(context.area, &WKey::Named(NamedKey::Enter)),
        Some(Outcome::Tasks(TasksRailRequest::OpenBucket {
            bucket: Bucket::Later,
            new_tab: false
        }))
    ));
    assert!(!view.has_focus());
}

#[test]
fn offscreen_completed_queue_can_be_focused_and_revealed_without_pointer() {
    let context = cx(Lang::En);
    let mut rail = TasksRail::default();
    rail.set_lists(
        (0..40)
            .map(|n| super::super::RailList {
                id: ListId::new(),
                name: format!("fixture {n}"),
                color: None,
                count: 0,
                overdue: 0,
            })
            .collect(),
    );
    assert!(
        !rail
            .rows(context.area, 100.0)
            .iter()
            .any(|(row, _)| *row == RailRow::Bucket(Bucket::Completed))
    );
    let target = occurrence(&context, Bucket::Completed);
    assert!(
        rail.queue_semantics(&context, 100.0)
            .children
            .iter()
            .any(|node| node.occurrence == target)
    );
    assert!(matches!(
        rail.queue_action(context.area, 100.0, target, Action::Focus),
        Some(Outcome::Consumed)
    ));
    assert!(
        rail.rows(context.area, 100.0)
            .iter()
            .any(|(row, _)| *row == RailRow::Bucket(Bucket::Completed))
    );
    assert_eq!(rail.queue_focus_occurrence(&context), Some(target));
    assert_eq!(rail.selected(), None);
}

fn contains(node: &SemanticNode, target: Occurrence) -> bool {
    node.occurrence == target || node.children.iter().any(|node| contains(node, target))
}

#[test]
fn changing_mode_retires_queue_controls_and_refuses_old_actions() {
    let context = cx(Lang::En);
    let mut view = ExploreView::default();
    let target = occurrence(&context, Bucket::Later);
    view.show_tasks();
    assert!(contains(&view.semantics(&context), target));
    assert!(!contains(
        &view.semantics(&context),
        Occurrence::surface(
            context.window,
            context.view,
            "explore",
            ControlPart::new("tree")
        )
    ));
    view.focus_tree(context.area);
    view.show_documents();
    assert!(!view.has_focus());
    assert!(!contains(&view.semantics(&context), target));
    assert!(
        view.accessibility_action(context.area, target, Action::Click, context.viewport)
            .is_none()
    );
}

#[test]
fn hidden_document_rows_cannot_be_activated_from_tasks_mode() {
    use koru_core::{
        DocumentId,
        explore::{ExploreEntry, KIND_DOCUMENT},
    };
    let context = cx(Lang::En);
    let id = DocumentId::new();
    let mut view = ExploreView::default();
    view.rebuild_from_entries(&[ExploreEntry {
        id,
        parent: None,
        ordinal: 0,
        title: "synthetic document".into(),
        kind: KIND_DOCUMENT.into(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    }]);
    let target = Occurrence::model(
        context.window,
        context.view,
        id.as_uuid(),
        ControlPart::ROOT,
    );
    assert!(contains(&view.semantics(&context), target));
    view.show_tasks();
    assert!(!contains(&view.semantics(&context), target));
    for action in [Action::Click, Action::Focus, Action::ScrollIntoView] {
        assert!(
            view.accessibility_action(context.area, target, action, context.viewport)
                .is_none()
        );
    }
}

#[test]
fn tab_and_escape_return_keyboard_without_opening_a_queue() {
    let context = cx(Lang::En);
    for key in [NamedKey::Tab, NamedKey::Escape] {
        let mut view = ExploreView::default();
        view.show_tasks();
        view.focus_tree(context.area);
        assert!(matches!(
            view.nav_key(context.area, &WKey::Named(key)),
            Some(Outcome::Consumed)
        ));
        assert!(!view.has_focus());
        assert_eq!(view.rail().selected(), None);
    }
}
