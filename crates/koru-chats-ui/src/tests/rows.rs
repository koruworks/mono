// SPDX-License-Identifier: MPL-2.0

//! What the list shows, in what order, and where.

use super::*;
use crate::text::who_answered;
use crate::{Load, Notice, Press};

#[test]
fn chats_are_listed_newest_first_whatever_order_they_arrive_in() {
    let surface = three();
    assert_eq!(titles(&surface), ["Newest", "Middle", "Oldest"]);
}

/// Every chat is a row and nothing heads them: *New chat* is the heading's
/// button now (the CTO's note).
#[test]
fn every_chat_is_a_row_and_nothing_heads_them() {
    let mut surface = three();
    assert_eq!(
        rows_of(&mut surface),
        [Row::Chat(0), Row::Chat(1), Row::Chat(2)]
    );
}

#[test]
fn a_cold_surface_says_it_is_loading_rather_than_empty() {
    let mut surface = ChatsSurface::without_reader();
    assert_eq!(surface.load(), Load::Cold);
    assert_eq!(rows_of(&mut surface), [Row::Notice(Notice::Loading)]);
}

#[test]
fn an_empty_answer_is_the_empty_notice() {
    let mut surface = showing(Vec::new());
    assert_eq!(rows_of(&mut surface), [Row::Notice(Notice::Empty)]);
}

/// **A service that stops answering is not an empty list.** What was on
/// screen stays, under a sentence saying why it may be old.
#[test]
fn an_unavailable_service_keeps_the_rows_and_says_so() {
    let mut surface = three();
    surface.take_listing(Err(crate::gateway::unreachable()));
    assert_eq!(surface.load(), Load::Unavailable);
    assert_eq!(
        rows_of(&mut surface),
        [
            Row::Notice(Notice::Unavailable),
            Row::Chat(0),
            Row::Chat(1),
            Row::Chat(2)
        ]
    );
}

#[test]
fn who_answered_speaks_the_chips_vocabulary() {
    let lang = koru_i18n::Lang::En;
    let agents = [opencode()];
    // An agent by its own name, with the model only when one was picked.
    assert_eq!(
        who_answered(lang, &model("agent-opencode", "default"), &agents),
        "OpenCode"
    );
    assert_eq!(
        who_answered(lang, &model("agent-opencode", "deepseek/v4-pro"), &agents),
        "OpenCode · deepseek/v4-pro"
    );
    // An agent this device no longer lists is still a local agent, never a
    // token.
    assert_eq!(
        who_answered(lang, &model("agent-codex", "default"), &agents),
        "Local agent"
    );
    // OpenAI and Ollama by the model a person chose; Koru by its label.
    assert_eq!(
        who_answered(lang, &model("openai", "gpt-6-astra"), &agents),
        "gpt-6-astra"
    );
    assert_eq!(
        who_answered(lang, &model("ollama", "qwen3:8b"), &agents),
        "qwen3:8b"
    );
    assert_eq!(
        who_answered(lang, &model("koru", "Qwen/Qwen3-8B"), &agents),
        koru_ai_core::AiProviderId::KORU_HOSTED_MODEL_LABEL
    );
}

#[test]
fn a_row_carries_who_answered_and_when() {
    let surface = three();
    let line = crate::text::meta_line(
        koru_i18n::Lang::En,
        &surface.chats()[1],
        surface.agents(),
        NOW,
    );
    assert!(line.starts_with("gpt-6-astra · "), "{line}");
    assert!(line.contains("minute"), "{line}");
}

/// **One clipped rectangle.** A row scrolled above the band is neither
/// returned nor hit, and a press below the band's floor is nobody's.
#[test]
fn the_band_is_the_cull_and_the_hit_test() {
    let many: Vec<_> = (1..=60)
        .map(|n| chat(n, &format!("Chat {n}"), i64::from(n) * 1_000))
        .collect();
    let mut surface = showing(many);
    let area = area();
    surface.scroll_by(area, TOP, -400.0);
    let visible = surface.rows(area, TOP);
    let band = surface.band(area, TOP);
    assert!(!visible.is_empty());
    for (_, rect) in &visible {
        assert!(rect.y + rect.h > band.y && rect.y < band.y + band.h);
    }
    // Above the band — where the heading is — nothing answers.
    assert_eq!(surface.row_at(area, TOP, 50.0, TOP - 5.0), None);
    // Below the floor, nothing answers either.
    assert_eq!(surface.row_at(area, TOP, 50.0, area.h - 2.0), None);
    // Inside it, the row that is drawn there answers.
    let (row, rect) = visible[visible.len() / 2];
    let hit = surface.row_at(area, TOP, 50.0, rect.y + rect.h * 0.5);
    assert_eq!(hit.map(|(row, _)| row), Some(row));
}

