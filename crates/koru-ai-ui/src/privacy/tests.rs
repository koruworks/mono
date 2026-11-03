// SPDX-License-Identifier: MPL-2.0

//! The workspace-access panel on elements (koru-ai D106).

use koru_ai_core::{AiProviderId, ModelSelection, ProfileId, ReasoningChoice};
use koru_draw::inspect;
use koru_i18n::Lang;
use vello::Scene;
use vello::kurbo::Affine;

use super::*;
use crate::reader::DestinationOption;
use crate::{ChatSurface, SurfaceMode};

/// A surface on an OpenAI chat that may reach the second of `count`
/// workspaces, the access panel open.
fn open_panel(count: usize) -> ChatSurface {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.lang = Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    let mut transcript = crate::unmade_transcript(ModelSelection {
        provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
        requested_model: "gpt-test".into(),
        default_reasoning: ReasoningChoice::Default,
    });
    surface.destinations = (0..count)
        .map(|index| DestinationOption {
            profile_id: ProfileId::new(),
            workspace_id: WorkspaceId::new(),
            label: format!("Workspace {index}"),
        })
        .collect();
    if let Some(second) = surface.destinations.get(1) {
        transcript.tool_policy.workspace_ids = vec![second.workspace_id];
    }
    surface.transcript = Some(transcript);
    surface.composer_rect();
    surface.tool_access_open = true;
    surface
}

fn near(a: f32, b: f32, what: &str) {
    assert!((a - b).abs() < 0.01, "{what}: {a}, not {b}");
}

/// The frame node at `index`'s first descendant named `name`.
fn descendant<'a>(
    frame: &'a koru_draw::el::Frame,
    index: usize,
    name: &str,
) -> Option<&'a koru_draw::el::Node> {
    let under = |mut at: usize| loop {
        match frame.nodes[at].parent {
            Some(parent) if parent == index => return true,
            Some(parent) => at = parent,
            None => return false,
        }
    };
    frame
        .nodes
        .iter()
        .enumerate()
        .find(|(at, node)| node.name == name && under(*at))
        .map(|(_, node)| node)
}

