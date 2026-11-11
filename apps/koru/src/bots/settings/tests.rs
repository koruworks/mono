// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::tests::bot;
use koru_ai_core::DocumentId;
use koru_ai_core::bots::{BotId, BotOperation, BotOperationKind, BotPage, BotPendingChange};
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

/// **Pause is pending until accepted**, and a rejected one reads the Bot
/// again and, when the read does not show it, says so; the button's word
/// follows the owner's state.
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
    assert!(settings.take_read_owed());
    // The read shows the Bot as it was: the Pause was not applied.
    settings.know(bot.clone());
    assert!(!settings.is_pending(Change::Pause));
    assert_eq!(
        settings.notice(),
        Some(Notice::Refused(Words::BotsErrNotApplied))
    );

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
        BotError::Settling,
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
    assert_eq!(refusal_words(""), Words::BotsErrNotApplied);
    // `BotError::Unavailable` is what runs the Bot not being ready, never
    // the workspace; `Settling` is retryable and says so.
    assert_eq!(
        refusal_words(BotError::Unavailable.code()),
        Words::BotsErrNotReady
    );
    assert_eq!(
        refusal_words(BotError::Settling.code()),
        Words::BotsErrOperationPending
    );
    assert!(
        koru_i18n::t(koru_i18n::Lang::En, Words::BotsErrOperationPending)
            .contains("Try again in a moment")
    );

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
    settings.answer(first, Err(Failure::lost()), Instant::now());
    assert_eq!(
        settings.notice(),
        Some(Notice::Refused(Words::BotsErrUnreachable))
    );
    assert!(!settings.is_pending(Change::Name));
    let (again, _) = write(settings.save_name().unwrap());
    assert_eq!(again, first, "the same change, the same request");
    settings.answer(again, Err(Failure::lost()), Instant::now());
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

/// **The agent-and-model card's write** (S7): the draft changes nothing
/// until *Save*, which is an `Update` of the configuration read with the
/// chosen model, mode and Fast and nothing else; it is pending until the
/// owner answers with the Bot, a conflict keeps the draft, and an edit back
/// to what the owner holds is no edit.
#[test]
fn the_model_is_an_update_at_the_read_revision_and_shown_only_when_answered() {
    let (mut settings, bot) = known();
    assert!(!settings.can_save_model(), "nothing edited");
    let mut draft = settings.model_shown().unwrap();
    draft.model.requested_model = "gpt-5".into();
    draft.model.default_reasoning =
        koru_ai_core::ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High);
    draft.mode = Some("auto-review".into());
    draft.fast = Some(true);
    settings.edit_model(draft.clone());
    assert!(settings.model_edited());
    let (request, command) = write(settings.save_model().unwrap());
    let BotCommand::Update {
        expected_revision,
        config,
        ..
    } = command
    else {
        panic!("an update");
    };
    assert_eq!(expected_revision, bot.revision);
    assert_eq!(config.model, draft.model);
    assert_eq!(config.mode, draft.mode);
    assert_eq!(config.fast, Some(true));
    assert_eq!(config.name, bot.config.name, "the name is the owner's");
    assert_eq!(config.working_folder, bot.config.working_folder);
    assert!(settings.is_pending(Change::Model));
    // Nothing moves while it is on its way.
    let mut other = draft.clone();
    other.fast = Some(false);
    settings.edit_model(other);
    assert_eq!(settings.model_shown(), Some(draft.clone()));

    // A conflict: the draft is kept and the Bot read again.
    let settled = settings.answer(
        request,
        Err(Failure::Refused("revision_conflict".into())),
        std::time::Instant::now(),
    );
    assert!(settled.matched);
    assert!(settings.take_read_owed());
    assert_eq!(settings.model_shown(), Some(draft.clone()));

    // Saved again, and answered with the Bot that holds it: no edit left.
    let (request, _) = write(settings.save_model().unwrap());
    let mut saved = bot.clone();
    saved.revision += 1;
    saved.config.model = draft.model.clone();
    saved.config.mode = draft.mode.clone();
    saved.config.fast = draft.fast;
    let settled = settings.answer(
        request,
        Ok(BotResponse::Bot { bot: saved.clone() }),
        std::time::Instant::now(),
    );
    assert_eq!(settled.bot, Some(saved));
    assert!(!settings.model_edited());
    assert!(!settings.is_pending(Change::Model));

    // An edit back to what the owner holds is no edit.
    let held = settings.model_shown().unwrap();
    settings.edit_model(held);
    assert!(!settings.model_edited());
}

