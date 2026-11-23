// SPDX-License-Identifier: MPL-2.0

//! The owned seam beside the ordinary surface it must not change.
//!
//! Every owned invariant has its ordinary pin next to it: the ordinary
//! scopes (in `reader.rs`'s tests), a send with no destination, a missing
//! chat that becomes a new chat, and a model choice remembered as the next
//! chat's start.

use std::sync::mpsc::Receiver;

use super::*;
use crate::reader::{Reader, Update};
use crate::{ResponseAction, composer_plus::PlusItem};
use koru_ai_core::{
    Chat, ChatToolPolicy, MessageId, ModelSelection, ReasoningChoice, Timestamp, ToolInvocationId,
    ToolMode,
};
use koru_ai_wire::{
    AgentProviderView, ChatTranscript, OllamaSettingsView, OpenAiSettingsView,
    ProviderSettingsResponse, ProviderValidationResponse, ProviderValidationStatus,
};

fn codex() -> AiProviderId {
    AiProviderId::parse("agent-codex").unwrap()
}

fn claude() -> AiProviderId {
    AiProviderId::parse("agent-claude").unwrap()
}

fn chat_on(id: ChatId, provider: AiProviderId) -> ChatTranscript {
    let now = Timestamp::from_millis(1);
    ChatTranscript {
        active_attempt: None,
        sources: Vec::new(),
        chat: Chat {
            id,
            title: crate::DEFAULT_CHAT_TITLE.into(),
            private: false,
            current_model: ModelSelection {
                provider,
                requested_model: "default".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            branched_from: None,
            agent_working_folder: None,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            bot: None,
            created_at: now,
            updated_at: now,
        },
        tool_policy: ChatToolPolicy {
            chat_id: id,
            mode: ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        notices: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        reasoning_control: None,
        agent_commands: None,
        attachments: Vec::new(),
    }
}

/// Settings with both agents listed and Claude chosen: where a person's
/// ordinary new chat starts.
fn settings() -> ProviderSettingsResponse {
    ProviderSettingsResponse {
        selected_provider: Some(claude()),
        openai: OpenAiSettingsView {
            model: None,
            reasoning: ReasoningChoice::Default,
            credential_configured: false,
        },
        ollama: OllamaSettingsView {
            endpoint: "http://localhost:11434".into(),
            model: None,
            reasoning: ReasoningChoice::Default,
        },
        agents: [claude(), codex()]
            .into_iter()
            .map(|id| AgentProviderView {
                id,
                label: "Agent".into(),
                steers: false,
            })
            .collect(),
    }
}

fn conversation(chat: ChatId, destination: WorkspaceId) -> OwnedConversation {
    OwnedConversation {
        chat,
        destination,
        provider: codex(),
        offers: OwnedOffers::default(),
        refusals: &[],
    }
}

/// An owned surface bound to `chat`, with a reader the test holds.
fn owned_surface(chat: ChatId, destination: WorkspaceId) -> (ChatSurface, Receiver<Command>) {
    let mut surface = ChatSurface::new_owned_at(None);
    assert!(surface.bind_owned(conversation(chat, destination)));
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    (surface, commands)
}

/// Everything the surface queued, oldest first.
fn drained(commands: &Receiver<Command>) -> Vec<Command> {
    std::iter::from_fn(|| commands.try_recv().ok()).collect()
}

fn names(commands: &[Command]) -> Vec<&'static str> {
    commands.iter().map(Command::name).collect()
}

fn boot(surface: &mut ChatSurface, transcript: Option<ChatTranscript>, link_missing: bool) {
    surface.apply_update(Update::Boot {
        settings: settings(),
        chats: Vec::new(),
        transcript,
        link_missing,
        destinations: Vec::new(),
        destination_error: None,
    });
}

fn ready(surface: &mut ChatSurface, provider: AiProviderId) {
    surface.apply_update(Update::Validation(ProviderValidationResponse {
        provider,
        status: ProviderValidationStatus::Ready,
        models: Vec::new(),
        selected_model: None,
        failure: None,
    }));
}

/// **Every turn an owned surface sends carries its destination**: a typed
/// message, a Retry and the answers to an agent's questions — the three
/// paths that used to send `None`, which the Bot owner refuses.
#[test]
fn owned_sends_carry_the_destination() {
    let chat = ChatId::new();
    let destination = WorkspaceId::new();
    let (mut surface, commands) = owned_surface(chat, destination);
    surface.transcript = Some(chat_on(chat, codex()));

    surface.composer.set_text("Find three sources");
    assert!(surface.send_composer());
    let sent = drained(&commands);
    let Some(Command::Send(request)) = sent.first() else {
        panic!("the message went first and alone: {:?}", names(&sent));
    };
    assert_eq!(request.destination_workspace, Some(destination));
    assert_eq!(request.chat_id, chat);
    assert_eq!(names(&sent), ["send"], "no grant, no automatic title");

    surface.perform_response_action(ResponseAction::Retry(MessageId::new()));
    let Some(Command::Retry(retried, request)) = commands.try_recv().ok() else {
        panic!("a retry is queued");
    };
    assert_eq!(retried, chat);
    assert_eq!(request.destination_workspace, Some(destination));

    assert!(surface.send_answers(ToolInvocationId::new(), Vec::new()));
    let Some(Command::Send(request)) = commands.try_recv().ok() else {
        panic!("the answers go as a turn");
    };
    assert!(request.answers.is_some());
    assert_eq!(request.destination_workspace, Some(destination));
}

#[test]
fn bot_chat_follows_the_reply_without_lifting_the_question_and_respects_reading() {
    let chat = ChatId::new();
    let (mut surface, _commands) = owned_surface(chat, WorkspaceId::new());
    surface.transcript = Some(chat_on(chat, codex()));
    surface.set_viewport(Rect::new(0.0, 0.0, 480.0, 360.0));
    surface.composer.set_text("Hello");
    assert!(surface.send_composer());
    assert!(!surface.scroll_to_pending_question);
    assert!(surface.follow_tail);
    surface.draft = "A longer reply that continues the conversation.\n\n".repeat(40);
    surface.messages_dirty = true;
    surface.draw_messages(&mut vello::Scene::new(), vello::kurbo::Affine::IDENTITY);
    assert_eq!(surface.pending_question_anchor, None);
    assert!((surface.conversation_extent - surface.conversation_content_extent()).abs() < 0.01);
    let bottom = (surface.conversation_extent - surface.conversation_rect().h).max(0.0);
    assert!(bottom > 0.0);
    assert!((surface.conversation_scroll - bottom).abs() < 0.01);

    surface.scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, 3.0));
    let reading = surface.conversation_scroll;
    assert!(!surface.follow_tail);
    surface.draft.push_str(&"More words.\n\n".repeat(20));
    surface.messages_dirty = true;
    surface.draw_messages(&mut vello::Scene::new(), vello::kurbo::Affine::IDENTITY);
    assert!((surface.conversation_scroll - reading).abs() < 0.01);
    surface.scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, -1000.0));
    assert!(
        surface.follow_tail,
        "scrolling back to the bottom resumes following"
    );
}