/// **Every part sits where the arithmetic put it** (D106): the heading 10
/// in and 9 down, the filter 8 in and 31 down, 34 high; the list 4 in, 8
/// under the filter (or 31 down without one), a row 32 high, its name 8 in
/// and elided 30 short of its end, the ✓ 8 short of it and in its middle;
/// the panel the heading, the filter, the rows and 4 under them.
#[test]
fn every_part_of_the_panel_sits_where_the_arithmetic_put_it() {
    for (count, filtered) in [(7, true), (5, false)] {
        let surface = open_panel(count);
        let panel = surface.tool_access_panel().expect("open");
        let rows = surface.tool_access_visible_rows();
        let (_, _, frame) = surface.tool_access_frame().expect("open");
        let fixed = if filtered { 35.0 + 34.0 + 8.0 } else { 35.0 };
        near(
            panel.h,
            fixed + rows as f32 * TOOL_ACCESS_WORKSPACE_ROW_H,
            "panel height",
        );
        let words = frame.rect_of(elements::HEADING_WORDS).unwrap();
        near(words.x, panel.x + 10.0, "heading x");
        near(words.y, panel.y + 9.0, "heading y");
        let count_words = frame.rect_of(elements::COUNT).unwrap();
        near(count_words.y, panel.y + 9.0, "count y");
        assert!(count_words.x > words.x + words.w, "the count follows");
        let list_y = if filtered {
            let filter = surface.tool_access_search_rect().expect("a filter");
            near(filter.x, panel.x + 8.0, "filter x");
            near(filter.y, panel.y + 31.0, "filter y");
            near(filter.w, panel.w - 16.0, "filter w");
            near(filter.h, 34.0, "filter h");
            panel.y + 73.0
        } else {
            assert!(surface.tool_access_search_rect().is_none());
            panel.y + 31.0
        };
        let list = frame.node(elements::LIST_KEY).unwrap();
        let view = list.viewport.unwrap();
        near(view.x, panel.x + 4.0, "list x");
        near(view.y, list_y, "list y");
        near(view.w, panel.w - 8.0, "list w");
        near(view.h, rows as f32 * 32.0, "list h");
        let row_at: Vec<usize> = frame
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.name == elements::ROW)
            .map(|(at, _)| at)
            .collect();
        assert_eq!(row_at.len(), count, "a row a workspace");
        for (index, at) in row_at.iter().take(rows).enumerate() {
            let rect = frame.nodes[*at].rect;
            let what = |part: &str| format!("{count}: row {index} {part}");
            near(rect.x, panel.x + 4.0, &what("x"));
            near(rect.y, list_y + index as f32 * 32.0, &what("y"));
            near(rect.w, panel.w - 8.0, &what("w"));
            near(rect.h, 32.0, &what("h"));
            let name = descendant(&frame, *at, elements::ROW_NAME).unwrap().rect;
            near(name.x, rect.x + 8.0, &what("name x"));
            near(name.x + name.w, rect.x + rect.w - 30.0, &what("name end"));
            let check = descendant(&frame, *at, elements::CHECK);
            assert_eq!(check.is_some(), index == 1, "{}", what("✓"));
            if let Some(check) = check {
                let check = check.rect;
                near(check.w, 14.0, &what("✓ size"));
                near(check.x + check.w, rect.x + rect.w - 8.0, &what("✓ end"));
                near(
                    check.y + check.h * 0.5,
                    rect.y + rect.h * 0.5,
                    &what("✓ middle"),
                );
            }
        }
    }
    // An empty list says so in its place: 6 in and 7 down from it.
    let mut surface = open_panel(7);
    surface
        .workspace_search
        .set_text(&mut surface.engine, "nothing like it");
    let panel = surface.tool_access_panel().unwrap();
    let (_, spec, frame) = surface.tool_access_frame().unwrap();
    assert_eq!(spec.empty.as_deref(), Some("No matching workspaces."));
    let note = frame.rect_of(elements::EMPTY_NOTE).unwrap();
    near(note.x, panel.x + 10.0, "note x");
    near(note.y, panel.y + 73.0 + 7.0, "note y");
    near(note.w, panel.w - 20.0, "note w");
}

/// **Everything drawn inside the panel nests under its `FloatingPanel`**
/// (D106, the user on the inspector: *the same applies for the workspace
/// selection*). Drawn with the inspector on, every record — the shadow and
/// body, the heading, the filter's `KoruTextInput`, each row's plate, name
/// and ✓, the scrollbar — descends from the panel through the element tree.
#[test]
fn everything_drawn_inside_the_panel_nests_under_the_floating_panel() {
    let mut surface = open_panel(9);
    let (_, _, frame) = surface.tool_access_frame().unwrap();
    let first = frame.named(elements::ROW).next().unwrap().rect;
    surface.pointer = (first.x + 20.0, first.y + first.h * 0.5);
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_tool_access_picker(&mut scene, Affine::IDENTITY);
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let named = |name: &str| {
        records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.name == name)
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    let one = |name: &str| {
        let found = named(name);
        assert_eq!(found.len(), 1, "one {name}: {found:?}");
        found[0]
    };
    let under = |mut index: usize, ancestor: usize| loop {
        match records[index].parent {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => index = parent,
            None => return false,
        }
    };
    let panel = one("FloatingPanel");
    assert_eq!(records[panel].parent, None);
    for (index, record) in records.iter().enumerate() {
        if index != panel {
            assert!(
                under(index, panel),
                "{} · {} lies outside the FloatingPanel",
                record.name,
                record.kind
            );
        }
    }
    let tree = one(elements::PANEL);
    assert_eq!(records[tree].parent, Some(panel));
    for part in [elements::HEADING, elements::FILTER_ROW, elements::LIST] {
        assert_eq!(records[one(part)].parent, Some(tree), "{part}");
    }
    let input = one("KoruTextInput");
    assert!(
        under(input, one(elements::FILTER)),
        "the field is the filter's"
    );
    let rows = named(elements::ROW);
    assert!(!rows.is_empty());
    let list = one(elements::LIST);
    assert!(rows.iter().all(|row| under(*row, list)));
    let value = |index: usize, name: &str| {
        records[index]
            .properties
            .iter()
            .find(|property| property.name == name)
            .map(|property| property.value.clone())
            .unwrap_or_default()
    };
    // The pointer's row on the hover plate, the chosen one on the selected.
    assert!(value(rows[0], "fill colour").starts_with("Surface::Hover"));
    assert!(value(rows[1], "fill colour").starts_with("Surface::Selected"));
    assert!(value(rows[2], "fill colour").is_empty());
    assert_eq!(named(elements::CHECK).len(), 1);
}