/// **A row is the library's row** (the CTO's note): one line at
/// `NAV_ROW_H`, the title at the pad with no lane reserved, and only a
/// private chat's title sits one icon slot in, behind its mark.
#[test]
fn a_row_is_one_line_at_the_pad_and_a_private_one_wears_its_mark() {
    let mut chats = vec![
        chat(1, "Plain", 1_000),
        chat(2, "Secret", 2_000),
        chat(3, "Other", 3_000),
    ];
    chats[1].private = true;
    let mut surface = showing(chats);
    let rows = surface.rows(tall(), TOP);
    let area = tall();
    for (row, rect) in &rows {
        assert_eq!(rect.h, koru_draw::theme::NAV_ROW_H, "{row:?}");
        let parts = surface.row_parts(*row, *rect);
        let Row::Chat(index) = row else { continue };
        if surface.chats()[*index].private {
            assert!(parts.lead.w > 0.0, "a private row has its mark");
            assert_eq!(parts.title.x, parts.lead.x + parts.lead.w + 4.0);
        } else {
            assert_eq!(parts.lead.w, 0.0, "no lane reserved");
            assert_eq!(parts.title.x, area.x + koru_draw::theme::NAV_PAD);
        }
        assert_eq!(
            parts.title.x + parts.title.w,
            area.x + area.w - koru_draw::theme::NAV_PAD,
            "the title runs to the pad"
        );
    }
}

/// **The pick is the library's** (the CTO's note): a plain press picks one
/// and opens it, Ctrl toggles, Shift extends from the anchor, and a
/// right-click on a picked row names the whole pick in list order.
#[test]
fn chats_are_picked_like_documents() {
    let mut surface = three();
    let area = tall();
    let rows = surface.rows(area, TOP);
    let centre = |i: usize| rows[i].1.centre();
    let (x, y) = centre(0);
    surface.mouse_down(area, TOP, x, y);
    assert_eq!(surface.selected(), [id(1)], "the newest is listed first");
    assert_eq!(
        surface.take_wants_open(),
        Some(id(1)),
        "a plain press opens"
    );
    let (x, y) = centre(2);
    surface.mouse_down_with(area, TOP, x, y, true, false);
    assert_eq!(
        surface.selected(),
        [id(1), id(3)],
        "Ctrl adds, in list order"
    );
    assert_eq!(surface.take_wants_open(), None, "and does not open");
    surface.mouse_down_with(area, TOP, x, y, true, false);
    assert_eq!(surface.selected(), [id(1)], "Ctrl again removes");
    // Ctrl moved the anchor to the row it toggled, as it does in the
    // library; a plain press re-anchors, and Shift extends from there.
    let (x, y) = centre(0);
    surface.mouse_down(area, TOP, x, y);
    surface.take_wants_open();
    let (x, y) = centre(2);
    surface.mouse_down_with(area, TOP, x, y, false, true);
    assert_eq!(surface.selected(), [id(1), id(2), id(3)], "Shift extends");
    // A right-click on a picked row is about the pick; on another row, that
    // row alone, which becomes the pick.
    let (x, y) = centre(1);
    assert_eq!(
        surface.context_menu(area, TOP, x, y),
        Some(vec![id(1), id(2), id(3)])
    );
    surface.select_only(id(1));
    let (x, y) = centre(1);
    assert_eq!(surface.context_menu(area, TOP, x, y), Some(vec![id(2)]));
    assert_eq!(surface.selected(), [id(2)]);
    // Delete asks for the pick when the keyboard's row is in it.
    assert!(surface.key(area, TOP, Press::Delete));
    assert_eq!(surface.take_wants_delete(), [id(2)]);
    // A listing without a picked chat drops it from the pick.
    surface.take_listing(Ok(crate::Listing {
        chats: vec![chat(1, "Oldest", 1_000), chat(3, "Newest", 3_000)],
        agents: Vec::new(),
    }));
    assert!(surface.selected().is_empty());
}

#[test]
fn the_accessibility_rects_are_the_drawn_rects_unculled() {
    let many: Vec<_> = (1..=60)
        .map(|n| chat(n, &format!("Chat {n}"), i64::from(n) * 1_000))
        .collect();
    let mut surface = showing(many);
    let area = area();
    let published = surface.chat_rects(area, TOP);
    // Every chat is published, including those below the fold.
    assert_eq!(published.len(), 60);
    // And each one that is drawn is published where it is drawn.
    for (row, rect) in surface.rows(area, TOP) {
        if let Row::Chat(index) = row {
            assert_eq!(published[index], (surface.chats()[index].id, rect));
        }
    }
}

/// **A host's rename field covers the title it replaces.** The rect is the
/// row's title part, the same one the draw puts the words in; a chat whose
/// row is scrolled out of the band has none, and the chat under a point is
/// the one whose row is there — without picking it.
#[test]
fn the_rename_field_sits_on_the_drawn_title_and_only_in_view() {
    let many: Vec<_> = (1..=60)
        .map(|n| chat(n, &format!("Chat {n}"), i64::from(n) * 1_000))
        .collect();
    let mut surface = showing(many);
    let area = area();
    let (row, rect) = surface.rows(area, TOP)[1];
    let Row::Chat(index) = row else {
        panic!("the second row is a chat");
    };
    let chat = surface.chats()[index].id;
    assert_eq!(
        surface.title_rect(area, TOP, chat),
        Some(surface.row_parts(row, rect).title)
    );
    assert_eq!(
        surface.chat_at(area, TOP, rect.x + 20.0, rect.y + 2.0),
        Some(chat)
    );
    assert!(surface.selected().is_empty(), "pointing is not choosing");
    // The last chat is far below the fold.
    let last = surface.chats()[59].id;
    assert_eq!(surface.title_rect(area, TOP, last), None);
    assert_eq!(surface.title_rect(area, TOP, id(999)), None);
}