/// **An ordinary surface's turns carry no destination**, exactly as before
/// the seam: the same three paths, `None` each time.
#[test]
fn ordinary_sends_carry_no_destination() {
    let mut surface = ChatSurface::new();
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    let chat = ChatId::new();
    surface.transcript = Some(chat_on(chat, AiProviderId::parse("openai").unwrap()));
    assert!(surface.owned().is_none());
    assert_eq!(surface.offers(), OwnedOffers::ORDINARY);

    surface.composer.set_text("Hello");
    assert!(surface.send_composer());
    let sent = drained(&commands);
    let send = sent
        .iter()
        .find_map(|command| match command {
            Command::Send(request) => Some(request),
            _ => None,
        })
        .expect("a send");
    assert_eq!(send.destination_workspace, None);
    assert!(
        names(&sent).contains(&"rename"),
        "the automatic title still follows an ordinary first message"
    );

    surface.perform_response_action(ResponseAction::Retry(MessageId::new()));
    let Some(Command::Retry(_, request)) = commands.try_recv().ok() else {
        panic!("a retry is queued");
    };
    assert_eq!(request.destination_workspace, None);

    assert!(surface.send_answers(ToolInvocationId::new(), Vec::new()));
    let Some(Command::Send(request)) = commands.try_recv().ok() else {
        panic!("the answers go as a turn");
    };
    assert_eq!(request.destination_workspace, None);
}