/// **A poll turned away for now keeps a pending Pause followed** — the
/// third review's probe, made permanent. The owner holds the Pause as
/// pending; its `ReadOperation` is turned away by the AI worker
/// (`queue_failure`'s `ai_worker_saturated`, through the real link's
/// classification). Only the question went unanswered: the Pause is still
/// pending, said as *Pausing…*, and polled again on the clock — never
/// *Koru AI is busy, so nothing changed*.
#[test]
fn a_poll_turned_away_for_now_keeps_a_pending_pause_followed() {
    let (mut settings, bot) = known();
    let (request, _) = write(settings.toggle_pause().unwrap());
    let now = Instant::now();
    settings.answer(
        request,
        Ok(operation(request, &bot, BotOperationStatus::Pending)),
        now,
    );
    let later = now + std::time::Duration::from_secs(5);
    assert_eq!(
        settings.polls(later).len(),
        1,
        "the pending Pause is polled"
    );
    settings.answer(request, Err(Failure::turned_away()), later);
    assert!(settings.is_pending(Change::Pause), "still followed");
    assert_eq!(settings.notice(), None, "nothing claimed");
    let again = settings.deadline(later).expect("polled again");
    assert!(again > later);
    assert_eq!(settings.polls(again).len(), 1, "and polled");
    // Refused for good: not the Pause's words, and the Bot is read.
    settings.answer(request, Err(Failure::Refused("wrong_owner".into())), again);
    assert!(!settings.is_pending(Change::Pause));
    assert_eq!(settings.notice(), None, "never the change's refusal");
    assert!(settings.take_read_owed(), "the Bot says whether it paused");
}

/// **A pause, a resume or an access change Koru AI did not answer, read as
/// made, is no longer unconfirmed** — as a save already was: the read says
/// which, so *Koru AI didn't answer* is not said beside a change it shows.
#[test]
fn an_unanswered_pause_resume_or_access_read_as_made_is_no_longer_unconfirmed() {
    // Pause, then Resume.
    for pausing in [true, false] {
        let (mut settings, mut bot) = known();
        bot.paused = !pausing;
        settings.know(bot.clone());
        let (request, _) = write(settings.toggle_pause().unwrap());
        settings.answer(request, Err(Failure::lost()), Instant::now());
        assert_eq!(
            settings.notice(),
            Some(Notice::Refused(Words::BotsErrUnreachable)),
            "{pausing}"
        );
        // A read that does not show it yet keeps the sentence.
        settings.know(bot.clone());
        assert!(settings.notice().is_some(), "{pausing}: not shown yet");
        bot.paused = pausing;
        settings.know(bot.clone());
        assert_eq!(settings.notice(), None, "{pausing}: read as made");
    }
    // An access change.
    let (mut settings, mut bot) = known();
    let other = if bot.access == BotAccess::Default {
        BotAccess::Workspace
    } else {
        BotAccess::Default
    };
    settings.choose_access(other);
    let (request, _) = write(settings.save_access().unwrap());
    settings.answer(request, Err(Failure::lost()), Instant::now());
    assert!(settings.notice().is_some());
    bot.access = other;
    settings.know(bot);
    assert_eq!(settings.notice(), None, "access read as made");
}

/// The Bot's read while `change` is still being delivered to what runs it,
/// as `read` fills it in (`bot_pending_change`,
/// `apps/koru-ai-server/src/worker/bots/admission.rs`): under `request`, the
/// one the AI owner made itself when it renewed a refused pause.
fn delivering(bot: &Bot, kind: BotOperationKind, request: BotRequestId) -> Bot {
    let mut bot = bot.clone();
    bot.pending_change = Some(BotPendingChange {
        request_id: request,
        kind,
        since_ms: 1,
        last_answer: Some("bot_conflict".into()),
    });
    bot
}