/// The pointer and a press find the row the frame draws; the list's thumb
/// drags it a whole row at a time and the wheel still steps it.
#[test]
fn a_row_is_found_where_it_is_drawn_and_the_thumb_drags_whole_rows() {
    let mut surface = open_panel(19);
    let rows = surface.tool_access_visible_rows();
    assert!(rows < 19, "the list overflows");
    let (_, _, frame) = surface.tool_access_frame().unwrap();
    let second = frame.named(elements::ROW).nth(1).unwrap().rect;
    assert_eq!(
        surface.tool_access_workspace_at(second.x + 20.0, second.y + 1.0),
        Some(surface.destinations[1].workspace_id)
    );
    let bar = surface.tool_access_scrollbar().expect("a scrollbar");
    let gutter = bar.gutter();
    assert_eq!(
        surface.tool_access_workspace_at(gutter.x + gutter.w * 0.5, gutter.y + 4.0),
        None,
        "the gutter is the bar's"
    );
    // A press at the bottom of the gutter: the list follows the thumb.
    let (x, y) = (gutter.x + gutter.w * 0.5, gutter.y + gutter.h - 1.0);
    assert!(surface.mouse_down(x, y));
    assert!(surface.tool_workspace_drag.is_some());
    assert_eq!(surface.tool_workspace_offset, 19 - rows, "to the end");
    assert!(surface.mouse_move(x, gutter.y));
    assert_eq!(surface.tool_workspace_offset, 0, "back to the top");
    surface.mouse_up();
    assert!(surface.tool_workspace_drag.is_none());
    // After a drag the first row shown is a whole row at the list's top.
    surface.tool_workspace_offset = 3;
    let (_, _, frame) = surface.tool_access_frame().unwrap();
    let view = frame.node(elements::LIST_KEY).unwrap().viewport.unwrap();
    let shown = frame.named(elements::ROW).nth(3).unwrap().rect;
    near(shown.y, view.y, "the fourth row at the top");
    assert_eq!(
        surface.tool_access_workspace_at(shown.x + 20.0, shown.y + 1.0),
        Some(surface.destinations[3].workspace_id)
    );
}

/// The panel's words are translated: its heading, its filter's placeholder
/// and an empty list's note.
#[test]
fn the_panels_words_come_from_the_catalog() {
    let mut surface = open_panel(0);
    surface.lang = Lang::De;
    let (_, spec, _) = surface.tool_access_frame().unwrap();
    assert_eq!(spec.heading, "ZUGRIFF AUF ARBEITSBEREICHE");
    assert_eq!(spec.count, "0/0");
    assert_eq!(
        spec.empty.as_deref(),
        Some("Keine Koru-Arbeitsbereiche verfügbar.")
    );
    surface.workspace_catalog_error = Some("The service did not answer.".into());
    let (_, spec, _) = surface.tool_access_frame().unwrap();
    assert_eq!(spec.empty.as_deref(), Some("The service did not answer."));
}