/// **An owned conversation that is gone is shown as gone** — never as a new
/// chat. The boot says the chat is missing; the surface is unavailable, owes
/// no fresh chat, and nothing that follows — a ready provider, a host's *new
/// chat*, Enter in the composer — queues a Create or a Send.
#[test]
fn owned_not_found_shows_unavailable_and_queues_no_create_or_send() {
    let chat = ChatId::new();
    let (mut surface, commands) = owned_surface(chat, WorkspaceId::new());
    boot(&mut surface, None, true);
    assert_eq!(surface.unavailable(), Some(Unavailable::Missing));
    assert!(!surface.fresh_chat_wanted, "no new chat is owed");
    assert!(surface.transcript.is_none());
    assert_eq!(
        surface.owned().map(|owned| owned.chat),
        Some(chat),
        "the host still knows which conversation it is"
    );

    ready(&mut surface, claude());
    ready(&mut surface, codex());
    assert!(!surface.start_new_chat(), "a host's new chat is refused");
    assert!(!surface.create_chat());
    surface.composer.set_text("anyone there?");
    surface.send_composer();
    assert!(
        !surface.key(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            winit::keyboard::ModifiersState::empty(),
        ),
        "an unavailable conversation takes no key"
    );
    let queued = drained(&commands);
    assert!(
        !queued
            .iter()
            .any(|command| matches!(command, Command::Create(_) | Command::Send(_))),
        "nothing creates or sends: {:?}",
        names(&queued)
    );
    assert!(surface.transcript.is_none(), "no stand-in chat either");
}

/// A read of the bound conversation that the owner answers *not found* is
/// the same state, and a read about another chat is ignored. A failed
/// refresh of the conversation already on screen keeps it there.
#[test]
fn an_owned_read_that_fails_is_typed_and_keeps_what_it_should() {
    let chat = ChatId::new();
    let (mut surface, _commands) = owned_surface(chat, WorkspaceId::new());
    boot(&mut surface, Some(chat_on(chat, codex())), false);
    assert!(surface.unavailable().is_none());

    surface.apply_update(Update::Unavailable {
        chat: Some(ChatId::new()),
        reason: Unavailable::Missing,
    });
    assert!(surface.unavailable().is_none(), "another chat's answer");
    surface.apply_update(Update::Unavailable {
        chat: Some(chat),
        reason: Unavailable::Unreachable,
    });
    assert!(surface.unavailable().is_none(), "a failed refresh keeps it");
    assert_eq!(surface.transcript.as_ref().map(|t| t.chat.id), Some(chat));

    surface.apply_update(Update::Unavailable {
        chat: Some(chat),
        reason: Unavailable::Missing,
    });
    assert_eq!(surface.unavailable(), Some(Unavailable::Missing));
    assert!(surface.transcript.is_none(), "a gone chat is not shown");
    assert!(!surface.fresh_chat_wanted);

    // Bound again — the host's retry — it reads that chat again.
    assert!(surface.bind_owned(conversation(chat, surface.owned_destination().unwrap())));
    assert!(surface.unavailable().is_none());
}

/// **The ordinary not-found fallback is unchanged**: a tab whose chat is
/// gone still owes a fresh chat, and makes one when its provider is ready.
#[test]
fn ordinary_not_found_still_makes_a_fresh_chat() {
    let mut surface = ChatSurface::new();
    let (reader, _commands, _updates) = Reader::test_pair();
    let gone = ChatId::new();
    assert!(surface.open_link(gone, None));
    surface.reader = Some(reader);
    boot(&mut surface, None, true);
    assert!(surface.unavailable().is_none());
    assert!(surface.fresh_chat_wanted, "a new chat is owed");
    ready(&mut surface, claude());
    assert!(
        surface.chat_unmade(),
        "the ready provider makes the new chat it was owed"
    );
}

