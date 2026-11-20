// SPDX-License-Identifier: MPL-2.0

//! Real window routing and AccessKit projection over an in-memory transcript.
use super::*;
use crate::ai_chat_view::AiChatView;
use crate::mainview::{harness, tab::Tab};
use koru_ai_core::*;
use koru_workspace_host::accessibility::{Scope, ViewSlot, WindowSlot};
use winit::keyboard::NamedKey;

fn transcript(chat: ChatId, reply: MessageId) -> koru_ai_ui::TestChatTranscript {
    let now = Timestamp::from_millis(1);
    let model = ModelSelection {
        provider: AiProviderId::parse("agent-codex").unwrap(),
        requested_model: "default".into(),
        default_reasoning: ReasoningChoice::Default,
    };
    koru_ai_ui::TestChatTranscript {
        active_attempt: None,
        chat: Chat {
            id: chat,
            title: "Scout".into(),
            private: false,
            current_model: model,
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
            chat_id: chat,
            mode: ToolMode::Manual,
            workspace_ids: vec![],
        },
        messages: vec![Message {
            id: reply,
            chat_id: chat,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: "Here is the result.".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: Some(now),
        }],
        tool_activities: vec![ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: chat,
            assistant_message_id: reply,
            tool_name: "agent.read".into(),
            read_only: true,
            status: ToolActivityStatus::Complete,
            approval: ToolApproval::NotRequired,
            proposed_at: now,
            started_at: Some(now),
            completed_at: Some(now),
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: Some("Read the retained fixture".into()),
            agent_summary: None,
            agent_input: Some("fixture input".into()),
            agent_output: Some("retained output line\n".repeat(300)),
            text_offset: None,
            display_target: None,
            live: None,
        }],
        sources: vec![],
        notices: vec![],
        blocks: vec![],
        pinned_documents: vec![],
        feedback: vec![],
        usage: vec![],
        reasoning_control: None,
        agent_commands: None,
        attachments: vec![],
    }
}

fn fixture() -> (MainView, koru_workspace::Workspace) {
    let (mut view, workspace) = harness::container();
    view.seat_accessibility(WindowSlot::from_raw(11));
    let chat = ChatId::new();
    let reply = MessageId::new();
    let mut chat_view = AiChatView::owned(koru_ai_ui::OwnedConversation {
        chat,
        destination: koru_store_core::WorkspaceId::new(),
        provider: AiProviderId::parse("agent-codex").unwrap(),
        offers: Default::default(),
        refusals: &[],
    });
    let surface = chat_view.dictation_surface_mut();
    surface.pretend_transcript(transcript(chat, reply));
    surface.set_viewport(Rect::new(0.0, 0.0, 700.0, 300.0));
    surface.draw(&mut Scene::new(), Affine::IDENTITY);
    let anchor = surface
        .bot_work_anchors()
        .into_iter()
        .next()
        .expect("retained reply anchor");
    assert!(surface.activate_bot_work_anchor(anchor.key));
    let mut tab = Tab::welcome();
    tab.view = TabView::AiChat(chat_view);
    view.tabs_mut().push(tab);
    view.set_viewport(1000.0, 400.0);
    view.sync_viewer_lift();
    view.draw_bot_work(&mut Scene::new(), Affine::IDENTITY);
    assert_eq!(view.blockers(), Blockers::Modal(ModalKind::BotWork));
    (view, workspace)
}

#[test]
fn window_modal_blocks_chrome_draft_and_outside_click_through() {
    let (mut view, mut workspace) = fixture();
    let tab = view.tabs().active_tab().id;
    let count = view.tabs().len();
    for (key, text, mods) in [
        (Key::Character("w".into()), None, ModifiersState::CONTROL),
        (Key::Named(NamedKey::F6), None, ModifiersState::empty()),
        (
            Key::Character("x".into()),
            Some("x"),
            ModifiersState::empty(),
        ),
        (Key::Named(NamedKey::Enter), None, ModifiersState::CONTROL),
    ] {
        assert!(MainView::key(
            &mut view,
            &mut workspace.ws,
            &key,
            text,
            mods
        ));
    }
    assert_eq!(view.tabs().len(), count);
    assert_eq!(view.tabs().active_tab().id, tab);
    assert!(view.bot_work_open());
    assert!(
        view.bot_work_surface_mut()
            .unwrap()
            .dictation_composer()
            .text()
            .is_empty()
    );
    assert!(View::mouse_down(
        &mut view,
        &mut workspace.ws,
        0.0,
        0.0,
        false
    ));
    assert!(!view.bot_work_open());
    assert!(View::mouse_up(&mut view, &mut workspace.ws));
    assert_eq!(view.tabs().len(), count);
    assert_eq!(view.tabs().active_tab().id, tab);
}

#[test]
fn dialog_accessibility_and_focus_follow_retained_controls_and_local_scroll() {
    let (mut view, mut workspace) = fixture();
    let slot = ViewSlot::from_raw(7);
    let revision = view.semantics_revision();
    let close = view.accessibility_focus(slot);
    assert!(MainView::key(
        &mut view,
        &mut workspace.ws,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty()
    ));
    assert_ne!(revision, view.semantics_revision());
    assert_ne!(close, view.accessibility_focus(slot));
    let tree = view.semantics(slot, Scope::Whole);
    fn contains(nodes: &[koru_workspace_host::accessibility::SemanticNode], part: &str) -> bool {
        nodes
            .iter()
            .any(|node| node.occurrence.part.name() == part || contains(&node.children, part))
    }
    assert!(contains(&tree, "bot-work"));
    assert!(contains(&tree, "bot-work-call"));
    // Calls start collapsed; Enter expands the focused retained call before
    // the long output can be scrolled independently of the conversation.
    assert!(MainView::key(
        &mut view,
        &mut workspace.ws,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    let before = view.semantics_revision();
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, -20.0));
    assert_ne!(before, view.semantics_revision());
    assert!(view.bot_work_open());
    assert!(MainView::key(
        &mut view,
        &mut workspace.ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert!(!view.bot_work_open());
    assert_eq!(
        view.accessibility_focus(slot).unwrap().part.name(),
        "bot-work-anchor"
    );
}
