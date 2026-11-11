// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::tests::bot;
use koru_ai_core::DocumentId;
use koru_ai_core::bots::{BotId, BotOperation, BotOperationKind, BotPage};
use koru_store_core::ids::WorkspaceId;

fn known() -> (Settings, Bot) {
    let bot = bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let mut settings = Settings::default();
    settings.know(bot.clone());
    (settings, bot)
}

/// The write a command carries: its request and the Bot command.
fn write(command: Command) -> (BotRequestId, BotCommand) {
    match command {
        Command::Write {
            request, command, ..
        } => (request, *command),
        _ => panic!("a settings change is a write"),
    }
}

fn operation(request: BotRequestId, bot: &Bot, status: BotOperationStatus) -> BotResponse {
    BotResponse::Operation {
        operation: BotOperation {
            request_id: request,
            bot_id: bot.id,
            kind: BotOperationKind::Pause,
            run_id: None,
            status,
        },
    }
}

/// **A revision conflict keeps the person's edit.** The owner refuses the
/// name at the revision it was read at; the settings say the Bot changed
/// elsewhere, owe a read, and still show the person's name. The newer Bot
/// read in does not overwrite it, and the next save carries the newer
/// revision.
#[test]
fn a_revision_conflict_keeps_the_unsaved_edit_and_reads_again() {
    let (mut settings, bot) = known();
    settings.edit_name("Scout two");
    let (request, sent) = write(settings.save_name().expect("a save"));
    let BotCommand::Update {
        expected_revision,
        config,
        ..
    } = &sent
    else {
        panic!("the name is an Update of the configuration");
    };
    assert_eq!(*expected_revision, bot.revision, "the revision it read");
    assert_eq!(config.name, "Scout two");
    assert!(settings.is_pending(Change::Name));
    assert_eq!(settings.name_shown(), "Scout two");

    let settled = settings.answer(
        request,
        Err(Failure::Refused("revision_conflict".into())),
        Instant::now(),
    );
    assert!(settled.matched && settled.bot.is_none());
    assert_eq!(settings.notice(), Some(Notice::ChangedElsewhere));
    assert!(settings.take_read_owed(), "the Bot is read again");
    assert!(!settings.take_read_owed(), "once");
    assert!(!settings.is_pending(Change::Name));
    assert_eq!(settings.name_shown(), "Scout two", "the edit is kept");

    let mut newer = bot.clone();
    newer.revision += 3;
    newer.config.name = "Renamed elsewhere".into();
    settings.know(newer.clone());
    assert_eq!(settings.name_shown(), "Scout two", "still the person's");
    let (_, again) = write(settings.save_name().expect("saved again"));
    assert!(matches!(
        again,
        BotCommand::Update { expected_revision, .. } if expected_revision == newer.revision
    ));
}

/// **A name is saved when the owner answers with the Bot that holds it**,
/// never when *Save* is pressed; the edit then is no longer an edit.
#[test]
fn a_name_is_shown_as_saved_only_on_the_owners_answer() {
    let (mut settings, bot) = known();
    assert!(!settings.can_save_name(), "no edit, nothing to save");
    settings.edit_name("Scout");
    assert!(!settings.name_edited(), "the owner's name is no edit");
    settings.edit_name("   ");
    assert!(!settings.can_save_name(), "a blank name is not sent");
    settings.edit_name("Researcher");
    let (request, _) = write(settings.save_name().unwrap());
    assert!(settings.save_name().is_none(), "one at a time");
    assert_eq!(
        settings.bot().unwrap().config.name,
        "Scout",
        "not applied yet"
    );

    let mut saved = bot.clone();
    saved.config.name = "Researcher".into();
    saved.revision += 1;
    let settled = settings.answer(
        request,
        Ok(BotResponse::Bot { bot: saved.clone() }),
        Instant::now(),
    );
    assert_eq!(settled.bot, Some(saved));
    assert!(!settings.name_edited());
    assert!(!settings.is_pending(Change::Name));
    assert_eq!(settings.name_shown(), "Researcher");
}