/// **A model picked in an owned conversation is not the person's new-chat
/// default.** An ordinary surface remembers the agent it was moved to; an
/// owned one changes its own conversation and nothing else, and refuses
/// another agent outright.
#[test]
fn owned_choice_is_not_remembered_and_ordinary_choice_is() {
    let pick = |provider| ModelSelection {
        provider,
        requested_model: "default".into(),
        default_reasoning: ReasoningChoice::Default,
    };

    let mut ordinary = ChatSurface::new();
    let (reader, commands, _updates) = Reader::test_pair();
    ordinary.reader = Some(reader);
    boot(&mut ordinary, None, false);
    drained(&commands);
    ordinary.transcript = Some(chat_on(ChatId::new(), claude()));
    assert!(ordinary.start_chat_on(pick(codex())));
    assert!(
        names(&drained(&commands)).contains(&"select_provider"),
        "an ordinary choice is where the next chat starts"
    );

    let chat = ChatId::new();
    let (mut owned, commands) = owned_surface(chat, WorkspaceId::new());
    boot(&mut owned, Some(chat_on(chat, codex())), false);
    drained(&commands);
    assert!(owned.start_chat_on(pick(codex())));
    let queued = drained(&commands);
    assert_eq!(
        names(&queued),
        ["update_model"],
        "its own conversation only"
    );
    assert!(!owned.start_chat_on(pick(claude())), "never another agent");
    assert!(drained(&commands).is_empty());
}

/// **The controls the owner refuses are absent**, not drawn disabled: the
/// picker's rail offers the owner's agent alone, the `+` menu has neither a
/// folder by its path nor a working folder, the private mark has no rect,
/// and a send asks for no grant. Under all of them, the commands themselves
/// are refused, so a host's direct call reaches nothing either. The surface
/// publishes no accessibility node of its own for any of these; the menu's
/// rows are its items, so an absent item is absent from every reading.
#[test]
fn refused_controls_are_absent_and_their_commands_refused() {
    let chat = ChatId::new();
    let destination = WorkspaceId::new();
    let (mut owned, commands) = owned_surface(chat, destination);
    boot(&mut owned, Some(chat_on(chat, codex())), false);
    drained(&commands);

    // The rail: the owner's agent, nothing else, and hidden as a begun
    // chat's is.
    assert_eq!(
        owned
            .picker_rail()
            .iter()
            .map(|entry| entry.provider().cloned())
            .collect::<Vec<_>>(),
        [Some(codex())]
    );
    assert!(owned.picker_rail_hidden());
    assert!(owned.picker_marks().is_empty());

    // The `+` menu.
    owned.open_plus_menu();
    let items: Vec<_> = owned
        .plus_menu
        .as_ref()
        .expect("the menu opened")
        .items()
        .to_vec();
    assert!(
        !items
            .iter()
            .any(|item| matches!(item, PlusItem::AddFolder(_) | PlusItem::WorkInProject(..))),
        "no path reference and no working folder"
    );
    assert!(
        items
            .iter()
            .any(|item| matches!(item, PlusItem::UploadFiles(_))),
        "stored files are offered"
    );

    // The private mark, and the commands under every refused control.
    assert_eq!(owned.privacy_button(), None);
    owned.toggle_chat_privacy();
    owned.rename_chat(chat, "Renamed");
    owned.pin_chat(chat, true);
    owned.working_folder_chosen(chat, std::path::PathBuf::from("D:/elsewhere"));
    owned.update_tool_policy(ToolMode::Auto, vec![destination]);
    assert!(
        drained(&commands).is_empty(),
        "none of them reached the reader"
    );

    // The workspace is the owner's destination, whatever the window shows.
    assert!(!owned.set_context_workspace(Some(WorkspaceId::new())));
    assert_eq!(owned.context_workspace, Some(destination));

    // The ordinary `+` menu still has both.
    let mut ordinary = ChatSurface::new();
    ordinary.transcript = Some(chat_on(ChatId::new(), codex()));
    ordinary.open_plus_menu();
    let items = ordinary
        .plus_menu
        .as_ref()
        .expect("the menu opened")
        .items();
    assert!(
        items
            .iter()
            .any(|item| matches!(item, PlusItem::AddFolder(_)))
    );
    assert!(
        items
            .iter()
            .any(|item| matches!(item, PlusItem::WorkInProject(..)))
    );
}

