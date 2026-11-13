// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::view::library as ui;
use koru_ai_core::bots::library::{BotKnowledgeReceipt, BotLibraryView};
use winit::keyboard::NamedKey;

fn page(bot: &Bot) -> BotPage {
    BotPage {
        workspace: bot.home_workspace,
        document: koru_ai_core::DocumentId::new(),
    }
}

fn ready(bot: Bot, pages: Vec<BotPage>) -> Fixture {
    let mut f = Fixture::opened(bot);
    assert!(f.view.library_pass());
    let seq = loop {
        if let Command::ReadLibrary { seq, .. } = f.commands.try_recv().unwrap() {
            break seq;
        }
    };
    f.answers
        .send(Answer::Library {
            seq,
            bot: f.bot.id,
            result: Ok(BotLibraryView {
                bot_id: f.bot.id,
                revision: f.bot.revision,
                role: f.bot.config.role.clone(),
                knowledge: pages,
                memory: None,
            }),
        })
        .unwrap();
    assert!(f.view.poll());
    f.publishable("Library read");
    f
}

fn focus(f: &mut Fixture, wanted: Key) {
    for _ in 0..160 {
        if f.view.focus() == Some(wanted) {
            return;
        }
        assert!(key(&mut f.view, named(NamedKey::Tab), None));
    }
    panic!("control not keyboard reachable: {wanted:?}");
}

#[test]
fn library_picker_edits_knowledge_without_editing_access() {
    use koru_editor::components::entity_picker::{DocumentChoice, WorkspaceChoice};
    let mut f = ready(unpaused(), vec![]);
    let access = f.view.settings().pages_shown().to_vec();
    assert!(f.press(ui::ADD));
    assert!(f.view.page_picker_open());
    let home = koru_core::WorkspaceId::from_uuid(f.bot.home_workspace.as_uuid());
    f.view.set_picker_context(vec![WorkspaceChoice {
        profile: koru_core::ProfileId::new(),
        profile_name: "Me".into(),
        id: home,
        name: "Research".into(),
    }]);
    let document = koru_core::DocumentId::new();
    f.view.set_picker_results(
        vec![DocumentChoice {
            target: koru_editor::trigger::DocumentTarget {
                document,
                node: None,
            },
            title: "Research notes".into(),
            breadcrumb: String::new(),
            is_file: false,
        }],
        false,
    );
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    assert!(!f.view.page_picker_open());
    assert_eq!(f.view.focus(), Some(ui::ADD));
    assert_eq!(f.view.library.pages().len(), 1);
    assert_eq!(
        f.view.library.pages()[0].document.as_uuid(),
        document.as_uuid()
    );
    assert_eq!(f.view.settings().pages_shown(), access);
    assert!(
        f.commands.try_recv().is_err(),
        "picker changes only the draft"
    );
    assert!(f.press(ui::ADD));
    assert!(key(&mut f.view, named(NamedKey::Escape), None));
    assert_eq!(f.view.focus(), Some(ui::ADD));
    assert_eq!(f.view.library.pages().len(), 1);
}

#[test]
fn library_keyboard_reorder_remove_and_retry_keep_page_identity() {
    let bot = unpaused();
    let a = page(&bot);
    let b = page(&bot);
    let mut f = ready(bot, vec![a.clone(), b.clone()]);
    f.view
        .page_titles
        .insert(a.document, "A long title with 日本語 and العربية".into());
    f.view
        .page_titles
        .insert(b.document, "Second reference".into());
    let up = ui::page_key(&b, "knowledge-up");
    let open = ui::page_key(&b, "knowledge-open");
    focus(&mut f, up);
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    assert_eq!(f.view.library.pages(), &[b.clone(), a.clone()]);
    assert_eq!(
        f.view.focus(),
        Some(open),
        "reordered boundary control gives focus to the same page"
    );
    assert!(
        published(&f.view, open)
            .unwrap()
            .contains("Second reference")
    );
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    assert_eq!(f.view.role_request.take(), Some(b.clone()));
    focus(&mut f, ui::page_key(&a, "knowledge-remove"));
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    assert_eq!(f.view.focus(), Some(ui::ADD));
    assert_eq!(f.view.library.pages(), &[b]);
    assert!(f.press(ui::SAVE));
    let (request, body) = f.wrote();
    f.reply(request, Err(Failure::Unanswered));
    assert!(f.view.library.dirty());
    assert!(f.press(ui::SAVE));
    assert_eq!(
        f.wrote(),
        (request, body),
        "lost reply retries the immutable original body"
    );
}