/// **Access is pending until the owner says it is effective.** The owner
/// answers `UpdateAccess` with a pending operation; the settings ask about
/// it again — only once its poll is due — and on *accepted* owe a read. The
/// Bot's own access is never changed by the press.
#[test]
fn access_stays_pending_until_the_operation_is_accepted() {
    let (mut settings, bot) = known();
    assert_eq!(
        crate::bots::view::region::CHOICES.len(),
        2,
        "exactly the two approved choices"
    );
    settings.choose_access(BotAccess::Workspace);
    assert!(settings.access_edited());
    let page = BotPage {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
    };
    assert!(settings.add_page(page.clone()));
    assert!(
        !settings.add_page(BotPage {
            workspace: WorkspaceId::new(),
            document: DocumentId::new(),
        }),
        "only a page of the home workspace"
    );
    let (request, sent) = write(settings.save_access().expect("a save"));
    match &sent {
        BotCommand::UpdateAccess {
            expected_revision,
            expected_policy_revision,
            access,
            selected_pages,
            ..
        } => {
            assert_eq!(*expected_revision, bot.revision);
            assert_eq!(*expected_policy_revision, bot.policy_revision);
            assert_eq!(*access, BotAccess::Workspace);
            assert_eq!(selected_pages, &[page.clone()]);
        }
        other => panic!("access is UpdateAccess, not {other:?}"),
    }
    assert!(settings.is_pending(Change::Access));
    settings.choose_access(BotAccess::Default);
    assert_eq!(
        settings.access_shown(),
        BotAccess::Workspace,
        "the choice does not move while it is on its way"
    );

    let now = Instant::now();
    settings.answer(
        request,
        Ok(operation(request, &bot, BotOperationStatus::Pending)),
        now,
    );
    assert!(settings.is_pending(Change::Access));
    assert!(settings.polls(now).is_empty(), "not asked again at once");
    let due = settings.deadline(now).expect("a poll is owed");
    assert!(due > now);
    let polls = settings.polls(due);
    assert_eq!(polls.len(), 1);
    let (polled, read) = write(polls.into_iter().next().unwrap());
    assert_eq!(polled, request);
    assert!(matches!(read, BotCommand::ReadOperation { request_id, .. } if request_id == request));
    assert!(settings.polls(due).is_empty(), "one question at a time");

    settings.answer(
        request,
        Ok(operation(request, &bot, BotOperationStatus::Accepted)),
        due,
    );
    assert!(!settings.is_pending(Change::Access));
    assert_eq!(
        settings.bot().unwrap().access,
        BotAccess::Default,
        "never applied by the press"
    );
    assert!(settings.take_read_owed(), "the owner's state is read");
    let mut effective = bot.clone();
    effective.access = BotAccess::Workspace;
    effective.selected_pages = vec![page];
    settings.know(effective);
    assert!(!settings.access_edited(), "the owner now holds it");
}

/// **Pause is pending until accepted**, and a rejected one says so and
/// reads the Bot again; the button's word follows the owner's state.
#[test]
fn pause_is_pending_until_accepted_and_a_rejection_is_said() {
    let (mut settings, bot) = known();
    let (request, sent) = write(settings.toggle_pause().unwrap());
    assert!(matches!(
        sent,
        BotCommand::Pause { expected_revision, .. } if expected_revision == bot.revision
    ));
    assert!(settings.is_pending(Change::Pause));
    assert!(settings.toggle_pause().is_none(), "not twice");
    let now = Instant::now();
    settings.answer(
        request,
        Ok(operation(request, &bot, BotOperationStatus::Pending)),
        now,
    );
    assert!(settings.is_pending(Change::Pause));
    settings.answer(
        request,
        Ok(operation(request, &bot, BotOperationStatus::Rejected)),
        now,
    );
    assert!(!settings.is_pending(Change::Pause));
    assert_eq!(
        settings.notice(),
        Some(Notice::Refused(Words::BotsErrNotApplied))
    );
    assert!(settings.take_read_owed());

    let mut paused = bot.clone();
    paused.paused = true;
    settings.know(paused.clone());
    let (_, resume) = write(settings.toggle_pause().unwrap());
    assert!(matches!(resume, BotCommand::Resume { .. }));
    assert!(settings.is_pending(Change::Resume));
}

/// **The folder changes only while the owner says it may**, through the
/// owner: pending until it answers with the Bot. Locked — a first task's
/// lock begun, or the folder frozen — there is no change at all.
#[test]
fn the_folder_changes_only_while_unlocked_and_is_pending_until_answered() {
    let (mut settings, bot) = known();
    let folder = if cfg!(windows) {
        std::path::PathBuf::from("D:\\bots\\scout")
    } else {
        std::path::PathBuf::from("/bots/scout")
    };
    assert!(settings.folder_changeable());
    assert!(
        settings
            .folder_chosen(std::path::Path::new("relative"))
            .is_none()
    );
    let (request, sent) = write(settings.folder_chosen(&folder).unwrap());
    assert!(matches!(
        &sent,
        BotCommand::Update { config, expected_revision, .. }
            if config.working_folder == folder.to_str().unwrap()
                && config.name == bot.config.name
                && *expected_revision == bot.revision
    ));
    assert!(settings.is_pending(Change::Folder));
    assert!(!settings.folder_changeable(), "not while one is on its way");
    assert_eq!(
        settings.bot().unwrap().config.working_folder,
        bot.config.working_folder
    );
    let mut moved = bot.clone();
    moved.config.working_folder = folder.to_str().unwrap().to_owned();
    settings.answer(request, Ok(BotResponse::Bot { bot: moved }), Instant::now());
    assert!(!settings.is_pending(Change::Folder));

    for (lock, frozen) in [(Some(BotRequestId::mint()), None), (None, Some(1))] {
        let (mut settings, mut bot) = known();
        bot.folder_lock_request = lock;
        bot.folder_frozen_at_ms = frozen;
        settings.know(bot);
        assert!(!settings.folder_changeable());
        assert!(settings.folder_chosen(&folder).is_none());
    }
}