/// An owner that admits a control gets it back: offers are the switch.
#[test]
fn an_offer_admits_its_control() {
    let chat = ChatId::new();
    let mut surface = ChatSurface::new_owned_at(None);
    surface.bind_owned(OwnedConversation {
        offers: OwnedOffers {
            rename: true,
            path_references: true,
            ..OwnedOffers::default()
        },
        ..conversation(chat, WorkspaceId::new())
    });
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.transcript = Some(chat_on(chat, codex()));
    surface.rename_chat(chat, "Renamed");
    assert_eq!(names(&drained(&commands)), ["rename"]);
    surface.open_plus_menu();
    assert!(
        surface
            .plus_menu
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .any(|item| matches!(item, PlusItem::AddFolder(_)))
    );
    surface.pin_chat(chat, true);
    assert!(drained(&commands).is_empty(), "pin is still refused");
}

/// **The path-reference offer, alone, is the reference choice and nothing
/// more**: with it a folder can be attached by its path and a file from the
/// computer asks whether to store it or attach its path; without it — the
/// default — neither is offered and the file is stored. Neither reaches a
/// grant or the working folder, which stay refused either way.
#[test]
fn the_path_reference_offer_alone_is_the_reference_choice() {
    let file = || {
        if cfg!(windows) {
            std::path::PathBuf::from("C:\\fixture\\notes.txt")
        } else {
            std::path::PathBuf::from("/fixture/notes.txt")
        }
    };
    let offered = OwnedOffers {
        path_references: true,
        ..OwnedOffers::default()
    };
    for (offers, expected) in [(offered, true), (OwnedOffers::default(), false)] {
        let chat = ChatId::new();
        let mut surface = ChatSurface::new_owned_at(None);
        surface.bind_owned(OwnedConversation {
            offers,
            ..conversation(chat, WorkspaceId::new())
        });
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(chat_on(chat, codex()));

        surface.open_plus_menu();
        let items = surface.plus_menu.as_ref().expect("the menu opened").items();
        assert_eq!(
            items
                .iter()
                .any(|item| matches!(item, PlusItem::AddFolder(_))),
            expected,
            "Add folder follows the offer"
        );
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, PlusItem::WorkInProject(..))),
            "the working folder is refused either way"
        );

        assert!(surface.computer_path_chosen(crate::ComputerPickerKind::File, file()));
        assert_eq!(
            surface.computer_file_choice.is_some(),
            expected,
            "the store-or-reference choice follows the offer"
        );
        assert!(
            !drained(&commands)
                .iter()
                .any(|command| matches!(command, Command::AddGrants(..))),
            "no grant either way"
        );
    }
}

/// Binding before activation makes the first boot read that conversation:
/// a host's bound surface never boots onto the service's latest chat.
#[test]
fn a_bound_surface_boots_on_its_conversation() {
    let chat = ChatId::new();
    let mut surface = ChatSurface::new_owned_at(None);
    assert_eq!(surface.scopes, crate::reader::Scopes::Owned);
    assert_eq!(ChatSurface::new().scopes, crate::reader::Scopes::Ordinary);
    surface.bind_owned(conversation(chat, WorkspaceId::new()));
    assert_eq!(surface.pending_link_chat, Some(chat));
    assert_eq!(surface.current_chat_id(), Some(chat));
    assert!(
        !surface.bind_owned(conversation(chat, surface.owned_destination().unwrap())),
        "the same binding again changes nothing"
    );
    assert!(!surface.open_link(ChatId::new(), None), "no other chat");
}

/// A message of `chat`, failed or complete, with its origin.
fn message(
    chat: ChatId,
    ordinal: u32,
    role: koru_ai_core::ChatRole,
    state: koru_ai_core::MessageState,
    origin: Option<MessageId>,
) -> koru_ai_core::Message {
    koru_ai_core::Message {
        id: MessageId::new(),
        chat_id: chat,
        ordinal,
        role,
        state,
        markdown: String::new(),
        origin_message_id: origin,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: Timestamp::from_millis(1),
        completed_at: None,
    }
}

