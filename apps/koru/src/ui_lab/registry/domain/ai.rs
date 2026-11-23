// SPDX-License-Identifier: MPL-2.0

//! **`koru-ai-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-ai-ui";

const AGENTS: &str = "crates/koru-ai-ui/src/agents.rs";
const APPROVAL_MODES: &str = "crates/koru-ai-ui/src/approval_modes.rs";
/// Where the approval modes' vocabulary moved (Bots P1.8 S2c); `koru_ai_ui`
/// re-exports it under its old path.
const AGENT_CONFIG_MODES: &str = "crates/koru-agent-config-ui/src/modes.rs";
const ATTACHMENTS: &str = "crates/koru-ai-ui/src/attachments.rs";
const CHAT_STATUS: &str = "crates/koru-ai-ui/src/chat_status.rs";
const COMPOSER: &str = "crates/koru-ai-ui/src/composer.rs";
const COMPOSER_PLUS: &str = "crates/koru-ai-ui/src/composer_plus.rs";
const COMPOSER_SHELL: &str = "crates/koru-ai-ui/src/composer_shell.rs";
const DIAGRAMS: &str = "crates/koru-ai-ui/src/diagrams.rs";
const DICTATION: &str = "crates/koru-ai-ui/src/dictation.rs";
const LIB: &str = "crates/koru-ai-ui/src/lib.rs";
const MICROPHONE: &str = "crates/koru-ai-ui/src/microphone.rs";
const MODEL_PICKER: &str = "crates/koru-ai-ui/src/model_picker.rs";
const NEW_DIVIDER: &str = "crates/koru-ai-ui/src/new_divider.rs";
const TOOL_OUTPUT: &str = "crates/koru-ai-ui/src/tool_output.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: new()/new_at(root) build a TextEngine, the composer's
    // private in-memory workspace and editor, the attachment event channel and
    // all panel state, and read the system locale; no thread and no service
    // client start until activate(), which is a no-op under cfg(test) or the
    // no-client feature. Every visible state after that comes from the AI
    // service through the background reader.
    record(
        "C-AI-CHAT-SURFACE",
        "ChatSurface",
        Key::UiLabAiChatName,
        Key::UiLabAiChatPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &["ComposerQueue"],
        source(OWNER, "koru_ai_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_ai_ui::ChatSurface"),
            painter("koru_ai_ui::ChatSurface::new"),
            painter("koru_ai_ui::ChatSurface::new_at"),
            painter("koru_ai_ui::ChatSurface::draw"),
            painter("koru_ai_ui::ChatSurface::draw_with_caret_phase"),
            support("koru_ai_ui::ChatSurface::activate"),
            support("koru_ai_ui::ChatSurface::set_viewport"),
            support("koru_ai_ui::ChatSurface::key"),
            support("koru_ai_ui::ChatSurface::mouse_down"),
            support("koru_ai_ui::ChatSurface::mouse_move"),
            support("koru_ai_ui::ChatSurface::mouse_up"),
            support("koru_ai_ui::ChatSurface::mouse_double_click"),
            support("koru_ai_ui::ChatSurface::scroll_by"),
            support("koru_ai_ui::ChatSurface::cursor_icon"),
            support("koru_ai_ui::ChatSurface::set_mods"),
            support("koru_ai_ui::ChatSurface::dismiss_popovers"),
            support("koru_ai_ui::ChatSurface::animating"),
            support("koru_ai_ui::ChatSurface::maintain"),
            support("koru_ai_ui::ChatSurface::drain_updates"),
            support("koru_ai_ui::ChatSurface::invalidate"),
            support("koru_ai_ui::ChatSurface::set_reduced_motion"),
            support("koru_ai_ui::ChatSurface::start_new_chat"),
            support("koru_ai_ui::ChatSurface::open_link"),
            support("koru_ai_ui::ChatSurface::set_context_document"),
            support("koru_ai_ui::ChatSurface::set_context_workspace"),
            support("koru_ai_ui::ChatSurface::take_open_document_request"),
            support("koru_ai_ui::ChatSurface::take_place_document_request"),
            support("koru_ai_ui::WorkingFolderRequest"),
            support("koru_ai_ui::ChatSurface::take_working_folder_request"),
            support("koru_ai_ui::ChatSurface::working_folder_chosen"),
            support("koru_ai_ui::ChatSurface::maintenance_deadline"),
            support("koru_ai_ui::ChatSurface::is_question_navigation_chord"),
            support("koru_ai_ui::ChatSurface::link_card_buttons"),
            support("koru_ai_ui::ChatSurface::jump_to_latest"),
            support("koru_ai_ui::ChatSurface::context_menu"),
        ],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // message layouts are rebuilt from the transcript the reader delivers
    // (layout_cache, live_markdown).
    record(
        "C-AI-CONVERSATION",
        "draw_messages",
        Key::UiLabAiConversationName,
        Key::UiLabAiConversationPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::ChatSurface::conversation_scroll"),
            painter("koru_ai_ui::ChatSurface::restore_conversation_scroll"),
        ],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // Shape::of and Call::of classify a row's text into Pieces, pure; layout
    // lays them out once per change.
    record(
        "C-AI-TOOL-CALL-DETAIL",
        "tool_output::Piece",
        Key::UiLabAiToolCallDetailName,
        Key::UiLabAiToolCallDetailPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", TOOL_OUTPUT),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_ai_ui::ChatSurface::offer_reveal_paths"),
            support("koru_ai_ui::ChatSurface::take_reveal_path_request"),
        ],
    ),
    // Construction: pure: the host's show_new_from(chat, after) sets it; the
    // conversation layout places it
    record(
        "C-AI-NEW-DIVIDER",
        "NewDivider",
        Key::UiLabAiNewDividerName,
        Key::UiLabAiNewDividerPurpose,
        Family::Controls,
        Kind::Widget,
        None,
        &[],
        source(OWNER, "koru_ai_ui", NEW_DIVIDER),
        DESKTOP_NOT_ON_PHONES,
        &[painter("koru_ai_ui::ChatSurface::show_new_from")],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // composer_shell::layout builds the element tree each frame from measured
    // heights, pure.
    record(
        "C-AI-COMPOSER",
        "KoruComposer",
        Key::UiLabAiComposerName,
        Key::UiLabAiComposerPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", COMPOSER_SHELL),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_ai_ui::ChatSurface::attach_document"),
            support("koru_ai_ui::ChatSurface::dictation_anchor"),
            support("koru_ai_ui::ChatSurface::submit_embedded_when_durable"),
        ],
    ),
    // Construction: ChatComposer::new is pub(crate): it creates a private
    // in-memory Koru workspace with one document and a koru-editor Editor over
    // it (no Store, no disk); a host reaches it only through
    // ChatSurface::dictation_composer, to route dictation
    // (koru_ui::DictationTarget with a () context).
    record(
        "C-AI-COMPOSER-DOCUMENT",
        "ChatComposer",
        Key::UiLabAiComposerTextName,
        Key::UiLabAiComposerTextPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", COMPOSER),
        DESKTOP_NOT_ON_PHONES,
        &[
            boundary("koru_ai_ui::ChatComposer"),
            boundary("koru_ai_ui::ChatComposer::text"),
            boundary("koru_ai_ui::ComposedMessage"),
            boundary("koru_ai_ui::ChatSurface::dictation_composer"),
        ],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // the host feeds recents (set_recent_documents) and performs the native
    // pickers it asks for.
    record(
        "C-AI-COMPOSER-PLUS-MENU",
        "PlusItem",
        Key::UiLabAiPlusMenuName,
        Key::UiLabAiPlusMenuPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", COMPOSER_PLUS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support_with(
                "koru_ai_ui::ComputerPickerKind",
                &[values(
                    Key::UiLabControlKind,
                    "koru_ai_ui::ComputerPickerKind",
                    COMPOSER_PLUS,
                    Declared::Enum,
                    &[literal("File"), literal("Folder"), literal("WorkspaceFile")],
                )],
            ),
            support("koru_ai_ui::WorkspaceImportRequest"),
            painter("koru_ai_ui::ChatSurface::set_recent_documents"),
            support("koru_ai_ui::ChatSurface::take_computer_picker_request"),
            support("koru_ai_ui::ChatSurface::computer_path_chosen"),
            support("koru_ai_ui::ChatSurface::take_workspace_import_request"),
            support("koru_ai_ui::ChatSurface::workspace_file_imported"),
        ],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // attachments::Work starts upload and thumbnail threads lazily, on the
    // first paste, store or thumbnail fetch, never at construction.
    record(
        "C-AI-ATTACHMENT-STRIP",
        "draw_strip",
        Key::UiLabAiAttachmentsName,
        Key::UiLabAiAttachmentsPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", ATTACHMENTS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::AttachmentOpenRequest"),
            painter_with(
                "koru_ai_ui::AttachmentPreview",
                &[values(
                    Key::UiLabControlKind,
                    "koru_ai_ui::AttachmentPreview",
                    ATTACHMENTS,
                    Declared::Enum,
                    &[literal("Text"), literal("Unsupported")],
                )],
            ),
            support("koru_ai_ui::ChatSurface::take_attachment_open_request"),
        ],
    ),
    // Construction: pure: ViewerLift is two (f32, f32) pairs; the viewer
    // itself is koru_ui::media_viewer::MediaViewer, opened by a press on a
    // picture or a diagram
    record(
        "H-AI-VIEWER-LIFT",
        "ViewerLift",
        Key::UiLabAiViewerLiftName,
        Key::UiLabAiViewerLiftPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_ai_ui", ATTACHMENTS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::ViewerLift"),
            support("koru_ai_ui::ChatSurface::set_viewer_lift"),
            support("koru_ai_ui::ChatSurface::viewer_open"),
            painter("koru_ai_ui::ChatSurface::draw_lifted_viewer"),
            support("koru_ai_ui::ChatSurface::viewer_semantics"),
            support("koru_ai_ui::ChatSurface::viewer_act"),
            support("koru_ai_ui::ChatSurface::viewer_zoom_key"),
            support("koru_ai_ui::ChatSurface::viewer_zoom_wheel"),
        ],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // PickerState::new(engine) is pub(crate) and makes two text inputs.
    record(
        "C-AI-MODEL-PICKER",
        "draw_model_picker",
        Key::UiLabAiModelPickerName,
        Key::UiLabAiModelPickerPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", MODEL_PICKER),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::FavoriteModel"),
            support("koru_ai_ui::FavoriteModel::new"),
            painter("koru_ai_ui::ChatSurface::set_favorite_models"),
            support("koru_ai_ui::ChatSurface::take_favorite_models_request"),
            support("koru_ai_ui::ChatSurface::offer_agent_settings"),
            support("koru_ai_ui::ChatSurface::ask_for_agent_settings"),
            support("koru_ai_ui::ChatSurface::take_agent_settings_request"),
        ],
    ),
    // Construction: pure: MicrophoneDevice::new(name, default) and Microphones
    // are plain values the host's voice worker fills; the panel only asks
    // (MicrophoneAsk) and never touches a device
    record(
        "C-AI-MICROPHONE-PANEL",
        "draw_microphone_panel",
        Key::UiLabAiMicrophoneName,
        Key::UiLabAiMicrophonePurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", MICROPHONE),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::MicrophoneDevice"),
            support("koru_ai_ui::MicrophoneDevice::new"),
            support("koru_ai_ui::Microphones"),
            support_with(
                "koru_ai_ui::MicrophoneAsk",
                &[values(
                    Key::UiLabControlValues,
                    "koru_ai_ui::MicrophoneAsk",
                    MICROPHONE,
                    Declared::Enum,
                    &[literal("List"), literal("Choose"), literal("Hold")],
                )],
            ),
            support("koru_ai_ui::ChatSurface::offer_microphone_choice"),
            painter("koru_ai_ui::ChatSurface::set_microphones"),
            support("koru_ai_ui::ChatSurface::take_microphone_asks"),
            support("koru_ai_ui::ChatSurface::microphone_panel_open"),
            support("koru_ai_ui::ChatSurface::microphone_meter_wanted"),
            support("koru_ai_ui::ChatSurface::set_microphone_level"),
        ],
    ),
    // Construction: pure: the host pushes DictationShown (level, listening,
    // finishing) and None at the end; no audio, device or model is touched
    // here
    record(
        "C-AI-DICTATION-ROW",
        "ComposerDictation",
        Key::UiLabAiDictationRowName,
        Key::UiLabAiDictationRowPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", DICTATION),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::DictationShown"),
            support_with(
                "koru_ai_ui::DictationPress",
                &[values(
                    Key::UiLabControlValues,
                    "koru_ai_ui::DictationPress",
                    DICTATION,
                    Declared::Enum,
                    &[literal("Stop"), literal("Cancel")],
                )],
            ),
            support("koru_ai_ui::ChatSurface::offer_dictation"),
            support("koru_ai_ui::ChatSurface::take_wants_dictation"),
            support("koru_ai_ui::ChatSurface::dictating"),
            painter("koru_ai_ui::ChatSurface::show_dictation"),
            support("koru_ai_ui::ChatSurface::dictation_press_at"),
        ],
    ),
    // Construction: built inside ChatSurface::new_at with the rest of the
    // surface's state; no public constructor, drawn only by ChatSurface::draw;
    // shown instead of the chat only after open_setup().
    record(
        "C-AI-PROVIDER-SETUP",
        "draw_setup",
        Key::UiLabAiProviderSetupName,
        Key::UiLabAiProviderSetupPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_ai_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_ai_ui::ChatSurface::open_setup"),
            support("koru_ai_ui::ChatSurface::setup_open"),
            support_with(
                "koru_ai_ui::OpenAiCredentialRequest",
                &[values(
                    Key::UiLabControlValues,
                    "koru_ai_ui::OpenAiCredentialRequest",
                    LIB,
                    Declared::Enum,
                    &[literal("Save"), literal("Clear")],
                )],
            ),
            support("koru_ai_ui::OpenAiCredentialRequest::into_secret"),
            support("koru_ai_ui::ChatSurface::take_openai_credential_request"),
            support("koru_ai_ui::ChatSurface::openai_credential_saved"),
        ],
    ),
    // Construction: pure reads of the surface's state
    record(
        "H-AI-CHAT-STATUS",
        "ChatSurface::chat_status",
        Key::UiLabAiChatStatusName,
        Key::UiLabAiChatStatusPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_ai_ui", CHAT_STATUS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::ChatSurface::chat_status"),
            support("koru_ai_ui::ChatSurface::is_working"),
            support("koru_ai_ui::ChatSurface::needs_you"),
            support("koru_ai_ui::ChatSurface::finished_replies"),
            support("koru_ai_ui::ChatSurface::take_arrival"),
            support("koru_ai_ui::ChatSurface::take_activity"),
            support("koru_ai_ui::ChatSurface::newest_seen_message"),
        ],
    ),
    // Construction: pure: a reply's closed diagram code blocks queue a
    // request; a host without the worker leaves the source visible
    record(
        "H-AI-CHAT-DIAGRAMS",
        "Diagrams",
        Key::UiLabAiDiagramsName,
        Key::UiLabAiDiagramsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_ai_ui", DIAGRAMS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::ChatSurface::take_diagram_requests"),
            support("koru_ai_ui::ChatSurface::diagram_result"),
            support("koru_ai_ui::ChatSurface::take_diagram_copy"),
            support("koru_ai_ui::ChatSurface::take_diagram_save"),
        ],
    ),
    // Construction: pure: plain values a host copies from its Agent providers
    // settings
    record(
        "H-AI-AGENT-PROVIDERS",
        "AgentDefaults",
        Key::UiLabAiAgentDefaultsName,
        Key::UiLabAiAgentDefaultsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_ai_ui", AGENTS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_ai_ui::AgentDefaults"),
            support_with(
                "koru_ai_ui::NewChatPermission",
                &[values(
                    Key::UiLabControlMode,
                    "koru_ai_ui::NewChatPermission",
                    AGENT_CONFIG_MODES,
                    Declared::Enum,
                    &[literal("Manual"), literal("Auto"), literal("Yolo")],
                )],
            ),
            support("koru_ai_ui::ChatSurface::set_agent_defaults"),
            support("koru_ai_ui::ChatSurface::set_hidden_agents"),
            support("koru_ai_ui::ChatSurface::set_agent_order"),
            support("koru_ai_ui::agent_key"),
            support("koru_ai_ui::provider_mark"),
        ],
    ),
];