#[test]
fn library_modified_keys_do_not_save_and_archived_pages_remain_readable() {
    let bot = unpaused();
    let a = page(&bot);
    let b = page(&bot);
    let mut f = ready(bot, vec![a.clone(), b.clone()]);
    assert!(f.press(ui::page_key(&b, "knowledge-up")));
    focus(&mut f, ui::SAVE);
    let mut ws =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    for mods in [
        ModifiersState::CONTROL,
        ModifiersState::ALT,
        ModifiersState::SUPER,
        ModifiersState::SHIFT,
    ] {
        assert!(!View::key(
            &mut f.view,
            &mut ws,
            &named(NamedKey::Enter),
            None,
            mods
        ));
    }
    assert!(!View::key(
        &mut f.view,
        &mut ws,
        &named(NamedKey::Tab),
        None,
        ModifiersState::CONTROL,
    ));
    assert_eq!(f.view.focus(), Some(ui::SAVE));
    assert!(f.commands.try_recv().is_err());
    let mut archived = f.bot.clone();
    archived.lifecycle = koru_ai_core::bots::BotLifecycle::Archived;
    archived.revision += 1;
    let mut f = ready(archived, vec![a.clone()]);
    let controls = every_control(f.view.controls());
    for key in [
        ui::ADD,
        ui::SAVE,
        ui::DISCARD,
        ui::page_key(&a, "knowledge-remove"),
    ] {
        assert!(controls.iter().find(|c| c.key == key).unwrap().disabled);
    }
    assert!(f.press(ui::page_key(&a, "knowledge-open")));
    assert_eq!(f.view.role_request.take(), Some(a));
    assert!(f.commands.try_recv().is_err());
}

#[test]
fn library_save_fences_a_pre_save_bot_read_and_keeps_polling_while_shown() {
    let bot = unpaused();
    let a = page(&bot);
    let b = page(&bot);
    let mut f = ready(bot, vec![a.clone(), b.clone()]);
    assert!(f.press(ui::page_key(&b, "knowledge-up")));
    assert!(f.view.ask());
    let seq = f.asked();
    assert!(f.press(ui::SAVE));
    let (request, _) = f.wrote();
    assert!(f.view.maintenance_deadline().is_some());
    f.reply(
        request,
        Ok(BotResponse::KnowledgeSaved {
            receipt: BotKnowledgeReceipt {
                bot_id: f.bot.id,
                request_id: request,
                revision: f.bot.revision + 1,
                pages: vec![b.clone(), a.clone()],
            },
        }),
    );
    f.answer(seq, f.bot.id, Ok(f.bot.clone()));
    assert_eq!(f.view.library.pages(), &[b, a]);
    assert_eq!(f.view.library.known_revision(), f.bot.revision + 1);
    assert!(!f.view.asked, "stale Bot read leaves canonical reload owed");
    assert!(
        f.view.maintenance_deadline().is_some(),
        "Library reload wakes without user input"
    );
}

#[test]
fn library_unavailable_link_reports_failure_and_does_not_spin() {
    let mut f = Fixture::opened(unpaused());
    f.view.link = None;
    assert!(
        f.view.library_pass(),
        "the unavailable notice changes the view"
    );
    assert_eq!(f.view.library.notice(), Some(Words::BotsErrUnreachable));
    assert!(!f.view.library.loading());
    assert!(!f.view.library.awaiting());
    assert!(!f.view.library_pass(), "no per-frame retry");
    assert!(published(&f.view, ui::RELOAD).is_some());
    assert!(f.press(ui::RELOAD));
    assert!(f.view.library_pass(), "Reload explicitly retries");
}

#[test]
fn library_page_controls_are_named_localized_and_scroll_reachable() {
    for lang in Lang::ALL {
        let bot = unpaused();
        let pages: Vec<_> = (0..16).map(|_| page(&bot)).collect();
        let last = pages.last().unwrap().clone();
        let mut f = ready(bot, pages);
        f.view.lang = lang;
        f.view.set_viewport(420.0, 480.0);
        f.view
            .page_titles
            .insert(last.document, "日本語 العربية résumé".into());
        let open = ui::page_key(&last, "knowledge-open");
        focus(&mut f, open);
        assert!(f.view.scroll > 0.0, "keyboard reveals the last page");
        assert!(
            published(&f.view, open)
                .unwrap()
                .contains("日本語 العربية résumé")
        );
        f.publishable("localized Library at narrow width");
        let controls = every_control(f.view.controls());
        let card = controls.iter().find(|c| c.key == ui::CARD).unwrap().rect;
        for key in [ui::ADD, ui::SAVE, ui::DISCARD, ui::RELOAD, open] {
            let control = controls.iter().find(|c| c.key == key).unwrap();
            assert!(!control.label.is_empty());
            assert!(
                control.rect.x >= card.x
                    && control.rect.x + control.rect.w <= card.x + card.w + 0.5,
                "{}: {key:?} remains in Library",
                lang.tag()
            );
        }
        assert!(f.view.accessibility_action(open, true));
        assert_eq!(f.view.role_request.take(), Some(last.clone()));
        assert!(
            !f.view
                .accessibility_action(ui::page_key(&last, "knowledge-down"), true),
            "disabled boundary action is inert for assistive technology"
        );
    }
}
