// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant};

use koru_ai_core::bots::{
    BotCreationLeftovers, BotCreationPhase, BotFolderChoice, BotRequestId, BotRoleInput,
};
use koru_ai_core::{AiProviderId, DocumentId, ModelSelection, ReasoningChoice};
use koru_i18n::{Lang, t};

use super::*;

fn model() -> ModelDraft {
    ModelDraft {
        model: ModelSelection {
            provider: AiProviderId::parse("agent-codex").expect("a provider id"),
            requested_model: "default".to_owned(),
            default_reasoning: ReasoningChoice::Default,
        },
        mode: None,
        fast: None,
    }
}

/// A draft that passes the form's checks: a name, a written role, an agent.
fn ready() -> CreateForm {
    let mut form = CreateForm::new(WorkspaceId::new());
    form.edit_name("Scout");
    form.edit_text("Find useful public discussions.");
    form.edit_model(model());
    form
}

/// The owner's view of `form`'s creation at `stage`.
fn creation(form: &CreateForm, stage: BotCreationStage) -> BotCreation {
    BotCreation {
        request_id: form.request(),
        bot_id: form.bot(),
        home_workspace: form.home(),
        name: "Scout".to_owned(),
        stage,
        phase: BotCreationPhase::Intent,
        reason: None,
        retryable: false,
        role_page: None,
        role_page_unconfirmed: false,
        folder_binding: None,
        left_behind: None,
        bot: None,
    }
}

fn answer(form: &mut CreateForm, creation: BotCreation, now: Instant) -> Settled {
    form.answer(
        creation.request_id,
        Ok(BotResponse::Creation { creation }),
        now,
    )
}

fn request_of(command: &BotCommand) -> BotRequestId {
    match command {
        BotCommand::CreateBotWithRole { request_id, .. }
        | BotCommand::ReadBotCreation { request_id }
        | BotCommand::RetryBotCreation { request_id }
        | BotCommand::AbandonBotCreation { request_id } => *request_id,
        _ => panic!("a creation command"),
    }
}

/// **One request id and one BotId for the life of a draft**: the Create,
/// every read of its progress, its Retry and its Abandon all name the same
/// creation, so a lost reply or a second press can only be replayed.
#[test]
fn one_request_and_one_bot_id_carry_the_draft_through_its_retries() {
    let mut form = ready();
    let (request, bot) = (form.request(), form.bot());
    let create = form.create(None).expect("sent");
    let BotCommand::CreateBotWithRole {
        request_id, bot_id, ..
    } = &create
    else {
        panic!("a create");
    };
    assert_eq!((*request_id, *bot_id), (request, bot));
    let now = Instant::now();
    let mut stopped = creation(&form, BotCreationStage::Incomplete);
    stopped.reason = Some(BotCreationReason::StoreUnavailable);
    stopped.retryable = true;
    answer(&mut form, stopped.clone(), now);
    let retry = form.retry().expect("Retry is offered");
    assert_eq!(request_of(&retry), request);
    answer(&mut form, stopped, now);
    form.shown();
    let read = form.read(now).expect("read once shown");
    assert_eq!(request_of(&read), request);
    form.answer(request, Err(Failure::Unreachable), now);
    assert_eq!(form.request(), request);
    assert_eq!(form.bot(), bot);
    assert_eq!(request_of(&form.abandon().expect("Abandon")), request);
}

/// **A duplicate press sends nothing**: while the Create is on its way,
/// and while the owner holds the creation.
#[test]
fn a_duplicate_press_sends_nothing() {
    let mut form = ready();
    assert!(form.create(None).is_some());
    assert!(form.create(None).is_none(), "on its way");
    assert!(!form.editable(), "and the inputs wait for its answer");
    form.edit_name("Other");
    assert_eq!(form.draft().name, "Scout", "an edit cannot reach it");
    let pending = creation(&form, BotCreationStage::Pending);
    answer(&mut form, pending, Instant::now());
    assert!(form.create(None).is_none(), "the owner holds it");
    assert!(form.retry().is_none(), "nothing to retry while it moves");
}

