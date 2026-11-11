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