/// **Every refusal code is a sentence of its own**, a conflict is the
/// changed-elsewhere sentence, and a code this build does not know is a
/// change not applied. Only the code reaches this model: the server's
/// message is never kept, so it cannot be shown.
#[test]
fn refused_codes_map_to_sentences() {
    use koru_ai_core::bots::BotError;
    let errors = [
        BotError::Invalid,
        BotError::NotFound,
        BotError::Forbidden,
        BotError::Unavailable,
        BotError::Conflict,
        BotError::RevisionConflict,
        BotError::FolderLocked,
        BotError::Storage,
    ];
    let mut seen = Vec::new();
    for error in errors {
        let words = refusal_words(error.code());
        assert_ne!(
            words,
            Words::BotsErrNotApplied,
            "{error:?} has its own sentence"
        );
        assert!(!seen.contains(&words), "{error:?} shares a sentence");
        seen.push(words);
        for lang in koru_i18n::Lang::ALL {
            assert!(
                !koru_i18n::t(lang, words).trim().is_empty(),
                "{lang:?} {error:?}"
            );
        }
    }
    assert_eq!(
        refusal_words("revision_conflict"),
        Words::BotsChangedElsewhere
    );
    assert_eq!(refusal_words("bot_something_new"), Words::BotsErrNotApplied);

    let (mut settings, _) = known();
    settings.edit_name("Other");
    let (request, _) = write(settings.save_name().unwrap());
    settings.answer(
        request,
        Err(Failure::Refused("storage_error".into())),
        Instant::now(),
    );
    assert_eq!(
        settings.notice(),
        Some(Notice::Refused(Words::BotsErrStorage))
    );
    assert_eq!(settings.name_shown(), "Other", "the edit is kept");
}

/// **A change to a Bot archived meanwhile is refused in the person's words**
/// (P1.7's `bot_archived`): its own sentence, never *not applied*; the edit
/// is kept, and the Bot is read again so the tab can say it is archived.
#[test]
fn a_change_to_an_archived_bot_says_so_and_reads_the_bot_again() {
    assert_eq!(refusal_words(BOT_ARCHIVED), Words::BotsErrArchived);
    for lang in koru_i18n::Lang::ALL {
        assert!(!koru_i18n::t(lang, Words::BotsErrArchived).trim().is_empty());
    }
    let (mut settings, _) = known();
    settings.edit_name("Other");
    let (request, _) = write(settings.save_name().unwrap());
    settings.answer(
        request,
        Err(Failure::Refused(BOT_ARCHIVED.into())),
        Instant::now(),
    );
    assert_eq!(
        settings.notice(),
        Some(Notice::Refused(Words::BotsErrArchived))
    );
    assert_eq!(settings.name_shown(), "Other", "the edit is kept");
    assert!(settings.take_read_owed(), "the Bot is read again");
}

/// **A change that was not answered is the same change when asked again**:
/// the same request, so a reply lost on the way back cannot become a second
/// change. Anything different is a new request.
#[test]
fn an_unanswered_change_asked_again_reuses_its_request() {
    let (mut settings, _) = known();
    settings.edit_name("Other");
    let (first, _) = write(settings.save_name().unwrap());
    settings.answer(first, Err(Failure::Unreachable), Instant::now());
    assert_eq!(
        settings.notice(),
        Some(Notice::Refused(Words::BotsErrUnreachable))
    );
    assert!(!settings.is_pending(Change::Name));
    let (again, _) = write(settings.save_name().unwrap());
    assert_eq!(again, first, "the same change, the same request");
    settings.answer(again, Err(Failure::Unreachable), Instant::now());
    settings.edit_name("Different");
    let (different, _) = write(settings.save_name().unwrap());
    assert_ne!(different, first);
}

/// An answer about a request this model does not hold — another tab's, an
/// old one — settles nothing and changes nothing.
#[test]
fn an_answer_about_another_request_settles_nothing() {
    let (mut settings, bot) = known();
    settings.edit_name("Other");
    let _ = settings.save_name();
    let mut other = bot.clone();
    other.config.name = "Elsewhere".into();
    let settled = settings.answer(
        BotRequestId::mint(),
        Ok(BotResponse::Bot { bot: other }),
        Instant::now(),
    );
    assert_eq!(settled, Settled::default());
    assert!(settings.is_pending(Change::Name));
    assert_eq!(settings.bot().unwrap().config.name, "Scout");
}