/// **A Create the link did not answer locks the draft until the owner
/// says what it holds**: acceptance is unknown, so nothing can be edited
/// and sent under the same request with other input, and a read is owed.
/// The owner's creation is then what is shown; `not_found` (never taken,
/// as `read_bot_creation` answers an unknown request) gives the draft back
/// under the same identities, and the next Create is the same command.
#[test]
fn an_unanswered_create_locks_the_draft_until_a_read_answers() {
    let mut form = ready();
    let first = form.create(None).expect("sent");
    let request = form.request();
    form.answer(request, Err(Failure::Unreachable), Instant::now());
    assert_eq!(form.notice(), Some(Words::BotsErrUnreachable));
    assert!(!form.editable(), "acceptance is unknown");
    form.edit_name("Other");
    assert_eq!(form.draft().name, "Scout", "no edit reaches it");
    assert!(form.create(None).is_none(), "nothing sent under it");
    let read = form.read(Instant::now()).expect("a read is owed");
    assert_eq!(request_of(&read), request);
    form.answer(
        request,
        Err(Failure::Refused("not_found".into())),
        Instant::now(),
    );
    assert!(form.editable(), "never taken: the person's again");
    assert_eq!(form.request(), request, "the same identities");
    assert_eq!(form.create(None).expect("sent again"), first);

    // Taken after all: the owner's creation is what the form shows.
    let mut taken = ready();
    taken.create(None).expect("sent");
    let request = taken.request();
    taken.answer(request, Err(Failure::Unreachable), Instant::now());
    let _ = taken.read(Instant::now()).expect("read");
    let moving = creation(&taken, BotCreationStage::Pending);
    answer(&mut taken, moving, Instant::now());
    assert!(!taken.editable());
    assert_eq!(taken.live().map(|live| live.name.as_str()), Some("Scout"));
    assert_eq!(taken.draft().name, "Scout", "the form says what was sent");
}

/// **Each stop reason has its sentence**, and Retry is offered exactly when
/// the owner says Retry can resolve it.
#[test]
fn every_stop_reason_has_a_sentence_and_retry_only_where_it_can_finish() {
    let mut seen = Vec::new();
    for reason in BotCreationReason::ALL {
        let words = reason_words(*reason);
        assert!(!seen.contains(&words), "{reason:?} has its own sentence");
        seen.push(words);
        for lang in Lang::ALL {
            assert!(!t(lang, words).trim().is_empty(), "{lang:?} {reason:?}");
        }
        let mut form = ready();
        form.create(None).expect("sent");
        let mut stopped = creation(&form, BotCreationStage::Incomplete);
        stopped.reason = Some(*reason);
        stopped.retryable = reason.retryable();
        let lines = stage_words(&stopped);
        assert_eq!(lines[0], words, "{reason:?}: why it stopped, first");
        assert_eq!(
            lines[1],
            if reason.retryable() {
                Words::BotsCreateIncomplete
            } else {
                Words::BotsCreateTerminal
            },
            "{reason:?}: what can be done"
        );
        answer(&mut form, stopped, Instant::now());
        assert_eq!(
            form.retry().is_some(),
            reason.retryable(),
            "{reason:?}: Retry only where it can resolve it"
        );
        assert!(form.can_abandon() || reason.retryable(), "{reason:?}");
    }
    assert_eq!(seen.len(), 12);
}

/// **A refusal is a sentence by its code**, never the server's English: a
/// chosen folder the owner refuses as `invalid_request` (one that does not
/// exist) reads as the folder sentence; Koru's own data folders have their
/// own code and sentence (below); the rest by the settings' own table.
#[test]
fn a_refused_create_says_why_in_words_and_keeps_the_draft() {
    let mut form = ready();
    form.choose_folder(std::path::Path::new(if cfg!(windows) {
        "C:\\Users\\me\\AppData\\Koru"
    } else {
        "/home/me/.koru"
    }));
    form.create(None).expect("sent");
    form.answer(
        form.request(),
        Err(Failure::Refused("invalid_request".to_owned())),
        Instant::now(),
    );
    assert_eq!(form.notice(), Some(Words::BotsFolderRefused));
    assert!(form.editable());
    let mut managed = ready();
    managed.create(None).expect("sent");
    managed.answer(
        managed.request(),
        Err(Failure::Refused("invalid_request".to_owned())),
        Instant::now(),
    );
    assert_eq!(managed.notice(), Some(Words::BotsErrInvalid));
    managed.create(None).expect("sent again");
    managed.answer(
        managed.request(),
        Err(Failure::Refused("request_conflict".to_owned())),
        Instant::now(),
    );
    assert_eq!(managed.notice(), Some(Words::BotsErrRequestConflict));
    assert!(
        matches!(
            managed.read(Instant::now()),
            Some(BotCommand::ReadBotCreation { .. })
        ),
        "what the owner holds under that request is read"
    );
}