/// **A Pause its execution owner refused as conflicting, which the AI owner
/// renewed, is never said as not applied** — the last agent's finding. The
/// owner settles the person's own request `rejected` and offers the same
/// change again under a request of its own, with the Bot still paused
/// (`settle_refused_bot_pause`), so `read_operation` of the person's request
/// answers `rejected` while the Bot's read says paused and names the
/// renewal in `pending_change`. What the read says wins: no *not applied*,
/// the change shown as still being delivered, and once it is delivered
/// nothing is left to say. The same holds for a Resume.
#[test]
fn a_rejected_pause_the_read_shows_made_is_never_said_as_not_applied() {
    for pausing in [true, false] {
        let (mut settings, mut bot) = known();
        bot.paused = !pausing;
        settings.know(bot.clone());
        let (request, _) = write(settings.toggle_pause().unwrap());
        let now = Instant::now();
        let change = if pausing {
            Change::Pause
        } else {
            Change::Resume
        };
        settings.answer(
            request,
            Ok(operation(request, &bot, BotOperationStatus::Pending)),
            now,
        );
        settings.answer(
            request,
            Ok(operation(request, &bot, BotOperationStatus::Rejected)),
            now,
        );
        assert_eq!(settings.notice(), None, "{pausing}: the read decides");
        assert!(
            settings.is_pending(change),
            "{pausing}: on its way until read"
        );
        assert!(settings.take_read_owed(), "{pausing}: the Bot is read");

        let kind = if pausing {
            BotOperationKind::Pause
        } else {
            BotOperationKind::Resume
        };
        let mut read = delivering(&bot, kind, BotRequestId::mint());
        read.paused = pausing;
        read.revision += 2;
        settings.know(read.clone());
        assert_eq!(settings.notice(), None, "{pausing}: never *not applied*");
        assert!(!settings.is_pending(change));
        let words = if pausing {
            Words::BotsDeliveringPause
        } else {
            Words::BotsDeliveringResume
        };
        assert_eq!(settings.delivering_line(), Some(words), "{pausing}");
        assert!(settings.toggle_pause().is_none(), "{pausing}: inert");

        // Delivered: the read names nothing, and nothing is left to say.
        read.pending_change = None;
        settings.know(read);
        assert_eq!(settings.delivering_line(), None, "{pausing}");
        assert_eq!(settings.notice_words(), None, "{pausing}");
        assert!(
            settings.toggle_pause().is_some(),
            "{pausing}: offered again"
        );
    }
}

/// **A change refused `bot_operation_pending` says which change it waited
/// on** once the Bot's read names it (`require_pause_settled`'s
/// `BotError::Settling`, read as the real link reads it): the general
/// sentence until then, the specific one after, and never both the refusal
/// and the line saying the same thing. The refusal owes the read.
#[test]
fn a_change_refused_while_another_is_delivered_says_which_one_once_the_read_names_it() {
    let (mut settings, bot) = known();
    settings.edit_name("Other");
    let (request, _) = write(settings.save_name().unwrap());
    settings.answer(
        request,
        Err(Failure::Refused(BOT_OPERATION_PENDING.into())),
        Instant::now(),
    );
    assert_eq!(
        settings.notice_words(),
        Some(Words::BotsErrOperationPending),
        "general until the read names it"
    );
    assert!(settings.take_read_owed(), "the read names the change");
    settings.know(delivering(
        &bot,
        BotOperationKind::UpdateAccess,
        BotRequestId::mint(),
    ));
    assert_eq!(settings.notice_words(), Some(Words::BotsDeliveringAccess));
    assert_eq!(settings.delivering_line(), None, "said once");
    assert_eq!(settings.name_shown(), "Other", "the edit is kept");
}

/// **While a change is still being delivered, what the owner would refuse
/// is not offered as if it would work** (`bot_operation_pending` answers a
/// pause, resume, settings save and access change): each command's model
/// sends nothing, and the drafts stay the person's. Offered again once the
/// read names nothing.
#[test]
fn while_a_change_is_delivered_its_commands_send_nothing() {
    let (mut settings, bot) = known();
    let folder = if cfg!(windows) {
        std::path::PathBuf::from("D:\\bots\\scout")
    } else {
        std::path::PathBuf::from("/bots/scout")
    };
    settings.know(delivering(
        &bot,
        BotOperationKind::Pause,
        BotRequestId::mint(),
    ));
    assert!(settings.delivering());
    settings.edit_name("Other");
    settings.choose_access(BotAccess::Workspace);
    assert!(!settings.can_save_name() && settings.save_name().is_none());
    assert!(!settings.can_save_access() && settings.save_access().is_none());
    assert!(!settings.can_toggle_pause() && settings.toggle_pause().is_none());
    assert!(settings.folder_changeable(), "the folder is not fixed");
    assert!(!settings.can_change_folder() && settings.folder_chosen(&folder).is_none());
    assert_eq!(settings.name_shown(), "Other", "the draft stays");

    settings.know(bot);
    assert!(!settings.delivering());
    assert!(settings.save_name().is_some());
}
