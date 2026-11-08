// SPDX-License-Identifier: MPL-2.0

//! **All eight languages, for every word this surface says**, every time
//! through `koru_i18n::time`, and every line fitting its row at a narrow
//! width.

use koru_draw::text::TextEngine;
use koru_i18n::strings::{Key, t};
use koru_i18n::{Lang, time};

use super::*;

/// Every key this surface reads. **Written out** rather than derived, because
/// it is what the completeness check compares against.
const KEYS: &[Key] = &[
    Key::ChatsTitle,
    Key::ChatsNew,
    Key::ChatsLoading,
    Key::ChatsUnavailable,
    Key::ChatsEmpty,
    Key::ChatsPrivate,
    Key::ChatsLocalAgent,
    Key::ChatsUntitled,
    Key::ChatsRowName,
    Key::ChatsRowNamePrivate,
    Key::ChatsDelete,
    Key::ChatsDeleteTitle,
    Key::ChatsDeleteBody,
    Key::ChatsDeleting,
    Key::ChatsRefusedAgent,
    Key::ChatsRefusedClearing,
    Key::ChatsRefusedBusy,
    Key::ChatsRefusedUnavailable,
    Key::ChatsRefusedOther,
];

#[test]
fn every_word_this_surface_says_exists_in_all_eight() {
    for key in KEYS {
        for lang in Lang::ALL {
            assert!(
                !t(lang, *key).trim().is_empty(),
                "{key:?} is blank in {}",
                lang.tag()
            );
        }
    }
}

/// The stable codes a delete can be refused with. A code with no sentence of
/// its own still says something.
const REFUSAL_CODES: &[&str] = &[
    "agent_still_running",
    "history_clearing",
    "ai_attempt_active",
    "chat_busy",
    "gateway_unavailable",
];

#[test]
fn every_refusal_a_delete_can_meet_has_its_own_sentence() {
    for code in REFUSAL_CODES {
        assert_ne!(
            crate::text::refusal(Lang::En, code),
            t(Lang::En, Key::ChatsRefusedOther),
            "{code} fell through to the last resort"
        );
    }
    assert_eq!(
        crate::text::refusal(Lang::En, "something_new"),
        t(Lang::En, Key::ChatsRefusedOther)
    );
}

fn sentences(lang: Lang) -> Vec<String> {
    let mut surface = three();
    surface.set_lang(lang);
    let mut out: Vec<String> = surface
        .chats()
        .iter()
        .map(|chat| crate::text::meta_line(lang, chat, surface.agents(), NOW))
        .collect();
    out.extend(
        surface
            .chats()
            .iter()
            .filter_map(|chat| surface.accessible_name(chat.id)),
    );
    let mut untitled = chat(9, "  ", 0);
    untitled.current_model = model("agent-codex", "default");
    out.push(crate::text::title(lang, &untitled).to_owned());
    out.push(crate::text::who_answered(
        lang,
        &untitled.current_model,
        &[],
    ));
    for code in REFUSAL_CODES {
        out.push(crate::text::refusal(lang, code).to_owned());
    }
    out
}

/// Every sentence, in every language, with nothing left unfilled and nothing
/// left in English by accident.
#[test]
fn every_assembled_sentence_reads_in_every_language() {
    let english = sentences(Lang::En);
    for lang in Lang::ALL {
        let spoken = sentences(lang);
        assert_eq!(spoken.len(), english.len());
        for sentence in &spoken {
            assert!(!sentence.contains("{}"), "{}: {sentence}", lang.tag());
            assert!(!sentence.trim().is_empty(), "{}: empty", lang.tag());
        }
        if lang != Lang::En {
            assert!(
                spoken.iter().zip(&english).any(|(a, b)| a != b),
                "{} reads exactly like English",
                lang.tag()
            );
        }
    }
}

/// **Every time goes through `koru_i18n::time`**, so a hand-rolled "3h ago"
/// anywhere in this crate would fail here.
#[test]
fn every_time_is_koru_i18n_time() {
    let surface = three();
    for lang in Lang::ALL {
        for chat in surface.chats() {
            let moment = time::relative_long(lang, chat.updated_at.millis(), NOW);
            assert_eq!(
                crate::text::when(lang, chat.updated_at.millis(), NOW),
                moment
            );
            assert!(crate::text::meta_line(lang, chat, surface.agents(), NOW).contains(&moment));
        }
    }
}

/// **The eight-language snapshot, at a narrow width.** Every line the list
/// draws fits the rect it is drawn in — elided, never wrapped or spilled —
/// in every language, including the right-to-left one.
#[test]
fn every_line_fits_its_row_in_every_language_at_a_narrow_width() {
    let narrow = Rect::new(0.0, 0.0, 150.0, 480.0);
    let mut engine = TextEngine::new();
    for lang in Lang::ALL {
        let mut chats = vec![
            chat(1, "A deliberately long chat title that cannot fit", 1_000),
            chat(2, "Release checklist", 90_000),
        ];
        chats[1].private = true;
        chats[1].current_model = model("agent-opencode", "deepseek/v4-pro");
        let mut surface = showing(chats);
        surface.set_lang(lang);
        surface.delete(id(1));
        surface.take_deleted(
            id(1),
            Err(koru_ai_wire::Failure {
                code: "agent_still_running".into(),
                message: String::new(),
                retryable: true,
                details: Default::default(),
            }),
        );
        let mut scene = vello::Scene::new();
        surface.draw(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut engine,
            narrow,
            TOP,
        );
        // Two titles and the note under the refused one: the second line
        // left the row (the review's note).
        assert!(surface.work().layouts >= 3, "{}", lang.tag());
        for (row, rect) in surface.rows(narrow, TOP) {
            let parts = surface.row_parts(row, rect);
            assert!(parts.title.x + parts.title.w <= narrow.x + narrow.w);
            let lines: Vec<(String, Rect, koru_draw::theme::TextStyle)> = match row {
                Row::Chat(index) => {
                    let chat = &surface.chats()[index];
                    vec![(
                        crate::text::title(lang, chat).to_owned(),
                        parts.title,
                        koru_draw::theme::role::row(),
                    )]
                }
                Row::Note(_) => vec![(
                    t(lang, Key::ChatsRefusedAgent).to_owned(),
                    parts.title,
                    koru_draw::theme::role::meta(),
                )],
                Row::Notice(_) | Row::PinnedHeading | Row::GroupGap => vec![],
            };
            for (text, rect, style) in lines {
                let shown = engine.elided(&text, style, rect.w);
                let width = engine.simple_layout(&shown, style).width();
                assert!(
                    width <= rect.w + 0.5,
                    "{}: {shown:?} is {width} wide in {}",
                    lang.tag(),
                    rect.w
                );
            }
        }
    }
}