/// **Over-limit input is refused in the form, in words**, before anything
/// is sent: the owner's own limits.
#[test]
fn over_limit_input_is_refused_in_the_form_in_words() {
    let mut form = ready();
    form.edit_text(&"x".repeat(BOT_ROLE_MARKDOWN_LIMIT + 1));
    assert_eq!(form.problem(), Some(Words::BotsRoleTextTooLong));
    assert!(form.create(None).is_none(), "nothing sent");
    form.edit_text(&"x".repeat(BOT_ROLE_MARKDOWN_LIMIT));
    assert_eq!(form.problem(), None, "the limit itself is taken");
    form.edit_title(&"t".repeat(BOT_ROLE_TITLE_LIMIT + 1));
    assert_eq!(form.problem(), Some(Words::BotsRoleTitleInvalid));
    form.edit_title("Two\nlines");
    assert_eq!(form.problem(), Some(Words::BotsRoleTitleInvalid));
    form.edit_title(&"t".repeat(BOT_ROLE_TITLE_LIMIT));
    assert_eq!(form.problem(), None);
    form.edit_name("  ");
    assert_eq!(form.problem(), Some(Words::BotsNameRequired));
    form.edit_name(&"n".repeat(257));
    assert_eq!(form.problem(), Some(Words::BotsNameTooLong));
    let mut existing = ready();
    existing.choose_role(RoleKind::Existing);
    assert_eq!(existing.problem(), Some(Words::BotsRolePageRequired));
    let mut agentless = CreateForm::new(WorkspaceId::new());
    agentless.edit_name("Scout");
    assert_eq!(agentless.problem(), Some(Words::BotsAgentRequired));
}

/// **The title follows the name** until the person types one; the examples
/// only fill in the text, and *Start your own* empties it.
#[test]
fn the_title_follows_the_name_and_examples_only_fill_the_text() {
    let mut form = ready();
    assert_eq!(form.title_shown(), "Scout");
    form.edit_name("Scout two");
    assert_eq!(form.title_shown(), "Scout two");
    form.edit_title("Research desk");
    form.edit_name("Scout three");
    assert_eq!(form.title_shown(), "Research desk");
    assert!(form.fill_text("An example."));
    assert_eq!(form.draft().text, "An example.");
    assert!(form.fill_text(""));
    assert!(form.draft().text.is_empty());
    let Some(BotCommand::CreateBotWithRole { role, folder, .. }) = form.create(None) else {
        panic!("sent");
    };
    assert_eq!(
        role,
        BotRoleInput::Authored {
            title: "Research desk".to_owned(),
            markdown: String::new(),
        }
    );
    assert_eq!(folder, BotFolderChoice::Managed {}, "managed by default");
}

/// **A new Bot starts in its agent's asking mode** unless the person chose
/// one: never the person's own default permission.
#[test]
fn a_new_bot_starts_in_the_asking_mode_unless_one_was_chosen() {
    assert_eq!(NEW_BOT_PERMISSION, NewChatPermission::Manual);
    let mut form = ready();
    let Some(BotCommand::CreateBotWithRole { config, .. }) = form.create(Some("ask".to_owned()))
    else {
        panic!("sent");
    };
    assert_eq!(config.mode.as_deref(), Some("ask"));
    let mut chosen = ready();
    let mut draft = model();
    draft.mode = Some("auto".to_owned());
    chosen.edit_model(draft);
    let Some(BotCommand::CreateBotWithRole { config, .. }) = chosen.create(Some("ask".to_owned()))
    else {
        panic!("sent");
    };
    assert_eq!(config.mode.as_deref(), Some("auto"), "the person's choice");
}