/// A branch's transcript: a question and a failed reply copied from another
/// chat (their origins are that chat's messages), then a new question and
/// its failed reply, retried here once — the retry's origin is the failed
/// reply it replaced, a message of this chat.
struct Branch {
    transcript: ChatTranscript,
    copied_reply: MessageId,
    own_reply: MessageId,
    retried_reply: MessageId,
}

fn branch(chat: ChatId) -> Branch {
    use koru_ai_core::{ChatRole, MessageState};
    let mut transcript = chat_on(chat, codex());
    let copied_question = message(
        chat,
        0,
        ChatRole::User,
        MessageState::Complete,
        Some(MessageId::new()),
    );
    let copied_reply = message(
        chat,
        1,
        ChatRole::Assistant,
        MessageState::Failed,
        Some(MessageId::new()),
    );
    let question = message(chat, 2, ChatRole::User, MessageState::Complete, None);
    let own_reply = message(chat, 3, ChatRole::Assistant, MessageState::Failed, None);
    let retried_reply = message(
        chat,
        4,
        ChatRole::Assistant,
        MessageState::Failed,
        Some(own_reply.id),
    );
    let (copied, own, retried) = (copied_reply.id, own_reply.id, retried_reply.id);
    transcript.messages = vec![
        copied_question,
        copied_reply,
        question,
        own_reply,
        retried_reply,
    ];
    Branch {
        transcript,
        copied_reply: copied,
        own_reply: own,
        retried_reply: retried,
    }
}

/// **A Bot branch leaves Retry off a reply it copied** (QUESTIONS O34): the
/// Bot owner retries from the attempt behind a reply, and a branch copies
/// messages, not attempts, so that Retry could only answer not-found. The
/// copy is told apart by where its origin points — another chat — not by
/// having one: a reply this branch retried itself has an origin too, in this
/// chat, and keeps its Retry, as does a failed reply with none. The command
/// floor refuses the copied reply's Retry as well.
#[test]
fn an_owned_branch_offers_no_retry_on_a_copied_reply() {
    let chat = ChatId::new();
    let (mut surface, commands) = owned_surface(chat, WorkspaceId::new());
    let branch = branch(chat);
    surface.transcript = Some(branch.transcript.clone());
    assert!(
        !surface.offers_retry(branch.copied_reply),
        "copied: no Retry"
    );
    assert!(surface.offers_retry(branch.own_reply));
    assert!(
        surface.offers_retry(branch.retried_reply),
        "retried here: its origin is this chat's"
    );
    assert!(
        surface.offers_retry(MessageId::new()),
        "a reply not in the transcript yet is not a copy"
    );

    surface.perform_response_action(ResponseAction::Retry(branch.copied_reply));
    assert!(drained(&commands).is_empty(), "nothing queued for the copy");
    surface.perform_response_action(ResponseAction::Retry(branch.retried_reply));
    let Some(Command::Retry(_, request)) = commands.try_recv().ok() else {
        panic!("the branch's own reply is retried");
    };
    assert_eq!(request.message_id, branch.retried_reply);
}

/// **An ordinary surface is unchanged**: the same branch offers Retry on
/// every failed reply, the copied one included, and queues it.
#[test]
fn an_ordinary_branch_still_offers_retry_on_a_copied_reply() {
    let mut surface = ChatSurface::new();
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    let chat = ChatId::new();
    let branch = branch(chat);
    surface.transcript = Some(branch.transcript.clone());
    for reply in [branch.copied_reply, branch.own_reply, branch.retried_reply] {
        assert!(surface.offers_retry(reply));
    }
    surface.perform_response_action(ResponseAction::Retry(branch.copied_reply));
    let Some(Command::Retry(_, request)) = commands.try_recv().ok() else {
        panic!("an ordinary Retry is queued as before");
    };
    assert_eq!(request.message_id, branch.copied_reply);
}

// ---------------------------------------- a host-drawn empty transcript

/// A surface on an empty, read conversation, in a window-sized viewport.
fn empty_chat(owned: bool) -> (ChatSurface, Receiver<Command>, ChatId) {
    let chat = ChatId::new();
    let (mut surface, commands) = if owned {
        owned_surface(chat, WorkspaceId::new())
    } else {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        (surface, commands)
    };
    surface.viewport = Rect::new(0.0, 0.0, 720.0, 640.0);
    surface.transcript = Some(chat_on(chat, codex()));
    (surface, commands, chat)
}

/// The rooms a host's painter was given in one draw: one, or none.
fn drawn(surface: &mut ChatSurface) -> Vec<Rect> {
    let mut rooms = Vec::new();
    surface.draw_hosting(
        &mut vello::Scene::new(),
        vello::kurbo::Affine::IDENTITY,
        &mut |_, _, room| rooms.push(room),
        true,
    );
    rooms
}

/// **A host draws the empty transcript only where it asked to**: the
/// greeting goes, the composer sits where it does once a chat has begun,
/// and the painter gets the room above it, once per draw.
#[test]
fn a_host_draws_the_empty_transcript_in_the_greetings_place() {
    let (mut surface, _commands, _) = empty_chat(true);
    let centred = surface.composer_rect();
    assert!(surface.greeting().is_some(), "the chat's own greeting");
    assert_eq!(surface.hosted_empty_area(), None);
    assert!(drawn(&mut surface).is_empty(), "nothing asked: no painter");

    assert!(surface.host_empty_transcript(true));
    assert!(
        !surface.host_empty_transcript(true),
        "said twice, changes once"
    );
    assert_eq!(surface.greeting(), None, "the host's opening");
    let docked = surface.composer_rect();
    assert!(docked.y > centred.y + 50.0, "the composer is at the bottom");
    let room = surface.hosted_empty_area().expect("the host's room");
    assert!(room.y + room.h <= docked.y, "above the composer");
    assert_eq!(drawn(&mut surface), [room], "painted once, in that room");

    assert!(surface.host_empty_transcript(false));
    assert!(
        surface.greeting().is_some(),
        "given back: the greeting returns"
    );
    assert_eq!(surface.composer_rect(), centred);
}

/// **A chat whose host never asks is unchanged**, owned or not: the same
/// greeting, the same centred composer, no room, no painter call and no
/// press kept, whether it is drawn through `draw` or `draw_hosting`.
#[test]
fn a_surface_that_hosts_nothing_is_unchanged() {
    for owned in [false, true] {
        let (mut surface, _commands, _) = empty_chat(owned);
        let composer = surface.composer_rect();
        let greeting = surface.greeting().expect("an empty chat greets");
        surface.draw(&mut vello::Scene::new(), vello::kurbo::Affine::IDENTITY);
        assert!(drawn(&mut surface).is_empty(), "owned {owned}: no painter");
        assert_eq!(surface.composer_rect(), composer, "owned {owned}");
        assert_eq!(surface.greeting(), Some(greeting), "owned {owned}");
        assert_eq!(surface.hosted_empty_area(), None);
        let conversation = surface.conversation_rect();
        surface.mouse_down(conversation.x + 20.0, conversation.y + 20.0);
        assert_eq!(surface.take_hosted_press(), None, "owned {owned}");
        assert!(!surface.set_host_keyboard(true), "nothing hosted");
        assert!(!surface.host_holds_keyboard());
    }
}