/// **Progress is read on the lifecycle UI's bounded clock**: 250 ms, then
/// doubling to two seconds while it moves; nothing while hidden or once it
/// stopped; one read when shown again.
#[test]
fn a_moving_creation_is_read_on_the_bounded_clock_and_never_while_hidden() {
    let mut form = ready();
    form.create(None).expect("sent");
    let now = Instant::now();
    assert!(
        form.deadline(now).is_some(),
        "a tick while it is with the link"
    );
    {
        let made = creation(&form, BotCreationStage::Pending);
        answer(&mut form, made, now);
    }
    let mut waits = Vec::new();
    let mut at = now;
    for _ in 0..5 {
        let due = form.deadline(at).expect("a read is due");
        waits.push(due - at);
        assert!(
            form.read(due - Duration::from_millis(1)).is_none(),
            "not early"
        );
        assert!(form.read(due).is_some());
        at = due;
        {
            let made = creation(&form, BotCreationStage::Pending);
            answer(&mut form, made, at);
        }
    }
    assert_eq!(
        waits,
        [250, 500, 1000, 2000, 2000].map(Duration::from_millis),
        "250 ms doubling to two seconds"
    );
    form.hidden();
    assert_eq!(form.deadline(at), None, "nothing while hidden");
    assert!(form.read(at + Duration::from_secs(60)).is_none());
    form.shown();
    assert!(form.read(at).is_some(), "shown again: read once");
    let mut stopped = creation(&form, BotCreationStage::Incomplete);
    stopped.reason = Some(BotCreationReason::StorageFailed);
    stopped.retryable = true;
    answer(&mut form, stopped, at);
    assert_eq!(form.deadline(at), None, "stopped: no clock");
}

/// **A creation the owner refused before it made anything keeps the
/// draft**, and the next Create is a new creation: the old identities are
/// ended.
#[test]
fn a_rejected_creation_keeps_the_draft_under_new_identities() {
    let mut form = ready();
    let (request, bot) = (form.request(), form.bot());
    form.create(None).expect("sent");
    let mut rejected = creation(&form, BotCreationStage::Rejected);
    rejected.reason = Some(BotCreationReason::FolderOccupied);
    answer(&mut form, rejected, Instant::now());
    assert_eq!(
        form.ended().map(|ended| ended.stage),
        Some(BotCreationStage::Rejected)
    );
    assert!(form.editable());
    assert_eq!(form.draft().name, "Scout");
    assert_ne!(form.request(), request);
    assert_ne!(form.bot(), bot);
    assert_eq!(
        stage_words(form.ended().expect("said")),
        [Words::BotsCreateFolderOccupied, Words::BotsCreateRejected]
    );
    assert!(
        form.create(None).is_some(),
        "created again, as a new creation"
    );
}

/// **Abandon deletes nothing and names what it left**; the draft stays,
/// under new identities.
#[test]
fn an_abandoned_creation_names_what_it_left_and_keeps_the_draft() {
    let mut form = ready();
    let request = form.request();
    form.create(None).expect("sent");
    let mut stopped = creation(&form, BotCreationStage::Incomplete);
    stopped.reason = Some(BotCreationReason::RoleMissing);
    answer(&mut form, stopped, Instant::now());
    assert!(form.retry().is_none(), "terminal");
    let abandon = form.abandon().expect("Abandon");
    assert_eq!(
        abandon,
        BotCommand::AbandonBotCreation {
            request_id: request
        }
    );
    assert!(form.abandon().is_none(), "once");
    let page = BotPage {
        workspace: form.home(),
        document: DocumentId::new(),
    };
    let mut abandoned = creation(&form, BotCreationStage::Abandoned);
    abandoned.role_page = Some(page.clone());
    abandoned.role_page_unconfirmed = true;
    abandoned.left_behind = Some(BotCreationLeftovers {
        role_page: Some(page.clone()),
        folder: Some("C:\\ai\\bots\\x\\work".to_owned()),
    });
    answer(&mut form, abandoned, Instant::now());
    let ended = form.ended().expect("what it left is said");
    assert_eq!(
        ended
            .left_behind
            .as_ref()
            .and_then(|left| left.role_page.clone()),
        Some(page)
    );
    assert!(form.editable());
    assert_ne!(form.request(), request);
    assert_eq!(form.draft().name, "Scout");
}

/// **The completion is the owner's Bot**, never one made up here.
#[test]
fn a_completed_creation_hands_over_the_owners_bot() {
    let mut form = ready();
    form.create(None).expect("sent");
    let made = crate::bots::tests::bot(form.bot(), form.home(), "Scout", false);
    let mut done = creation(&form, BotCreationStage::Completed);
    done.bot = Some(made.clone());
    let settled = answer(&mut form, done, Instant::now());
    assert!(settled.matched);
    assert_eq!(settled.created, Some(made));
    let mut unreadable = ready();
    unreadable.create(None).expect("sent");
    let done = creation(&unreadable, BotCreationStage::Completed);
    let settled = answer(&mut unreadable, done.clone(), Instant::now());
    assert_eq!(settled.created, None, "no Bot to read: nothing invented");
    assert_eq!(stage_words(&done), [Words::BotsCreateCreated]);
    let other = BotRequestId::mint();
    assert_eq!(
        form.answer(other, Err(Failure::Unreachable), Instant::now()),
        Settled::default(),
        "another request's answer settles nothing"
    );
}

/// **The folder the owner bound** is shown when it differs from the one
/// chosen, and always for the managed default.
#[test]
fn the_resolved_folder_is_shown_when_it_differs_from_the_choice() {
    let mut form = ready();
    let chosen = if cfg!(windows) {
        "C:\\work\\link"
    } else {
        "/work/link"
    };
    form.choose_folder(std::path::Path::new(chosen));
    form.create(None).expect("sent");
    let mut moving = creation(&form, BotCreationStage::Pending);
    moving.folder_binding = Some(chosen.to_owned());
    answer(&mut form, moving.clone(), Instant::now());
    assert_eq!(form.resolved_folder(), None, "the same folder");
    moving.folder_binding = Some("D:\\real".to_owned());
    answer(&mut form, moving, Instant::now());
    assert_eq!(form.resolved_folder(), Some("D:\\real"));
}

/// **A Create under a request the owner already holds with other input**
/// is refused as `create_bot_with_role` refuses it
/// (`apps/koru-ai-server/src/worker/bots/creation.rs`: a known request whose
/// fingerprint or Bot differs answers `request_conflict`). The form says
/// so, reads what the owner holds under that request, and then shows the
/// owner's creation — never the draft as if it were what is being made.
#[test]
fn a_create_under_a_request_held_with_other_input_shows_what_the_owner_holds() {
    let mut form = ready();
    form.create(None).expect("sent");
    let request = form.request();
    form.answer(
        request,
        Err(Failure::Refused("request_conflict".into())),
        Instant::now(),
    );
    assert_eq!(form.notice(), Some(Words::BotsErrRequestConflict));
    let read = form.read(Instant::now()).expect("what it holds is read");
    assert_eq!(request_of(&read), request);
    let mut held = creation(&form, BotCreationStage::Pending);
    held.name = "Other".to_owned();
    answer(&mut form, held, Instant::now());
    assert!(!form.editable(), "the owner's creation, not the draft");
    assert_eq!(form.live().map(|live| live.name.as_str()), Some("Other"));
}

/// **A folder that is Koru's own data has its own sentence**: the owner
/// answers `folder_reserved` (`koru_ai_wire::bot_creation_codes`, from the
/// AI server's creation folder check) for a chosen folder that is, or is
/// inside, the AI or Store data root, and `invalid_request` for one that
/// does not exist. The two read differently, and the draft stays editable.
#[test]
fn a_reserved_folder_and_a_missing_one_read_as_different_sentences() {
    let said = |code: &str| {
        let mut form = ready();
        form.choose_folder(std::path::Path::new(if cfg!(windows) {
            "C:\\Users\\me\\AppData\\Local\\koru"
        } else {
            "/home/me/.local/share/koru"
        }));
        form.create(None).expect("sent");
        form.answer(
            form.request(),
            Err(Failure::Refused(code.to_owned())),
            Instant::now(),
        );
        assert!(form.editable(), "{code}: the person chooses another");
        form.notice()
    };
    let reserved = said("folder_reserved");
    let missing = said("invalid_request");
    assert_eq!(reserved, Some(Words::BotsErrFolderReserved));
    assert_eq!(missing, Some(Words::BotsFolderRefused));
    assert_ne!(reserved, missing);
    // The settings' change of folder says it the same way.
    assert_eq!(
        crate::bots::settings::refusal_words(crate::bots::settings::FOLDER_RESERVED),
        Words::BotsErrFolderReserved
    );
    for lang in Lang::ALL {
        assert_ne!(
            t(lang, Words::BotsErrFolderReserved),
            t(lang, Words::BotsFolderRefused),
            "{lang:?}"
        );
    }
}