/// **The host's content shows only over a conversation that is read and
/// holds nothing**: not before the read, not for another chat's
/// transcript, not once a reply is on its way, and not while the
/// conversation is unavailable.
#[test]
fn the_hosted_room_is_only_for_a_read_and_empty_conversation() {
    let (mut surface, _commands, chat) = empty_chat(true);
    surface.host_empty_transcript(true);
    assert_eq!(surface.transcript_is_empty(), Some(true));
    assert!(surface.hosted_empty_area().is_some());

    let read = surface.transcript.take();
    assert_eq!(surface.transcript_is_empty(), None, "not read yet");
    assert_eq!(surface.hosted_empty_area(), None);
    assert!(drawn(&mut surface).is_empty());

    surface.transcript = Some(chat_on(ChatId::new(), codex()));
    assert_eq!(surface.transcript_is_empty(), None, "another chat's");
    assert_eq!(surface.hosted_empty_area(), None);

    surface.transcript = read;
    surface.attempt = Some(koru_ai_core::InferenceAttemptId::new());
    assert_eq!(surface.transcript_is_empty(), Some(false), "a reply runs");
    assert_eq!(surface.hosted_empty_area(), None);
    assert!(drawn(&mut surface).is_empty());
    surface.attempt = None;

    surface.owned_unavailable(Unavailable::Unreachable);
    assert_eq!(surface.hosted_empty_area(), None);
    assert!(drawn(&mut surface).is_empty());
    assert_eq!(surface.owned().map(|owned| owned.chat), Some(chat));
}

/// **A press in the host's room is the host's, once**; one on the composer
/// is the composer's and is never handed over.
#[test]
fn a_press_in_the_hosted_room_is_kept_for_the_host() {
    let (mut surface, _commands, _) = empty_chat(true);
    surface.host_empty_transcript(true);
    let room = surface.hosted_empty_area().expect("the host's room");
    let at = (room.x + room.w * 0.5, room.y + room.h * 0.5);
    assert!(surface.mouse_down(at.0, at.1));
    assert_eq!(surface.take_hosted_press(), Some(at));
    assert_eq!(surface.take_hosted_press(), None, "taken once");

    let composer = surface.composer_rect();
    surface.mouse_down(composer.x + 24.0, composer.y + 16.0);
    assert_eq!(surface.take_hosted_press(), None, "the composer's own");
    assert!(surface.focus == crate::Focus::Composer);
}

/// **The composer is the ordinary composer while the host draws**: typing
/// reaches it, and it gives up its caret only while the host says its own
/// content holds the keyboard.
#[test]
fn typing_reaches_the_composer_while_the_host_draws() {
    let (mut surface, _commands, _) = empty_chat(true);
    surface.host_empty_transcript(true);
    surface.focus = crate::Focus::Composer;
    for typed in ["h", "i"] {
        assert!(surface.key(
            &winit::keyboard::Key::Character(typed.into()),
            Some(typed),
            winit::keyboard::ModifiersState::empty(),
        ));
    }
    assert_eq!(surface.composer.text(), "hi");
    assert!(!surface.host_holds_keyboard());
    assert!(surface.set_host_keyboard(true));
    assert!(surface.host_holds_keyboard());
    assert!(surface.set_host_keyboard(false));
    assert!(!surface.host_holds_keyboard());
}

/// **Nothing of the host's reaches the conversation.** Drawing the host's
/// content adds no message, and the first turn a hosted surface sends is,
/// field for field, the turn the same surface sends hosting nothing.
#[test]
fn nothing_the_host_draws_enters_the_transcript_or_a_send() {
    let send = |hosted: bool| {
        let (mut surface, commands, chat) = empty_chat(true);
        surface.host_empty_transcript(hosted);
        assert_eq!(drawn(&mut surface).len(), usize::from(hosted));
        let transcript = surface.transcript.as_ref().expect("read");
        assert!(transcript.messages.is_empty(), "no message was added");
        assert!(transcript.blocks.is_empty());
        assert!(surface.pending_user.is_none());
        surface.composer.set_text("Find three sources");
        assert!(surface.send_composer());
        let sent = drained(&commands);
        assert_eq!(names(&sent), ["send"], "one turn and nothing beside it");
        let Some(Command::Send(mut request)) = sent.into_iter().next() else {
            unreachable!()
        };
        assert_eq!(request.chat_id, chat);
        // What a turn mints for itself, and the chat it is for.
        request.operation_id = uuid::Uuid::nil();
        request.chat_id = ChatId::from_uuid(uuid::Uuid::nil());
        request.destination_workspace = None;
        request
    };
    let (plain, hosted) = (send(false), send(true));
    assert_eq!(plain.markdown, "Find three sources");
    assert_eq!(hosted, plain, "the same turn, hosted or not");
}
