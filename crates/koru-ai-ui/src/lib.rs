// SPDX-License-Identifier: MPL-2.0

//! Reusable Koru AI chat and provider-setup surface.
//!
//! The surface owns pixels and interaction state. All service work crosses the
//! bounded background client in [`reader`]; the winit thread never discovers a
//! gateway, reads Store, opens SQLite, or calls a provider.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_ai_core::{
    AiErrorClass, AiProviderId, AiSurface, AiUsage, Chat, ChatRole, ContextRef,
    DocumentDestination, DocumentExportId, DocumentId, FeedbackEventId, FeedbackValue,
    InferenceAttemptId, MessageBlockId, MessageId, MessageState, ModelSelection, ReasoningChoice,
    ReasoningControl, SourceLink, ToolActivity, ToolActivityStatus, ToolApproval, ToolInvocationId,
    ToolMode, UsageReport, WebSearchPhase, WorkspaceId,
};

/// Re-exported so a host can name the id [`ChatSurface::open_link`] and
/// [`ChatSurface::recent_chats`] carry without taking a `koru-ai-core`
/// dependency of its own.
pub use koru_ai_core::ChatId;
use koru_ai_wire::{
    BranchChatRequest, ChatTranscript, CreateChatRequest, ExportChatRequest, FeedbackRequest,
    ModelPullEvent, ProviderSettingsResponse, ProviderValidationResponse, ProviderValidationStatus,
    PublicAiEvent, PullOllamaModelRequest, RenameChatRequest, RetryTurnRequest,
    SelectProviderRequest, SendTurnRequest, UpdateChatModelRequest, UpdateChatToolPolicyRequest,
    UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};
use koru_draw::{scene, theme::role};
use koru_ui::button::{ActionButton, ActionStyle, ButtonRole};
use koru_ui::confirm::{Confirm, Outcome as ConfirmOutcome};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::tooltip::{Tooltip, TooltipContent};
use koru_ui::{Menu, PulsingDots, TextArea, Typed};
use parley::Layout;
use parley::editing::{Cursor, Selection};
use parley::layout::{Affinity, Cluster};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

/// One line of the composer's text: the app's text line at the current
/// density ([`theme::DocMetrics::ui`]). The document text size does not
/// reach the chat, so this does not follow it.
pub(crate) fn chat_line_height() -> f32 {
    theme::DocMetrics::ui().line
}

mod agents;
mod chats_pane;
mod composer;
mod doc_links;
mod effort;
mod layout_cache;
mod privacy;
mod quote_pills;
mod reader;
mod sources;
mod tool_groups;
mod usage;

pub use composer::{ChatComposer, ComposedMessage};

use agents::{
    AgentStatus, PickerRow, agent_chip_label, agent_row_body, agent_row_title, is_agent_row,
};
use koru_ai_core::AGENT_DEFAULT_MODEL;
use layout_cache::{BlockLayout, MessageInput, MessageLayout, RebuildStats};
use quote_pills::{
    CommentEditor, ContextDocumentPill, PendingQuote, PillStrip, ResponseMenuItem,
    compose_markdown, pill_band_height,
};
use reader::{Command, DestinationOption, POLL_INTERVAL, Reader, SendError, Update};
use usage::{format_usage_report, message_content_height, message_height, usage_line};

const PAD: f32 = 14.0;
const HEADER_H: f32 = 46.0;
const FIELD_H: f32 = 36.0;
const BUTTON_H: f32 = ActionButton::H;
const CARD_GAP: f32 = 10.0;
const COMPOSER_GAP: f32 = 8.0;
const COMPOSER_MIN_LINES: usize = 1;
const COMPOSER_MAX_LINES: usize = 8;
const MIN_COMPOSER_W: f32 = 80.0;
const HISTORY_ROW_H: f32 = 40.0;
const MODEL_VISIBLE_ROWS: usize = 8;
const ACTION_H: f32 = 28.0;
const BLOCK_GAP: f32 = 8.0;
const USAGE_GAP: f32 = 4.0;
const HIGHLIGHT_FOR: Duration = Duration::from_secs(2);
const CONVERSATION_SCROLL_SPEED: f32 = 1.8;
const CONTENT_MAX_W: f32 = 800.0;
const USER_MESSAGE_MAX_FRACTION: f32 = 0.70;
/// The composer's toolbar row — the private mark, the spaces, the mode, the
/// model chip and the send button: one control tall.
const COMPOSER_TOOLBAR_H: f32 = BUTTON_H;
/// **The composer's inner padding**, one constant per side, read by the
/// layout ([`ChatSurface::composer_rect`]), by the editor's placement inside
/// the box ([`composer`]), and by every control and hit-test that derives
/// from the box, so none of them can disagree.
///
/// The text sits `COMPOSER_PAD_X` in from the left and right and
/// `COMPOSER_PAD_TOP` below the top; the toolbar row is inset by the same
/// `COMPOSER_PAD_X` and sits `COMPOSER_PAD_BOTTOM` above the bottom, with
/// `COMPOSER_TEXT_GAP` between the last line and the row. It used to be the
/// chrome field padding, 7 px, which left the words flush with the border.
const COMPOSER_PAD_X: f32 = 16.0;
const COMPOSER_PAD_TOP: f32 = 12.0;
const COMPOSER_TEXT_GAP: f32 = 4.0;
const COMPOSER_PAD_BOTTOM: f32 = 8.0;
/// **The quiet row under the composer box**, left-aligned with it: an agent
/// chat's folder, small and dim, the way a checkout is named under a prompt.
/// `COMPOSER_FOOTER_GAP` below the box, `COMPOSER_FOOTER_H` tall; a chat with
/// nothing to show there gives it no room at all
/// ([`ChatSurface::composer_footer_rect`]).
const COMPOSER_FOOTER_GAP: f32 = 4.0;
const COMPOSER_FOOTER_H: f32 = 22.0;
/// How long a first Escape during a turn waits for the second that stops
/// it. Past it the hint goes away and the next Escape starts over.
const STOP_ESCAPE_WINDOW: Duration = Duration::from_millis(1500);
/// Between an empty chat's greeting and the composer under it.
const GREETING_GAP: f32 = 20.0;
/// Where the setup fields start: below four provider cards.
const SETUP_FIELDS_Y: f32 = 330.0;
/// The title a chat is made with, until its first message names it.
const DEFAULT_CHAT_TITLE: &str = "New chat";
/// The greetings an empty chat opens on, one picked at random per chat.
const GREETINGS: [koru_i18n::Key; 20] = [
    koru_i18n::Key::AiGreeting01,
    koru_i18n::Key::AiGreeting02,
    koru_i18n::Key::AiGreeting03,
    koru_i18n::Key::AiGreeting04,
    koru_i18n::Key::AiGreeting05,
    koru_i18n::Key::AiGreeting06,
    koru_i18n::Key::AiGreeting07,
    koru_i18n::Key::AiGreeting08,
    koru_i18n::Key::AiGreeting09,
    koru_i18n::Key::AiGreeting10,
    koru_i18n::Key::AiGreeting11,
    koru_i18n::Key::AiGreeting12,
    koru_i18n::Key::AiGreeting13,
    koru_i18n::Key::AiGreeting14,
    koru_i18n::Key::AiGreeting15,
    koru_i18n::Key::AiGreeting16,
    koru_i18n::Key::AiGreeting17,
    koru_i18n::Key::AiGreeting18,
    koru_i18n::Key::AiGreeting19,
    koru_i18n::Key::AiGreeting20,
];
/// How long a title made from a first message may be, in characters.
const AUTO_TITLE_CHARS: usize = 48;
const TOOL_ACTIVITY_ROW_H: f32 = 34.0;
const TOOL_ACTIVITY_MIN_EXPANDED_H: f32 = 60.0;
const TOOL_ACCESS_WORKSPACE_ROW_H: f32 = 32.0;
const TOOL_ACCESS_VISIBLE_ROWS: usize = 6;

/// Where the shared surface is hosted. It is recorded on every attempt.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SurfaceMode {
    Standalone,
    ContextPane,
}

impl SurfaceMode {
    const fn wire(self) -> AiSurface {
        match self {
            Self::Standalone => AiSurface::Standalone,
            Self::ContextPane => AiSurface::ContextPane,
        }
    }
}

/// A capability that must remain outside the public AI HTTP contract.
///
/// The desktop host consumes this immediately and writes or removes the OS
/// vault entry. A carried secret must never be logged, persisted, cloned, or
/// serialized.
pub enum OpenAiCredentialRequest {
    Save(String),
    Clear,
}

impl OpenAiCredentialRequest {
    /// `Some(secret)` saves/replaces the key; `None` removes it. Taking the
    /// string leaves only an empty allocation for `Drop` to clear.
    pub fn into_secret(mut self) -> Option<String> {
        match &mut self {
            Self::Save(secret) => Some(std::mem::take(secret)),
            Self::Clear => None,
        }
    }
}

impl Drop for OpenAiCredentialRequest {
    fn drop(&mut self) {
        // Best-effort clearing of the String's initialized bytes. The host
        // should consume this immediately into the provider's zeroizing type.
        if let Self::Save(secret) = self {
            unsafe { secret.as_bytes_mut().fill(0) };
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    None,
    Composer,
    WorkspaceSearch,
    OpenAiKey,
    OpenAiModel,
    OllamaEndpoint,
    OllamaModel,
    SystemPrompt,
}

/// A folder pick the host performs for an agent chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingFolderRequest {
    pub chat: ChatId,
    /// Where the picker starts: the chat's folder when one was chosen. The
    /// dialog's title is the host's, through `koru-i18n`.
    pub start: Option<std::path::PathBuf>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SetupProvider {
    Koru,
    OpenAi,
    Ollama,
    /// The *Local agents* card: which agent is picked lives in `setup_agent`.
    Agents,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DestructiveAction {
    /// One chat, or the pane's pick (the CTO's note: several at once).
    DeleteChats(Vec<ChatId>),
    ClearHistory,
    ClearUsage,
}

enum ResponseAction {
    Copy(String),
    Rate(MessageId, FeedbackValue),
    Branch(MessageId),
    Store(MessageId),
    Link(MessageBlockId),
    Retry(MessageId),
}

struct ToolActivityLayout {
    activity: ToolActivity,
    before_message: Option<MessageId>,
    top: f32,
    title: Layout<Brush>,
    detail: Layout<Brush>,
    expanded_height: f32,
    /// Inside a collapsed box: laid out, never drawn or hit.
    hidden: bool,
}

/// One turn's tool rows as a box: the heading that sums them up, above the
/// rows it folds (`tool_groups`).
struct ToolGroupLayout {
    message: MessageId,
    top: f32,
    title: Layout<Brush>,
}

#[derive(Clone)]
struct ImageSpec {
    url: String,
    alt: String,
}

struct LoadedImage {
    brush: vello::peniko::ImageBrush,
    width: u32,
    height: u32,
}

type ImageResult = Result<LoadedImage, String>;

struct RemoteImage {
    pending: Option<Arc<Mutex<Option<ImageResult>>>>,
    ready: Option<LoadedImage>,
    failed: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ResponsePosition {
    message: usize,
    block: Option<usize>,
    byte: usize,
    affinity: Affinity,
}

struct ResponseSelection {
    anchor: ResponsePosition,
    focus: ResponsePosition,
    dragging: bool,
    pressed_link: Option<String>,
}

struct ResponseTextHit {
    position: ResponsePosition,
    selection: Selection,
    link: Option<String>,
}

/// One AI conversation surface, usable as a window body or in Koru's context
/// column. Constructing it in tests starts no service client.
pub struct ChatSurface {
    mode: SurfaceMode,
    viewport: Rect,
    engine: TextEngine,
    root: Option<PathBuf>,
    reader: Option<Reader>,
    waiting: bool,
    settings: Option<ProviderSettingsResponse>,
    chats: Vec<Chat>,
    /// **The Chats pane is open** — the standalone window's floating host of
    /// [`koru_chats_ui::ChatsSurface`] (see [`chats_pane`]). Only ever true in
    /// [`SurfaceMode::Standalone`]; the name is the old popover's, kept so
    /// the many pickers that close it still read the same.
    history_open: bool,
    chats_pane: koru_chats_ui::ChatsSurface,
    /// Whether the pane's reader has been started — on its first opening, so
    /// a window whose list was never opened dials nothing for it.
    chats_pane_started: bool,
    /// A chat being renamed in the pane: the field over its row's title.
    chats_rename: Option<chats_pane::ChatsRename>,
    /// A chat was created, renamed, deleted, re-pointed or answered since the
    /// host last asked ([`ChatSurface::take_chat_list_changed`]).
    chat_list_changed: bool,
    /// The same news, for this surface's own pane.
    chat_list_changed_for_pane: bool,
    /// The *Chats* button was pressed where the host has a navigator.
    wants_chats_view: bool,
    transcript: Option<ChatTranscript>,
    usage_report: Option<UsageReport>,
    live_usage: Option<AiUsage>,
    live_sources: Vec<SourceLink>,
    open_searches: HashSet<String>,
    web_search_armed: bool,
    /// The chip's measured label width from the last draw, so the chip
    /// is as wide as its words (an agent and its model can be long).
    chip_label_w: f32,
    /// The folder control's measured width — its icon, its name and the air
    /// around them — from the last [`ChatSurface::composer_rect`].
    folder_button_w: f32,
    /// A press on the effort slider: the stop under the pointer until
    /// release, when it becomes the chat's setting.
    effort_drag: Option<ReasoningChoice>,
    live_reasoning: String,
    reasoning_expanded: bool,
    attempt: Option<InferenceAttemptId>,
    pending_assistant_message_id: Option<MessageId>,
    pending_user: Option<String>,
    pulse: Option<PulsingDots>,
    draft: String,
    status: Option<String>,
    validation: Option<ProviderValidationResponse>,
    reasoning: ReasoningChoice,
    validated_ready: bool,
    pull_active: bool,
    pull_status: Option<String>,
    setup_open: bool,
    setup_provider: SetupProvider,
    /// The chip composer: a private Koru document under the editor
    /// ([`composer`]), so `@` puts a document in the text.
    composer: ChatComposer,
    composer_hit_rect: Rect,
    /// Titles for the documents chips name, from the chips this window sent,
    /// the chat's pinned documents, and the reader thread's lookups. Read by
    /// the conversation to draw a chip as a titled link.
    document_titles: HashMap<DocumentId, String>,
    /// Documents whose title has been asked of the reader once already.
    titles_requested: HashSet<DocumentId>,
    /// A chip or a titled link was clicked: the document the host should open.
    open_document_request: Option<koru_link::KoruLink>,
    conversation_hit_rect: Rect,
    workspace_search: TextInput,
    /// Asked for a fresh chat before the service answered the boot (a new
    /// tab, the CTO's note): the boot's snapshot carries the service's
    /// current chat, and a tab that wanted a new one must not take it.
    /// Cleared when the chat it asked for arrives.
    fresh_chat_wanted: bool,
    openai_key: TextInput,
    openai_model: TextInput,
    ollama_endpoint: TextInput,
    ollama_model: TextInput,
    system_prompt: TextArea,
    debug_prompt_enabled: bool,
    focus: Focus,
    pointer: (f32, f32),
    message_layouts: Vec<MessageLayout>,
    tool_activity_layouts: Vec<ToolActivityLayout>,
    tool_group_layouts: Vec<ToolGroupLayout>,
    /// The turns whose tool box is folded to its heading.
    collapsed_tool_groups: HashSet<MessageId>,
    tool_group_hover: Option<MessageId>,
    message_width: f32,
    last_rebuild_stats: RebuildStats,
    messages_dirty: bool,
    conversation_scroll: f32,
    conversation_extent: f32,
    conversation_scroll_drag: Option<f32>,
    scroll_to_pending_question: bool,
    scroll_loaded_chat_to_bottom: bool,
    pending_question_anchor: Option<f32>,
    question_navigation: Option<usize>,
    question_navigation_anchor: Option<f32>,
    context_document: Option<(DocumentId, String)>,
    dismissed_context_document: Option<DocumentId>,
    /// When true, the next `set_context_document` call is ignored and the
    /// document pill is cleared instead. Used when the host explicitly starts
    /// a new chat and does not want the active tab's document pre-attached.
    suppress_next_document_context: bool,
    context_workspace: Option<WorkspaceId>,
    pending_quotes: Vec<PendingQuote>,
    staged_chat: Option<ChatId>,
    pill_strip: PillStrip,
    comment_editor: Option<CommentEditor>,
    response_menu_anchor: Option<Rect>,
    credential_request: Option<OpenAiCredentialRequest>,
    pending_openai_settings: Option<(Option<String>, ReasoningChoice)>,
    editing_openai_key: bool,
    model_options_open: bool,
    model_option_offset: usize,
    chat_model_open: bool,
    chat_model_offset: usize,
    chat_model_scroll_drag: Option<f32>,
    tool_access_open: bool,
    tool_mode_open: bool,
    session_auto_approve_chats: HashSet<ChatId>,
    session_approval_sent: HashSet<ToolInvocationId>,
    openai_models: Vec<String>,
    ollama_models: Vec<String>,
    /// Each local agent's readiness, from its validation answer.
    agent_status: HashMap<AiProviderId, AgentStatus>,
    /// When the agents' readiness was last asked for, so a running check is
    /// followed up while the picker or the card shows it.
    agent_status_asked: Option<Instant>,
    /// Each agent's listed models, once its row was expanded.
    agent_models: HashMap<AiProviderId, agents::AgentModels>,
    /// The agent row expanded in the picker, and the group under it.
    expanded_agent: Option<AiProviderId>,
    expanded_group: Option<String>,
    /// The agent picked on the *Local agents* card.
    setup_agent: Option<AiProviderId>,
    /// A folder pick the host owes this surface (the native dialog is the
    /// window's, never the surface's).
    working_folder_request: Option<WorkingFolderRequest>,
    response_hover: Option<(usize, u8)>,
    response_message_hover: Option<usize>,
    tool_activity_hover: Option<(ToolInvocationId, u8)>,
    expanded_tool_activity: Option<ToolInvocationId>,
    tool_workspace_offset: usize,
    copy_feedback: Tooltip,
    web_search_tooltip: Tooltip,
    response_selection: Option<ResponseSelection>,
    response_menu: Option<Menu<ResponseMenuItem>>,
    remote_images: HashMap<String, RemoteImage>,
    pending_model_prefetch: Option<AiProviderId>,
    retry_link: Option<Rect>,
    ollama_search_link: Option<Rect>,
    staged_embedded_send: Option<SendTurnRequest>,
    durable_observations: u8,
    destinations: Vec<DestinationOption>,
    workspace_catalog_error: Option<String>,
    selected_destination: usize,
    destination_open: Option<MessageId>,
    pending_link_chat: Option<ChatId>,
    highlight: Option<(MessageBlockId, Instant)>,
    confirm: Option<Confirm>,
    destructive_action: Option<DestructiveAction>,
    /// **The first Escape of a turn**, waiting [`STOP_ESCAPE_WINDOW`] for
    /// the second that stops it; the hint above the composer shows while it
    /// waits. A field rather than a clock read so a test can age it.
    stop_armed_at: Option<Instant>,
    /// What this surface sent, by chat, so Up can recall a message before
    /// the transcript that carries it has been read back.
    sent_inputs: Vec<(ChatId, String)>,
    /// Where Up and Down stand in [`ChatSurface::input_history`]; `None`
    /// is the message being typed.
    history_cursor: Option<usize>,
    /// The message being typed when Up first left it, whole, for Down to
    /// give back past the newest.
    history_draft: Option<composer::Draft>,
    /// The empty chat's greeting: which of [`GREETINGS`], and the chat it
    /// was picked for, so it holds still while shown and a new chat gets a
    /// new one.
    greeting: Option<(Option<ChatId>, usize)>,
    lang: koru_i18n::Lang,
}

impl ChatSurface {
    /// The composer, handed to a host routing an ephemeral dictation
    /// composition into this surface. It owns its private document, so it is
    /// driven through `koru_ui::DictationTarget` with a `()` context, or as
    /// `koru_ui::dictation::ComposeInto::Held`.
    pub fn dictation_composer(&mut self) -> &mut ChatComposer {
        &mut self.composer
    }

    /// A document chip, or a document link in the conversation, was clicked.
    /// The host opens the document: a tab in Koru, or through the Agent from
    /// the standalone window.
    pub fn take_open_document_request(&mut self) -> Option<koru_link::KoruLink> {
        self.composer
            .take_open_request()
            .or_else(|| self.open_document_request.take())
    }

    /// The host could not open a document this surface asked for. Said on the
    /// status line, in the person's language.
    /// Show the provider setup — what the header's settings button opens —
    /// for a host that arrives here from its own Settings.
    pub fn open_setup(&mut self) {
        self.chat_model_open = false;
        self.tool_access_open = false;
        self.tool_mode_open = false;
        self.history_open = false;
        self.setup_open = true;
        self.focus = Focus::None;
    }

    /// Whether the provider setup is showing.
    pub fn setup_open(&self) -> bool {
        self.setup_open
    }

    pub fn open_document_failed(&mut self) -> bool {
        self.status = Some(koru_i18n::t(self.lang, koru_i18n::Key::AiOpenDocumentFailed).into());
        true
    }

    /// Where the composer is drawn, in the surface's host coordinate space.
    pub fn dictation_anchor(&self) -> Rect {
        self.composer_text_rect()
    }

    pub fn new(mode: SurfaceMode) -> Self {
        Self::new_at(mode, None)
    }

    pub fn new_at(mode: SurfaceMode, root: Option<PathBuf>) -> Self {
        let mut engine = TextEngine::new();
        let composer = ChatComposer::new(root.clone());
        let workspace_search = TextInput::new(&mut engine, role::ui());
        let openai_key = TextInput::new(&mut engine, role::row());
        let openai_model = TextInput::new(&mut engine, role::row());
        let ollama_endpoint =
            TextInput::with_text(&mut engine, role::row(), "http://localhost:11434");
        let ollama_model = TextInput::new(&mut engine, role::row());
        let system_prompt = TextArea::new(&mut engine, role::ui());
        Self {
            mode,
            viewport: Rect::new(0.0, 0.0, 460.0, 680.0),
            engine,
            root,
            reader: None,
            waiting: false,
            settings: None,
            chats: Vec::new(),
            history_open: false,
            chats_pane: koru_chats_ui::ChatsSurface::without_reader(),
            chats_pane_started: false,
            chats_rename: None,
            chat_list_changed: false,
            chat_list_changed_for_pane: false,
            wants_chats_view: false,
            transcript: None,
            usage_report: None,
            live_usage: None,
            live_sources: Vec::new(),
            open_searches: HashSet::new(),
            web_search_armed: false,
            effort_drag: None,
            chip_label_w: 0.0,
            folder_button_w: 0.0,
            live_reasoning: String::new(),
            reasoning_expanded: false,
            attempt: None,
            pending_assistant_message_id: None,
            pending_user: None,
            pulse: None,
            draft: String::new(),
            status: None,
            validation: None,
            reasoning: ReasoningChoice::Default,
            validated_ready: false,
            pull_active: false,
            pull_status: None,
            setup_open: false,
            setup_provider: SetupProvider::OpenAi,
            composer,
            composer_hit_rect: Rect::new(14.0, 626.0, 338.0, 40.0),
            document_titles: HashMap::new(),
            titles_requested: HashSet::new(),
            open_document_request: None,
            conversation_hit_rect: Rect::new(14.0, HEADER_H, 432.0, 500.0),
            workspace_search,
            fresh_chat_wanted: false,
            openai_key,
            openai_model,
            ollama_endpoint,
            ollama_model,
            system_prompt,
            debug_prompt_enabled: koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG),
            focus: Focus::Composer,
            pointer: (0.0, 0.0),
            message_layouts: Vec::new(),
            tool_activity_layouts: Vec::new(),
            tool_group_layouts: Vec::new(),
            collapsed_tool_groups: HashSet::new(),
            tool_group_hover: None,
            message_width: 0.0,
            last_rebuild_stats: RebuildStats::default(),
            messages_dirty: true,
            conversation_scroll: 0.0,
            conversation_extent: 0.0,
            conversation_scroll_drag: None,
            scroll_to_pending_question: false,
            scroll_loaded_chat_to_bottom: false,
            pending_question_anchor: None,
            question_navigation: None,
            question_navigation_anchor: None,
            context_document: None,
            dismissed_context_document: None,
            suppress_next_document_context: false,
            context_workspace: None,
            pending_quotes: Vec::new(),
            staged_chat: None,
            pill_strip: PillStrip::default(),
            comment_editor: None,
            response_menu_anchor: None,
            credential_request: None,
            pending_openai_settings: None,
            editing_openai_key: false,
            model_options_open: false,
            model_option_offset: 0,
            chat_model_open: false,
            chat_model_offset: 0,
            chat_model_scroll_drag: None,
            tool_access_open: false,
            tool_mode_open: false,
            session_auto_approve_chats: HashSet::new(),
            session_approval_sent: HashSet::new(),
            openai_models: Vec::new(),
            ollama_models: Vec::new(),
            agent_status: HashMap::new(),
            agent_status_asked: None,
            agent_models: HashMap::new(),
            expanded_agent: None,
            expanded_group: None,
            setup_agent: None,
            working_folder_request: None,
            response_hover: None,
            response_message_hover: None,
            tool_activity_hover: None,
            expanded_tool_activity: None,
            tool_workspace_offset: 0,
            copy_feedback: Tooltip::default(),
            web_search_tooltip: Tooltip::default(),
            response_selection: None,
            response_menu: None,
            remote_images: HashMap::new(),
            pending_model_prefetch: None,
            retry_link: None,
            ollama_search_link: None,
            staged_embedded_send: None,
            durable_observations: 0,
            destinations: Vec::new(),
            workspace_catalog_error: None,
            selected_destination: 0,
            destination_open: None,
            pending_link_chat: None,
            highlight: None,
            confirm: None,
            destructive_action: None,
            stop_armed_at: None,
            sent_inputs: Vec::new(),
            history_cursor: None,
            history_draft: None,
            greeting: None,
            lang: koru_i18n::Lang::for_system(
                koru_window::system_locale::system_locale().as_deref(),
            ),
        }
    }

    /// Start the background API client on first use. A context surface
    /// constructed behind a closed pane performs no discovery until a host
    /// activates it — opening Add… does, so the Chats flyout can list recents
    /// without the pane being open.
    pub fn activate(&mut self) -> bool {
        if self.reader.is_some() || cfg!(any(test, feature = "no-client")) {
            return false;
        }
        self.reader = Reader::start(self.root.clone());
        if let Some(reader) = &self.reader {
            self.waiting = reader.send(Command::Boot(self.pending_link_chat)).is_ok();
            true
        } else {
            self.status = Some("Koru AI service is not connected.".into());
            true
        }
    }

    /// Start a brand-new chat, suppressing the active document context for this
    /// chat so it begins empty (workspace context still applies).
    pub fn start_new_chat(&mut self) -> bool {
        self.suppress_next_document_context = true;
        self.fresh_chat_wanted = true;
        // Before the boot has answered there is no provider to create under:
        // the intent is kept, and the boot's snapshot creates the chat
        // instead of handing this tab the service's current one.
        if self.settings.is_none() {
            return true;
        }
        self.create_chat()
    }

    /// Open a durable chat link. Calling this before activation makes the
    /// first read target the link directly; calling it later queues a read.
    pub fn open_link(&mut self, chat: ChatId, block: Option<MessageBlockId>) -> bool {
        self.fresh_chat_wanted = false;
        self.pending_link_chat = Some(chat);
        self.highlight = block.map(|block| (block, Instant::now()));
        self.pending_question_anchor = None;
        self.scroll_to_pending_question = false;
        if self.reader.is_some() {
            self.scroll_loaded_chat_to_bottom = true;
            self.send_command(Command::Read(chat))
        } else {
            true
        }
    }

    pub fn set_context_document(&mut self, document: Option<(DocumentId, String)>) -> bool {
        let document = if self.suppress_next_document_context {
            self.suppress_next_document_context = false;
            None
        } else {
            document
        };
        let new_id = document.as_ref().map(|(id, _)| *id);
        let current_id = self.context_document.as_ref().map(|(id, _)| *id);
        if new_id == current_id {
            // Title may have changed; update it without touching dismissal state.
            if document.as_ref().map(|(_, title)| title)
                != self.context_document.as_ref().map(|(_, title)| title)
            {
                self.context_document = document;
                return true;
            }
            return false;
        }
        if new_id.is_some() && new_id == self.dismissed_context_document {
            // The user dismissed this document while it was active; keep it dismissed.
            return false;
        }
        self.dismissed_context_document = None;
        self.context_document = document;
        true
    }

    fn remove_context_document(&mut self) {
        if let Some((id, _)) = self.context_document.take() {
            self.dismissed_context_document = Some(id);
        }
    }

    pub fn clear_context_document_dismissal(&mut self) {
        self.dismissed_context_document = None;
    }

    fn context_document_pill(&self) -> Option<ContextDocumentPill> {
        self.context_document
            .as_ref()
            .map(|(id, title)| ContextDocumentPill {
                document_id: *id,
                title: title.clone(),
            })
    }

    /// Tell an embedded surface which workspace its host is currently showing.
    /// The workspace is presented first and becomes the initial grant for each
    /// chat; users may still remove it afterwards.
    pub fn set_context_workspace(&mut self, workspace: Option<WorkspaceId>) -> bool {
        if self.context_workspace == workspace {
            return false;
        }
        self.context_workspace = workspace;
        self.prioritize_context_workspace();
        self.select_context_workspace_if_needed();
        true
    }

    /// The durable title of the loaded chat, when there is one. Used by hosts
    /// that show the chat as a tab and want the label to follow renames.
    pub fn current_chat_title(&self) -> Option<&str> {
        self.transcript.as_ref().map(|t| t.chat.title.as_str())
    }

    /// The chat this surface is seated on: the loaded transcript, or the
    /// link it is still opening. A host that opens a tab onto a recent chat
    /// uses this to focus that tab before the read has landed, so a second
    /// pick of the same row does not mint another.
    pub fn current_chat_id(&self) -> Option<ChatId> {
        self.transcript
            .as_ref()
            .map(|t| t.chat.id)
            .or(self.pending_link_chat)
    }

    /// Recent chats, most recent first, for a host menu. Empty until the
    /// surface has booted and listed them.
    pub fn recent_chats(&self, limit: usize) -> Vec<(ChatId, String)> {
        self.chats
            .iter()
            .take(limit)
            .map(|chat| (chat.id, chat.title.clone()))
            .collect()
    }

    /// Apply queued service updates without the visible-surface clocks.
    /// A host that activated this surface only to list chats uses this while
    /// the pane is closed, so Boot can land without a draw.
    pub fn drain_updates(&mut self) -> bool {
        let mut changed = false;
        while let Some(update) = self.reader.as_ref().and_then(Reader::try_update) {
            changed |= self.apply_update(update);
        }
        changed
    }

    /// When the next poll for a pending Boot (or other command) is due.
    pub fn listing_deadline(&self) -> Option<Instant> {
        self.waiting.then(|| {
            Instant::now()
                .checked_add(POLL_INTERVAL)
                .unwrap_or_else(Instant::now)
        })
    }

    /// A folder pick the host performs for an agent chat: the request names
    /// the chat and where the picker starts. Cancelling answers nothing.
    pub fn take_working_folder_request(&mut self) -> Option<WorkingFolderRequest> {
        self.working_folder_request.take()
    }

    /// The host's answer to [`take_working_folder_request`](Self::take_working_folder_request).
    pub fn working_folder_chosen(&mut self, chat: ChatId, folder: std::path::PathBuf) -> bool {
        if self.transcript.as_ref().map(|t| t.chat.id) != Some(chat) {
            return false;
        }
        self.send_command(Command::UpdateFolder(
            chat,
            koru_ai_wire::UpdateChatFolderRequest {
                folder: Some(folder.to_string_lossy().into_owned()),
            },
        ))
    }

    pub fn take_openai_credential_request(&mut self) -> Option<OpenAiCredentialRequest> {
        self.credential_request.take()
    }

    /// Complete the host-owned vault mutation. Only after success do we ask
    /// the service to save non-secret model settings and detect vault presence.
    pub fn openai_credential_saved(&mut self, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => {
                let Some((model, reasoning)) = self.pending_openai_settings.take() else {
                    return false;
                };
                self.send_command(Command::UpdateOpenAi(UpdateOpenAiSettingsRequest {
                    model,
                    reasoning,
                }))
            }
            Err(message) => {
                self.pending_openai_settings = None;
                self.status = Some(message);
                true
            }
        }
    }

    pub fn set_viewport(&mut self, area: Rect) -> bool {
        if self.viewport == area {
            return false;
        }
        if (self.viewport.w - area.w).abs() > f32::EPSILON {
            self.messages_dirty = true;
        }
        self.viewport = area;
        true
    }

    pub fn invalidate(&mut self) {
        let workspace_search = self.workspace_search.value();
        let openai_key = self.openai_key.value();
        let openai_model = self.openai_model.value();
        let ollama_endpoint = self.ollama_endpoint.value();
        let ollama_model = self.ollama_model.value();
        let system_prompt = self.system_prompt.value();
        self.engine = TextEngine::new();
        self.composer.invalidate();
        self.workspace_search =
            TextInput::with_text(&mut self.engine, role::ui(), &workspace_search);
        self.openai_key = TextInput::with_text(&mut self.engine, role::row(), &openai_key);
        self.openai_model = TextInput::with_text(&mut self.engine, role::row(), &openai_model);
        self.ollama_endpoint =
            TextInput::with_text(&mut self.engine, role::row(), &ollama_endpoint);
        self.ollama_model = TextInput::with_text(&mut self.engine, role::row(), &ollama_model);
        self.system_prompt = TextArea::new(&mut self.engine, role::ui());
        self.system_prompt
            .set_text(&mut self.engine, &system_prompt);
        if let Some(editor) = &mut self.comment_editor {
            editor.rebind(&mut self.engine);
        }
        self.messages_dirty = true;
    }

    pub fn maintenance_deadline(&self) -> Option<Instant> {
        let poll = self.waiting.then(|| {
            Instant::now()
                .checked_add(POLL_INTERVAL)
                .unwrap_or_else(Instant::now)
        });
        let highlight = self
            .highlight
            .and_then(|(_, since)| since.checked_add(HIGHLIGHT_FOR));
        let image = self
            .remote_images
            .values()
            .any(|image| image.pending.is_some())
            .then(|| Instant::now() + Duration::from_millis(100));
        let chats_pane = self.chats_pane_awaiting().then(|| {
            Instant::now()
                .checked_add(POLL_INTERVAL)
                .unwrap_or_else(Instant::now)
        });
        // The stop hint goes away when its window closes.
        let stop_hint = self
            .stop_armed_at
            .and_then(|at| at.checked_add(STOP_ESCAPE_WINDOW));
        [
            poll,
            highlight,
            image,
            chats_pane,
            stop_hint,
            self.copy_feedback.maintenance_deadline(),
            self.web_search_tooltip.maintenance_deadline(),
            self.pill_strip.maintenance_deadline(),
            self.composer.maintenance_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn maintain(&mut self, _now: Instant) -> bool {
        // A staged embedded send needs another host frame so the Store has an
        // opportunity to publish and acknowledge the edit that preceded it.
        let mut changed = self.staged_embedded_send.is_some();
        changed |= self.drain_updates();
        changed |= self.maintain_chats_pane();
        self.follow_agent_checks(_now);
        if self
            .highlight
            .is_some_and(|(_, since)| _now.duration_since(since) >= HIGHLIGHT_FOR)
        {
            self.highlight = None;
            changed = true;
        }
        if self.stop_armed_at.is_some() && !self.stop_armed(_now) {
            self.stop_armed_at = None;
            changed = true;
        }
        changed |= self.maintain_remote_images();
        changed |= self.copy_feedback.maintain(_now);
        changed |= self.web_search_tooltip.maintain(_now);
        // The `@` picker's search answers, delivered on this tick.
        self.composer
            .set_context(self.context_workspace, self.context_document.clone());
        changed |= self.composer.maintain();
        changed | self.pill_strip.maintain(_now)
    }

    /// Let an embedding host release a staged turn only after Store reports no
    /// unacknowledged work. Two observations are required so a local edit that
    /// was still unsent during the press cannot look durable merely because it
    /// had not entered the Store session yet.
    pub fn submit_embedded_when_durable(&mut self, durable: bool) -> bool {
        if self.staged_embedded_send.is_none() {
            return false;
        }
        if !durable {
            self.durable_observations = 0;
            return false;
        }
        self.durable_observations = self.durable_observations.saturating_add(1);
        if self.durable_observations < 2 {
            return false;
        }
        self.durable_observations = 0;
        let request = self
            .staged_embedded_send
            .take()
            .expect("a staged send was checked above");
        self.send_command(Command::Send(request))
    }

    pub fn animating(&self) -> bool {
        self.copy_feedback.animating(Instant::now())
            || self.web_search_tooltip.animating(Instant::now())
            || self.pill_strip.animating(Instant::now())
            || self.pulse.as_ref().is_some_and(PulsingDots::animating)
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        if self.confirm.is_some() || self.response_menu.is_some() {
            return CursorIcon::Default;
        }
        let p = self.pointer;
        if self
            .composer
            .picker_rect()
            .is_some_and(|panel| panel.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        if let Some(editor) = &self.comment_editor {
            let bounds = self.conversation_hit_rect;
            if editor.panel_rect(bounds).contains(p.0, p.1) {
                return CursorIcon::Text;
            }
        }
        if self.conversation_scroll_drag.is_some()
            || self
                .conversation_scrollbar()
                .is_some_and(|scrollbar| scrollbar.in_gutter(p.0, p.1))
        {
            return CursorIcon::NsResize;
        }
        if self.chat_model_scroll_drag.is_some()
            || self
                .chat_model_scrollbar()
                .is_some_and(|scrollbar| scrollbar.in_gutter(p.0, p.1))
        {
            return CursorIcon::NsResize;
        }
        if self.retry_link.is_some_and(|rect| rect.contains(p.0, p.1))
            || self
                .ollama_search_link
                .is_some_and(|rect| rect.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        if self.control_hover_at(p.0, p.1).is_some() || self.response_hover.is_some() {
            return CursorIcon::Default;
        }
        if self.tool_activity_at(p.0, p.1).is_some() {
            return CursorIcon::Default;
        }
        if let Some(hit) = self.response_text_hit(p.0, p.1) {
            return if hit.link.is_some() {
                CursorIcon::Default
            } else {
                CursorIcon::Text
            };
        }
        if self
            .tool_access_search_rect()
            .is_some_and(|rect| rect.contains(p.0, p.1))
            || (!self.showing_setup() && self.composer_text_rect().contains(p.0, p.1))
            || (self.showing_setup() && self.setup_text_field_at(p.0, p.1))
            || self
                .system_prompt_rect()
                .is_some_and(|r| r.contains(p.0, p.1))
        {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        if let Some(confirm) = &mut self.confirm {
            let outcome = confirm.key(key);
            return self.finish_confirmation(outcome);
        }
        if let Some(handled) = self.chats_pane_key(key, text, mods) {
            return handled;
        }
        if let Some(handled) = self.quote_key(key, text, mods) {
            return handled;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) && self.chat_model_open {
            self.chat_model_open = false;
            self.chat_model_scroll_drag = None;
            return true;
        }
        if self.tool_access_open && self.focus == Focus::WorkspaceSearch {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.tool_access_open = false;
                self.workspace_search.set_text(&mut self.engine, "");
                self.focus = Focus::Composer;
                return true;
            }
            let before = self.workspace_search.raw_text().to_owned();
            let handled =
                self.workspace_search.key(&mut self.engine, key, text, mods) == Typed::Handled;
            if self.workspace_search.raw_text() != before {
                self.tool_workspace_offset = 0;
            }
            return handled;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) && self.setup_open {
            if self.transcript.is_some() {
                self.setup_open = false;
                self.focus = Focus::Composer;
                return true;
            }
        }
        if self.showing_setup() {
            return self.setup_key(key, text, mods);
        }
        if koru_keys::is_chord(mods)
            && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case("c"))
        {
            return self.copy_response_selection();
        }
        if mods.shift_key()
            && !koru_keys::has_primary(mods)
            && !mods.alt_key()
            && matches!(
                key,
                Key::Named(
                    NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                )
            )
            && self.move_response_selection(key)
        {
            return true;
        }
        if Self::is_question_navigation_chord(key, mods) {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] AI chat claimed=question-navigation direction={}",
                if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                    "down"
                } else {
                    "up"
                }
            ));
            return self.navigate_questions(matches!(key, Key::Named(NamedKey::ArrowDown)));
        }
        if self.focus != Focus::Composer {
            self.focus = Focus::Composer;
        }
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        self.composer
            .set_context(self.context_workspace, self.context_document.clone());
        // **The `@` picker's keys come first.** While it is open, Up, Down,
        // Enter, Tab and Escape choose or dismiss a document; Enter sends only
        // once it has closed.
        if self.composer.picker_open() {
            return self.composer.key(key, text, mods);
        }
        if matches!(key, Key::Named(NamedKey::Escape))
            && let Some(attempt) = self.attempt
        {
            return self.escape_during_turn(attempt, Instant::now());
        }
        // **Up and Down walk the messages this chat was sent**, from the
        // composer's first and last lines; inside a longer message they move
        // the caret as they always did. Alt and Shift with them were claimed
        // above, by question navigation and the response selection.
        if mods.is_empty()
            && matches!(key, Key::Named(NamedKey::ArrowUp))
            && self.composer.caret_on_first_line()
            && self.recall_input(true)
        {
            return true;
        }
        if mods.is_empty()
            && matches!(key, Key::Named(NamedKey::ArrowDown))
            && self.composer.caret_on_last_line()
            && self.recall_input(false)
        {
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Enter)) {
            // The editor's key map reads Shift+Enter as *split above* and
            // Ctrl+Enter as *toggle done*; in a message both are claimed here.
            if mods.shift_key() && !koru_keys::has_primary(mods) && !mods.alt_key() {
                return self.composer.insert_break();
            }
            if !koru_keys::is_chord(mods) && !mods.alt_key() {
                return self.send_composer();
            }
        }
        if mods.control_key()
            && !mods.shift_key()
            && !mods.alt_key()
            && koru_keys::letter(key, text) == Some('j')
        {
            return self.composer.insert_break();
        }
        self.composer.key(key, text, mods)
    }

    /// This chord belongs to a visible chat surface even when another part of
    /// the containing workspace most recently held keyboard focus.
    pub fn is_question_navigation_chord(key: &Key, mods: ModifiersState) -> bool {
        mods.alt_key()
            && !mods.control_key()
            && !mods.super_key()
            && !mods.shift_key()
            && matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown))
    }

    /// **Escape twice stops the work in progress.** The first arms the stop
    /// and says so above the composer; a second inside
    /// [`STOP_ESCAPE_WINDOW`] cancels the attempt exactly as the stop button
    /// does. One press alone never stops a turn — Escape is also how a
    /// person leaves whatever they were doing. An open workspace or mode
    /// popover takes the press first, so Escape closes it, not the turn.
    fn escape_during_turn(&mut self, attempt: InferenceAttemptId, now: Instant) -> bool {
        if self.tool_mode_open || self.tool_access_open || self.destination_open.is_some() {
            self.tool_mode_open = false;
            self.tool_access_open = false;
            self.destination_open = None;
            return true;
        }
        if self.stop_armed(now) {
            self.stop_armed_at = None;
            return self.send_command(Command::Cancel(attempt));
        }
        self.stop_armed_at = Some(now);
        true
    }

    /// Whether a first Escape is still waiting for its second at `now`.
    fn stop_armed(&self, now: Instant) -> bool {
        self.attempt.is_some()
            && self
                .stop_armed_at
                .is_some_and(|at| now.saturating_duration_since(at) < STOP_ESCAPE_WINDOW)
    }

    /// The line above the composer: the stop hint while it waits, else the
    /// status.
    fn footer_line(&self) -> Option<&str> {
        if self.stop_armed(Instant::now()) {
            return Some(koru_i18n::t(
                self.lang,
                koru_i18n::Key::AiPressEscAgainToStop,
            ));
        }
        self.status.as_deref()
    }

    /// What Up walks back through, oldest first: this chat's messages from
    /// the person, then what this surface sent that the transcript does not
    /// carry yet. The same message twice in a row is one step.
    fn input_history(&self) -> Vec<String> {
        let Some(transcript) = &self.transcript else {
            return Vec::new();
        };
        let mut asked: Vec<(u32, &str)> = transcript
            .messages
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .map(|message| (message.ordinal, message.markdown.as_str()))
            .collect();
        asked.sort_by_key(|(ordinal, _)| *ordinal);
        let mut entries: Vec<String> = asked
            .iter()
            .map(|(_, markdown)| (*markdown).to_owned())
            .collect();
        for (chat, sent) in &self.sent_inputs {
            if *chat == transcript.chat.id && !asked.iter().any(|(_, known)| known == sent) {
                entries.push(sent.clone());
            }
        }
        entries.retain(|entry| !entry.trim().is_empty());
        entries.dedup();
        entries
    }

    /// Step through [`ChatSurface::input_history`]: `older` is Up. Leaving the
    /// message being typed sets it aside whole, chips and all, and Down past
    /// the newest gives it back. `false` when there is nowhere to go, so the
    /// key falls through to the editor.
    fn recall_input(&mut self, older: bool) -> bool {
        let entries = self.input_history();
        let next = match (self.history_cursor, older) {
            (None, false) => return false,
            (None, true) if entries.is_empty() => return false,
            (None, true) => Some(entries.len() - 1),
            // At the oldest already: the press is spent, the text stays.
            (Some(0), true) if !entries.is_empty() => return true,
            (Some(index), true) => Some(index.saturating_sub(1)),
            (Some(index), false) => Some(index + 1),
        }
        .filter(|index| *index < entries.len());
        match next {
            Some(index) => {
                if self.history_cursor.is_none() {
                    self.history_draft = Some(self.composer.stash());
                }
                self.history_cursor = Some(index);
                self.composer.set_text(&entries[index]);
            }
            None => {
                self.history_cursor = None;
                match self.history_draft.take() {
                    Some(draft) => self.composer.unstash(draft),
                    None => self.composer.clear(),
                }
            }
        }
        true
    }

    /// Forget where Up and Down stood — after a send, or on another chat.
    fn forget_input_walk(&mut self) {
        self.history_cursor = None;
        self.history_draft = None;
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(handled) = self.quote_overlay_mouse_down(x, y) {
            return handled;
        }
        if let Some(confirm) = &mut self.confirm {
            let outcome = confirm.mouse_down(
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
                x - self.viewport.x,
                y - self.viewport.y,
            );
            return self.finish_confirmation(outcome);
        }
        if let Some(handled) = self.chats_pane_mouse_down(x, y) {
            return handled;
        }
        if self.showing_setup() {
            return self.setup_mouse_down(x, y);
        }
        // The composer and every control on it are hit where they are now:
        // centred under an empty chat's greeting, or at the bottom.
        self.composer_rect();
        // The `@` picker floats over the conversation, so a press on it is the
        // composer's before anything underneath can claim it.
        if self
            .composer
            .picker_rect()
            .is_some_and(|panel| panel.contains(x, y))
        {
            self.focus = Focus::Composer;
            return self.composer.mouse_down(x, y);
        }
        // A press anywhere but the composer answers an open picker the way
        // Escape does, and then does whatever it does.
        if !self.composer.contains(x, y) {
            self.composer.close_picker();
        }
        if self.pill_mouse_down(x, y) {
            return true;
        }
        if let Some(scrollbar) = self.conversation_scrollbar()
            && scrollbar.in_gutter(x, y)
        {
            if scrollbar.on_thumb(x, y) {
                // Preserve the exact grab point so the thumb does not jump
                // underneath the pointer when the drag begins.
                self.conversation_scroll_drag = Some(y - scrollbar.thumb_top);
            } else {
                // The invisible track is still an interaction target: page
                // toward the press, matching the document scrollbar.
                self.conversation_scroll += if y < scrollbar.thumb_top {
                    -self.conversation_hit_rect.h
                } else {
                    self.conversation_hit_rect.h
                };
                self.conversation_scroll =
                    self.conversation_scroll.clamp(0.0, scrollbar.max_scroll);
            }
            return true;
        }
        if self.destination_open.is_some() {
            if let Some(index) = self.destination_at(x, y) {
                self.selected_destination = index;
                let message = self.destination_open.expect("picker is open");
                return self.start_export(message);
            }
            self.destination_open = None;
            return true;
        }
        if self
            .privacy_button()
            .is_some_and(|button| button.contains(x, y))
        {
            self.history_open = false;
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            return self.toggle_chat_privacy();
        }
        if self.tool_mode_open {
            if let Some(modes) = self.tool_mode_buttons()
                && let Some(policy) = self
                    .transcript
                    .as_ref()
                    .map(|transcript| transcript.tool_policy.clone())
            {
                if modes[0].contains(x, y) {
                    return self.update_tool_policy(ToolMode::Manual, policy.workspace_ids);
                }
                if modes[1].contains(x, y) {
                    return self.update_tool_policy(ToolMode::Auto, policy.workspace_ids);
                }
            }
            if self
                .tool_mode_session_button()
                .is_some_and(|rect| rect.contains(x, y))
            {
                return self.toggle_session_auto_approve();
            }
        }
        if self.tool_access_open {
            if self
                .tool_access_search_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                let rect = self
                    .tool_access_search_rect()
                    .expect("an open searchable workspace panel has a field");
                self.workspace_search.click(&mut self.engine, rect, x, y);
                self.focus = Focus::WorkspaceSearch;
                return true;
            }
            if let Some(workspace) = self.tool_access_workspace_at(x, y) {
                return self.toggle_tool_workspace(workspace);
            }
        }
        if self.tool_access_button().contains(x, y) {
            self.history_open = false;
            self.chat_model_open = false;
            self.tool_mode_open = false;
            self.tool_workspace_offset = 0;
            self.tool_access_open = !self.tool_access_open;
            self.workspace_search.set_text(&mut self.engine, "");
            self.focus = if self.tool_access_open && self.destinations.len() > 5 {
                Focus::WorkspaceSearch
            } else {
                Focus::Composer
            };
            return true;
        }
        if self.folder_editable()
            && self
                .folder_button()
                .is_some_and(|button| button.contains(x, y))
        {
            self.history_open = false;
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            if let Some(transcript) = &self.transcript {
                self.working_folder_request = Some(WorkingFolderRequest {
                    chat: transcript.chat.id,
                    start: transcript
                        .chat
                        .agent_working_folder
                        .as_deref()
                        .map(std::path::PathBuf::from),
                });
            }
            return true;
        }
        if self.tool_mode_button().contains(x, y) {
            self.history_open = false;
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = !self.tool_mode_open;
            return true;
        }
        if self
            .reasoning_toggle_rect()
            .is_some_and(|rect| rect.contains(x, y))
        {
            self.reasoning_expanded = !self.reasoning_expanded;
            return true;
        }
        if self
            .web_search_button()
            .is_some_and(|button| button.contains(x, y))
        {
            self.web_search_armed = !self.web_search_armed;
            self.history_open = false;
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            self.web_search_tooltip.clear();
            return true;
        }
        if self.chat_model_open {
            if let Some(scrollbar) = self.chat_model_scrollbar()
                && scrollbar.in_gutter(x, y)
            {
                let grab = if scrollbar.on_thumb(x, y) {
                    y - scrollbar.thumb_top
                } else {
                    scrollbar.thumb_h * 0.5
                };
                self.chat_model_scroll_drag = Some(grab);
                let scroll = scrollbar.scroll_for_pointer(y, grab);
                self.chat_model_offset = (scroll / 30.0).round() as usize;
                return true;
            }
            if let (Some(section), Some(control)) =
                (self.effort_section(), self.effort_control().cloned())
                && let Some(choice) = effort::stop_at(&effort::slider(section, &control), x, y)
            {
                self.effort_drag = Some(choice);
                return true;
            }
            if let Some(index) = self.chat_model_option_at(x, y) {
                return self.click_picker_row(index, x);
            }
            if !self.chat_model_button().contains(x, y) {
                self.chat_model_open = false;
                self.chat_model_scroll_drag = None;
                return true;
            }
        }
        if self.chat_model_button().contains(x, y) {
            self.history_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            self.chat_model_offset = 0;
            self.chat_model_scroll_drag = None;
            self.chat_model_open = !self.chat_model_open;
            if self.chat_model_open {
                self.refresh_agent_status();
            }
            if self.chat_model_open
                && self.chat_model_rows().is_empty()
                && self.effort_control().is_none()
            {
                if let Some(provider) = self
                    .transcript
                    .as_ref()
                    .map(|transcript| transcript.chat.current_model.provider.clone())
                {
                    return self.send_command(Command::Validate(provider));
                }
            }
            return true;
        }
        if self.hosts_header_buttons() && self.history_button().contains(x, y) {
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            return self.press_chats_button();
        }
        if self.hosts_header_buttons() && self.settings_button().contains(x, y) {
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            self.setup_open = true;
            self.history_open = false;
            self.focus = Focus::None;
            return true;
        }
        if self.hosts_header_buttons() && self.new_chat_button().contains(x, y) {
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            return self.create_chat();
        }
        if self.tool_mode_open || self.tool_access_open {
            self.tool_mode_open = false;
            self.tool_access_open = false;
            return true;
        }
        if self.attempt.is_some() && self.send_button().contains(x, y) {
            if let Some(attempt) = self.attempt {
                return self.send_command(Command::Cancel(attempt));
            }
        } else if self.send_button().contains(x, y) {
            self.send_composer();
            return true;
        }
        if let Some(message) = self.tool_group_at(x, y) {
            if !self.collapsed_tool_groups.remove(&message) {
                self.collapsed_tool_groups.insert(message);
            }
            self.messages_dirty = true;
            return true;
        }
        if let Some((tool, part)) = self.tool_activity_at(x, y) {
            match part {
                part if part >= agents::CHOICE_PART_BASE => {
                    return self.decide_agent_choice(tool, part);
                }
                1 => return self.send_command(Command::ApproveTool(tool)),
                2 => return self.auto_approve_tool_for_session(tool),
                3 => return self.send_command(Command::DenyTool(tool)),
                _ => {
                    self.expanded_tool_activity = if self.expanded_tool_activity == Some(tool) {
                        None
                    } else {
                        Some(tool)
                    };
                    let content_extent = self.position_conversation_items();
                    let viewport_height = self.conversation_rect().h;
                    self.conversation_extent = self
                        .pending_question_anchor
                        .into_iter()
                        .chain(self.question_navigation_anchor)
                        .fold(content_extent, |extent, top| {
                            extent.max(top + viewport_height)
                        });
                    return true;
                }
            }
        }
        if let Some(action) = self.response_action_at(x, y) {
            return self.perform_response_action(action);
        }
        if let Some(url) = self.source_link_at(x, y) {
            let _ = open_web_link(&url);
            return true;
        }
        if let Some(url) = self.user_link_at(x, y) {
            return self.follow_link(&url);
        }
        if let Some(hit) = self.response_text_hit(x, y) {
            self.focus = Focus::None;
            let position = hit.position;
            self.response_selection = Some(ResponseSelection {
                anchor: position,
                focus: position,
                dragging: true,
                pressed_link: hit.link,
            });
            return true;
        }
        self.response_selection = None;
        if self.composer.contains(x, y) {
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
            return self.composer.mouse_down(x, y);
        }
        false
    }

    /// Follow a link the person clicked in the conversation: a Koru document
    /// goes to the host to open, a web address to the browser.
    fn follow_link(&mut self, url: &str) -> bool {
        match koru_link::parse(url) {
            Ok(link @ koru_link::KoruLink::Document { .. }) => {
                self.open_document_request = Some(link);
                true
            }
            _ => {
                let _ = open_web_link(url);
                true
            }
        }
    }

    /// The document link under a point in one of the person's own messages.
    fn user_link_at(&self, x: f32, y: f32) -> Option<String> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for message in &self.message_layouts {
            if message.role != ChatRole::User || message.links.is_empty() {
                continue;
            }
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            let origin = (card.x + PAD, card.y + PAD);
            let Some((cluster, _)) =
                Cluster::from_point_exact(&message.layout, x - origin.0, y - origin.1)
            else {
                continue;
            };
            let index = cluster.text_range().start;
            if let Some(link) = message
                .links
                .iter()
                .find(|link| link.range.contains(&index))
            {
                return Some(link.url.clone());
            }
        }
        None
    }

    /// Close every transient picker, menu, or confirmation, returning whether
    /// anything changed. The host calls this when a press lands outside the AI
    /// surface so an open popover does not keep floating over the document.
    pub fn dismiss_popovers(&mut self) -> bool {
        let mut changed = self.close_chats_pane();
        if self.chat_model_open {
            self.chat_model_open = false;
            self.chat_model_scroll_drag = None;
            changed = true;
        }
        if self.tool_access_open {
            self.tool_access_open = false;
            changed = true;
        }
        if self.tool_mode_open {
            self.tool_mode_open = false;
            changed = true;
        }
        if self.model_options_open {
            self.model_options_open = false;
            changed = true;
        }
        if self.destination_open.take().is_some() {
            changed = true;
        }
        if self.response_menu.take().is_some() {
            self.response_menu_anchor = None;
            changed = true;
        }
        if self.confirm.take().is_some() {
            changed = true;
        }
        changed | self.composer.close_picker()
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        // Over the open Chats pane the pane is all there is: nothing under it
        // hovers.
        if self.history_open && self.chats_panel().contains(x, y) {
            self.pointer = (x, y);
            return self.chats_pane_mouse_move(x, y);
        }
        let pane_left = self.chats_pane_mouse_move(x, y);
        let old = self.cursor_icon();
        let old_control_hover = self.control_hover_at(self.pointer.0, self.pointer.1);
        self.pointer = (x, y);
        let search_tooltip_changed = self.web_search_tooltip.set_hover(
            self.web_search_button()
                .filter(|button| button.contains(x, y))
                .map(|button| {
                    TooltipContent::new(
                        21,
                        button,
                        "Search the web for the next replies in this chat",
                    )
                }),
            Instant::now(),
        );
        if let Some(changed) = self.response_menu_mouse_move(x, y, old) {
            return changed;
        }
        if let Some(grab) = self.conversation_scroll_drag {
            if let Some(scrollbar) = self.conversation_scrollbar() {
                let next = scrollbar.scroll_for_pointer(y, grab);
                let changed = (next - self.conversation_scroll).abs() > f32::EPSILON;
                self.conversation_scroll = next;
                return changed || old != self.cursor_icon();
            }
            return true;
        }
        if self.effort_drag.is_some()
            && let (Some(section), Some(control)) =
                (self.effort_section(), self.effort_control().cloned())
        {
            let next = effort::nearest(&effort::slider(section, &control), x);
            let changed = next != self.effort_drag;
            self.effort_drag = next;
            return changed;
        }
        if let Some(grab) = self.chat_model_scroll_drag {
            if let Some(scrollbar) = self.chat_model_scrollbar() {
                let scroll = scrollbar.scroll_for_pointer(y, grab);
                let next = (scroll / 30.0).round() as usize;
                let changed = next != self.chat_model_offset;
                self.chat_model_offset = next;
                return changed || old != self.cursor_icon();
            }
            return true;
        }
        if let Some(confirm) = &mut self.confirm {
            let changed = confirm.mouse_move(
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
                x - self.viewport.x,
                y - self.viewport.y,
            );
            return changed || old != self.cursor_icon();
        }
        let (claimed, pill_changed) = self.quote_content_mouse_move(x, y, old);
        if let Some(changed) = claimed {
            return changed;
        }
        let old_response_hover = self.response_hover;
        let old_message_hover = self.response_message_hover;
        let old_tool_hover = self.tool_activity_hover;
        let old_group_hover = self.tool_group_hover;
        self.response_message_hover = if self.showing_setup() {
            None
        } else {
            self.response_message_hover_at(x, y, old_message_hover)
        };
        self.response_hover = if self.showing_setup() {
            None
        } else {
            self.response_action_hover_at(x, y)
        };
        self.tool_activity_hover = if self.showing_setup() {
            None
        } else {
            self.tool_activity_at(x, y)
        };
        self.tool_group_hover = if self.showing_setup() {
            None
        } else {
            self.tool_group_at(x, y)
        };
        let dragged = if self.focus == Focus::WorkspaceSearch {
            self.tool_access_search_rect()
                .is_some_and(|rect| self.workspace_search.drag(&mut self.engine, rect, x))
        } else if self.showing_setup() {
            self.drag_setup_input(x)
        } else {
            self.composer_rect();
            self.composer.mouse_move(x, y)
        };
        let response_dragged = self.drag_response_selection(x, y);
        dragged
            || pill_changed
            || response_dragged
            || old_response_hover != self.response_hover
            || old_message_hover != self.response_message_hover
            || old_tool_hover != self.tool_activity_hover
            || old_group_hover != self.tool_group_hover
            || old_control_hover != self.control_hover_at(x, y)
            || search_tooltip_changed
            || pane_left
            || old != self.cursor_icon()
    }

    pub fn mouse_up(&mut self) -> bool {
        self.conversation_scroll_drag = None;
        self.chat_model_scroll_drag = None;
        if let Some(choice) = self.effort_drag.take() {
            self.choose_effort(choice);
            return true;
        }
        self.composer.mouse_up();
        self.workspace_search.release();
        self.openai_key.release();
        self.openai_model.release();
        self.ollama_endpoint.release();
        self.ollama_model.release();
        self.system_prompt.release();
        if let Some(editor) = &mut self.comment_editor {
            editor.release();
        }
        let pressed = self.response_selection.as_mut().and_then(|selection| {
            selection.dragging = false;
            (selection.anchor == selection.focus)
                .then(|| selection.pressed_link.take())
                .flatten()
        });
        if let Some(url) = pressed {
            self.follow_link(&url);
        }
        true
    }

    pub fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        if !self.showing_setup() && self.composer.contains(x, y) {
            return self.composer.mouse_double_click(x, y);
        }
        if let Some(mut hit) = self.response_text_hit(x, y) {
            if let Some((layout, origin)) =
                self.response_layout(hit.position.message, hit.position.block)
            {
                hit.selection = Selection::word_from_point(layout, x - origin.0, y - origin.1);
            }
            let anchor = ResponsePosition {
                byte: hit.selection.anchor().index(),
                affinity: hit.selection.anchor().affinity(),
                ..hit.position
            };
            let focus = ResponsePosition {
                byte: hit.selection.focus().index(),
                affinity: hit.selection.focus().affinity(),
                ..hit.position
            };
            self.response_selection = Some(ResponseSelection {
                anchor,
                focus,
                dragging: false,
                pressed_link: None,
            });
            return true;
        }
        false
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) -> bool {
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        // Over the composer's open picker, or over its own lines once they
        // overflow, the wheel is the composer's.
        let (px, py) = self.pointer;
        if !self.showing_setup() && self.composer.scroll_by(px, py, delta) {
            return true;
        }
        if self.showing_setup() {
            if !self.model_options_open {
                return false;
            }
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 30.0).clamp(-4.0, 4.0),
            };
            let previous = self.model_option_offset;
            let max = self
                .model_options()
                .len()
                .saturating_sub(MODEL_VISIBLE_ROWS);
            if dy > 0.0 {
                self.model_option_offset =
                    self.model_option_offset.saturating_sub(dy.ceil() as usize);
            } else if dy < 0.0 {
                self.model_option_offset =
                    (self.model_option_offset + (-dy).ceil() as usize).min(max);
            }
            return previous != self.model_option_offset;
        }
        if self.tool_access_open {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 30.0).clamp(-4.0, 4.0),
            };
            let previous = self.tool_workspace_offset;
            let max = self
                .matching_workspace_indices()
                .len()
                .saturating_sub(self.tool_access_visible_rows());
            if dy > 0.0 {
                self.tool_workspace_offset = self.tool_workspace_offset.saturating_sub(1);
            } else if dy < 0.0 {
                self.tool_workspace_offset = (self.tool_workspace_offset + 1).min(max);
            }
            return previous != self.tool_workspace_offset;
        }
        if self.chat_model_open {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 30.0).clamp(-4.0, 4.0),
            };
            let previous = self.chat_model_offset;
            let max = self
                .chat_model_options()
                .len()
                .saturating_sub(MODEL_VISIBLE_ROWS);
            if dy > 0.0 {
                self.chat_model_offset = self.chat_model_offset.saturating_sub(dy.ceil() as usize);
            } else if dy < 0.0 {
                self.chat_model_offset = (self.chat_model_offset + (-dy).ceil() as usize).min(max);
            }
            return previous != self.chat_model_offset;
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 38.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        } * CONVERSATION_SCROLL_SPEED;
        if let Some(changed) = self.chats_pane_scroll(dy) {
            return changed;
        }
        let max = (self.conversation_extent - self.conversation_rect().h).max(0.0);
        let next = (self.conversation_scroll - dy).clamp(0.0, max);
        let changed = (next - self.conversation_scroll).abs() > f32::EPSILON;
        self.conversation_scroll = next;
        changed
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            p.bg,
            &scene::rect(
                self.viewport.x,
                self.viewport.y,
                self.viewport.w,
                self.viewport.h,
            ),
        );
        if self.showing_setup() {
            self.draw_setup(canvas, root);
        } else {
            self.draw_chat(canvas, root);
            let bounds = self.conversation_hit_rect;
            if let Some(editor) = &mut self.comment_editor {
                editor.draw(canvas, root, &mut self.engine, bounds);
            }
        }
        if let Some(confirm) = &self.confirm {
            confirm.draw(
                canvas,
                root * Affine::translate((self.viewport.x as f64, self.viewport.y as f64)),
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
            );
        }
        let menu_viewport = self.menu_viewport();
        if let Some(menu) = &self.response_menu {
            menu.draw(canvas, root, &mut self.engine, menu_viewport, &[true, true]);
        }
    }

    fn showing_setup(&self) -> bool {
        self.setup_open || !self.validated_ready
    }

    fn send_command(&mut self, command: Command) -> bool {
        let Some(reader) = &self.reader else {
            self.status = Some("Koru AI service is not connected.".into());
            return true;
        };
        match reader.send(command) {
            Ok(()) => {
                self.waiting = true;
                self.status = None;
            }
            Err(SendError::Full) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI dropped a command because the client queue is full"
                );
                self.status = Some("Koru AI is busy. Try again in a moment.".into());
            }
            Err(SendError::Disconnected) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI dropped a command because the client thread is disconnected"
                );
                self.status = Some("Koru AI service is not connected.".into());
            }
        }
        true
    }

    fn validate_models_after_settings(&mut self, selected: Option<AiProviderId>) {
        let openai = || AiProviderId::parse(AiProviderId::OPENAI).expect("built-in provider id");
        match selected {
            Some(provider) => {
                if provider.as_str() != AiProviderId::OPENAI && self.openai_credential_configured()
                {
                    self.pending_model_prefetch = Some(openai());
                }
                self.send_command(Command::Validate(provider));
            }
            None if self.openai_credential_configured() => {
                self.send_command(Command::Validate(openai()));
            }
            None => {}
        }
    }

    fn send_model_prefetch(&mut self, provider: AiProviderId) {
        let Some(reader) = &self.reader else {
            return;
        };
        if reader.send(Command::Validate(provider)).is_ok() {
            self.waiting = true;
        }
    }

    fn open_confirmation(&mut self, action: DestructiveAction) {
        let (title, body, label) = match &action {
            DestructiveAction::DeleteChats(chats) if chats.len() <= 1 => (
                "Delete this chat?",
                "Its messages and search results will be removed. Anonymous usage and feedback totals remain.",
                "Delete chat",
            ),
            DestructiveAction::DeleteChats(_) => (
                "Delete these chats?",
                "Their messages and search results will be removed. Anonymous usage and feedback totals remain.",
                "Delete chats",
            ),
            DestructiveAction::ClearHistory => (
                "Clear all chat history?",
                "All AI conversations and search results will be removed. Anonymous usage and feedback totals remain.",
                "Clear history",
            ),
            DestructiveAction::ClearUsage => (
                "Clear usage data?",
                "All private token, timing, outcome, and feedback aggregates will be removed. Chat history is unchanged.",
                "Clear usage",
            ),
        };
        self.confirm = Some(Confirm::new(title, body, label));
        self.destructive_action = Some(action);
    }

    fn finish_confirmation(&mut self, outcome: ConfirmOutcome) -> bool {
        match outcome {
            // Two answers here; the third belongs to a dialog that declared
            // one (`Confirm::with_alternative`) and never reaches this.
            ConfirmOutcome::Keep | ConfirmOutcome::Alternative => true,
            ConfirmOutcome::Cancel => {
                self.confirm = None;
                self.destructive_action = None;
                true
            }
            ConfirmOutcome::Confirm => {
                self.confirm = None;
                let Some(action) = self.destructive_action.take() else {
                    return true;
                };
                match action {
                    // The pane stays open when it asked: the row leaves it
                    // once the list is asked again, and a refusal is said on
                    // the status line.
                    DestructiveAction::DeleteChats(chats) => chats
                        .into_iter()
                        .map(|chat| self.send_command(Command::DeleteChat(chat)))
                        .last()
                        .unwrap_or_default(),
                    DestructiveAction::ClearHistory => {
                        self.setup_open = false;
                        self.send_command(Command::ClearHistory)
                    }
                    DestructiveAction::ClearUsage => {
                        let chat = self
                            .transcript
                            .as_ref()
                            .map(|transcript| transcript.chat.id);
                        self.setup_open = false;
                        self.send_command(Command::ClearUsage(chat))
                    }
                }
            }
        }
    }

    fn apply_update(&mut self, update: Update) -> bool {
        if update.changes_chat_list() {
            self.chat_list_changed = true;
            self.chat_list_changed_for_pane = true;
        }
        match update {
            Update::Boot {
                settings,
                chats,
                transcript,
                destinations,
                destination_error,
            } => {
                // A tab opened for a new chat does not take the service's
                // current chat from the boot either — the last one used —
                // the same rule `HistoryChanged` keeps below. The chat it
                // asked for is created once the provider validates.
                let transcript = if self.fresh_chat_wanted {
                    None
                } else {
                    transcript
                };
                self.waiting = false;
                self.apply_settings_fields(&settings);
                self.setup_provider = selected_setup_provider(&settings);
                self.setup_agent = settings
                    .selected_provider
                    .clone()
                    .filter(agents::is_agent_provider);
                self.setup_open = true;
                self.validated_ready = false;
                let selected = settings.selected_provider.clone();
                self.settings = Some(settings);
                self.chats = chats;
                self.destinations = destinations;
                self.prioritize_context_workspace();
                self.workspace_catalog_error = destination_error;
                self.selected_destination = self
                    .selected_destination
                    .min(self.destinations.len().saturating_sub(1));
                if let Some(current) = &transcript {
                    self.retain_staged_quotes_for(current.chat.id);
                }
                self.expanded_tool_activity = transcript.as_ref().and_then(pending_tool_activity);
                self.scroll_loaded_chat_to_bottom = transcript.is_some();
                self.transcript = transcript;
                self.select_context_workspace_if_needed();
                self.messages_dirty = true;
                // A saved OpenAI credential is enough to discover the
                // account-scoped model catalog, even when another provider is
                // currently selected. This makes the picker ready when
                // Settings first opens instead of after a provider toggle.
                self.validate_models_after_settings(selected);
            }
            Update::Created(transcript) => {
                self.fresh_chat_wanted = false;
                self.forget_input_walk();
                self.retain_staged_quotes_for(transcript.chat.id);
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.draft.clear();
                self.remember_chat(&transcript.chat, true);
                self.transcript = Some(transcript);
                self.select_context_workspace_if_needed();
                self.usage_report = None;
                self.history_open = false;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.pending_question_anchor = None;
                self.scroll_to_pending_question = false;
                self.expanded_tool_activity = None;
                self.scroll_loaded_chat_to_bottom = false;
                self.messages_dirty = true;
            }
            Update::Transcript(transcript) => {
                self.retain_staged_quotes_for(transcript.chat.id);
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.live_reasoning.clear();
                self.reasoning_expanded = false;
                self.draft.clear();
                let changed_chat = self
                    .transcript
                    .as_ref()
                    .is_none_or(|current| current.chat.id != transcript.chat.id);
                if changed_chat || self.scroll_loaded_chat_to_bottom {
                    self.pending_question_anchor = None;
                    self.scroll_to_pending_question = false;
                }
                self.remember_chat(&transcript.chat, false);
                if changed_chat {
                    self.expanded_tool_activity = pending_tool_activity(&transcript);
                    self.forget_input_walk();
                }
                self.transcript = Some(transcript);
                if changed_chat {
                    self.select_context_workspace_if_needed();
                }
                self.usage_report = None;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.messages_dirty = true;
            }
            Update::Renamed(chat) => {
                self.waiting = false;
                self.remember_chat(&chat, true);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat;
                }
            }
            Update::ModelUpdated(chat, reasoning_control) => {
                self.waiting = false;
                self.chat_model_open = false;
                self.remember_chat(&chat, true);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat;
                    transcript.reasoning_control = reasoning_control;
                }
                self.status =
                    Some("This chat will use the selected model on its next turn.".into());
            }
            Update::PrivacyUpdated(chat) => {
                self.waiting = false;
                self.remember_chat(&chat, true);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat.clone();
                }
                self.status = Some(if chat.private {
                    "This chat is private to you.".into()
                } else {
                    "Agents and connected assistants can read this chat.".into()
                });
            }
            Update::FolderUpdated(chat) => {
                self.waiting = false;
                self.remember_chat(&chat, true);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat.clone();
                }
                self.status = None;
            }
            Update::ToolPolicy(policy) => {
                self.waiting = false;
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == policy.chat_id
                {
                    transcript.tool_policy = policy;
                }
                self.status = Some("Workspace access updated for this chat.".into());
            }
            Update::ToolActivity(activity) => {
                // A decision response is not the end of the provider turn;
                // keep polling the stream until its terminal event arrives.
                self.waiting = self.attempt.is_some();
                self.upsert_tool_activity(activity);
            }
            Update::Usage(report) => {
                self.waiting = false;
                self.usage_report = Some(report);
                self.messages_dirty = true;
                self.status = Some("Private local usage — no provider was contacted.".into());
            }
            Update::HistoryChanged { chats, transcript } => {
                // A tab that asked for a new chat does not take the
                // service's current one (the CTO's note): the snapshot's
                // chat is dropped and a chat is created below.
                let transcript = if self.fresh_chat_wanted {
                    None
                } else {
                    transcript
                };
                if let Some(current) = &transcript {
                    self.retain_staged_quotes_for(current.chat.id);
                } else {
                    self.pending_quotes.clear();
                    self.comment_editor = None;
                    self.staged_chat = None;
                    self.pill_strip.clear_tooltip();
                }
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.draft.clear();
                if self.transcript.as_ref().map(|t| t.chat.id)
                    != transcript.as_ref().map(|t| t.chat.id)
                {
                    self.forget_input_walk();
                }
                self.chats = chats;
                self.transcript = transcript;
                self.usage_report = None;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.pending_question_anchor = None;
                self.scroll_to_pending_question = false;
                self.messages_dirty = true;
                if self.transcript.is_none() && self.validated_ready {
                    self.create_chat();
                }
            }
            Update::Accepted(accepted) => {
                self.attempt = Some(accepted.attempt_id);
                self.pending_assistant_message_id = Some(accepted.assistant_message_id);
                self.messages_dirty = true;
            }
            Update::Event(event) => {
                let terminal = event.event.is_terminal();
                match &event.event {
                    PublicAiEvent::TextDelta { text, .. } => {
                        self.pulse = None;
                        self.draft.push_str(text);
                    }
                    PublicAiEvent::ReasoningDelta { text, .. } => {
                        self.live_reasoning.push_str(text);
                        self.reasoning_expanded = true;
                    }
                    PublicAiEvent::Usage { usage, .. } => {
                        self.live_usage = Some(usage.clone());
                    }
                    PublicAiEvent::ToolActivity { activity, .. } => {
                        self.upsert_tool_activity(activity.clone());
                    }
                    PublicAiEvent::Notice { notice, .. } => {
                        self.status = Some(match notice {
                            koru_ai_core::AttemptNotice::FreshStart => {
                                koru_i18n::t(self.lang, koru_i18n::Key::AiAgentStartedFresh)
                                    .to_owned()
                            }
                        });
                    }
                    PublicAiEvent::WebSearch { call_id, phase, .. } => match phase {
                        WebSearchPhase::Started => {
                            self.open_searches.insert(call_id.clone());
                        }
                        WebSearchPhase::Completed => {
                            self.open_searches.remove(call_id);
                        }
                    },
                    PublicAiEvent::Citation { source, .. } => {
                        if !self
                            .live_sources
                            .iter()
                            .any(|existing| existing.url == source.url)
                        {
                            self.live_sources.push(source.clone());
                        }
                    }
                    PublicAiEvent::Failed { error, .. } => {
                        self.status = Some(error.public_message.clone());
                    }
                    _ => {}
                }
                if terminal {
                    self.pulse = None;
                    self.open_searches.clear();
                    self.reasoning_expanded = false;
                }
                self.messages_dirty = true;
                if terminal {
                    if let Some(chat) = self.transcript.as_ref().map(|t| t.chat.id) {
                        self.send_command(Command::Read(chat));
                    } else {
                        self.waiting = false;
                    }
                }
            }
            Update::RecoveryStarted(attempt) => {
                if self.attempt == Some(attempt) {
                    self.waiting = true;
                    self.status =
                        Some("Connection interrupted. Recovering this response...".into());
                }
            }
            Update::Recovered {
                attempt,
                status,
                transcript,
            } => {
                if self.attempt != Some(attempt) {
                    tracing::debug!(
                        target: "koru_ai::ui_stream",
                        attempt = %attempt,
                        status = ?status,
                        "AI UI ignored recovery for an attempt that is no longer active"
                    );
                    return true;
                }
                let changed = self.apply_update(Update::Transcript(transcript));
                self.status = match status {
                    koru_ai_core::AttemptStatus::Complete => None,
                    koru_ai_core::AttemptStatus::Interrupted => {
                        Some("This response was interrupted. You can retry it.".into())
                    }
                    koru_ai_core::AttemptStatus::Failed => {
                        Some("This response failed. You can retry it.".into())
                    }
                    koru_ai_core::AttemptStatus::Cancelled => Some("Response stopped.".into()),
                    koru_ai_core::AttemptStatus::Pending
                    | koru_ai_core::AttemptStatus::Streaming => {
                        Some("Connection interrupted. Recovering this response...".into())
                    }
                };
                return changed;
            }
            Update::RecoveryFailed { attempt, message } => {
                if self.attempt != Some(attempt) {
                    tracing::debug!(
                        target: "koru_ai::ui_stream",
                        attempt = %attempt,
                        "AI UI ignored a recovery failure for an attempt that is no longer active"
                    );
                    return true;
                }
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.draft.clear();
                self.pending_question_anchor = None;
                self.scroll_to_pending_question = false;
                self.messages_dirty = true;
                self.status = Some(message);
            }
            Update::Settings(settings) => {
                self.waiting = false;
                self.validated_ready = false;
                self.setup_open = true;
                self.apply_settings_fields(&settings);
                if let Some(agent) = settings
                    .selected_provider
                    .clone()
                    .filter(agents::is_agent_provider)
                {
                    self.setup_agent = Some(agent);
                }
                let selected = settings.selected_provider.clone();
                self.settings = Some(settings);
                self.validate_models_after_settings(selected);
            }
            Update::AgentModels(response) => {
                self.agent_models.insert(
                    response.provider,
                    agents::AgentModels::Listed(response.models),
                );
            }
            Update::AgentModelsFailed(provider) => {
                self.agent_models
                    .insert(provider, agents::AgentModels::Failed);
            }
            Update::Validation(validation) => {
                tracing::info!(
                    target: "koru_ai::ui",
                    provider = %validation.provider,
                    status = ?validation.status,
                    model_count = validation.models.len(),
                    "AI UI applied provider validation"
                );
                self.waiting = false;
                match validation.provider.as_str() {
                    AiProviderId::OPENAI => self.openai_models = validation.models.clone(),
                    AiProviderId::OLLAMA => self.ollama_models = validation.models.clone(),
                    _ => {}
                }
                if agents::is_agent_provider(&validation.provider) {
                    let status = AgentStatus::from_validation(&validation);
                    self.agent_status
                        .insert(validation.provider.clone(), status);
                    self.messages_dirty = true;
                    if self.setup_provider == SetupProvider::Agents
                        && self.setup_agent.as_ref() == Some(&validation.provider)
                        && !status.selectable()
                    {
                        self.status = self.agent_not_ready_line(&validation.provider);
                    }
                }
                let current_provider = self.setup_provider_id();
                if current_provider.as_ref() != Some(&validation.provider) {
                    if let Some(provider) = self.pending_model_prefetch.take() {
                        self.send_model_prefetch(provider);
                    }
                    return true;
                }
                let validates_selected_provider = self
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.selected_provider.as_ref())
                    .is_some_and(|selected| selected == &validation.provider);
                if validates_selected_provider {
                    // Readiness describes the persisted provider used for the
                    // next turn, not whichever setup card happens to be open.
                    // A daemon or catalog that disappears during the session
                    // must revoke an earlier successful validation.
                    self.validated_ready = validation.status == ProviderValidationStatus::Ready;
                    // The provider is ready now and the tab still owes the
                    // fresh chat it was opened for.
                    if self.validated_ready && self.fresh_chat_wanted && self.transcript.is_none() {
                        self.create_chat();
                    }
                }
                self.constrain_reasoning(&validation);
                let mut discovered_model = false;
                if validation.provider.as_str() == AiProviderId::OPENAI
                    && !validation.models.is_empty()
                    && self.openai_model.is_empty()
                {
                    if let Some(model) = preferred_openai_model(&validation.models) {
                        self.openai_model.set_text(&mut self.engine, model);
                        self.model_options_open = true;
                        discovered_model = true;
                    }
                }
                if validation.status == ProviderValidationStatus::Ready
                    && validates_selected_provider
                {
                    self.setup_open = false;
                    self.status = None;
                    if self.transcript.is_none() {
                        self.create_chat();
                    }
                } else if validation.status == ProviderValidationStatus::Ready {
                    self.status = Some("Provider ready. Save & check to use it.".into());
                } else if discovered_model {
                    self.status = Some("Choose an available model, then Save & check.".into());
                } else if validation.provider.as_str() == AiProviderId::KORU
                    && validation.status == ProviderValidationStatus::Unconfigured
                {
                    self.status =
                        Some(koru_i18n::t(self.lang, koru_i18n::Key::AiKoruUnconfigured).into());
                } else if validation.provider.as_str() == AiProviderId::KORU
                    && validation.status == ProviderValidationStatus::Unavailable
                {
                    self.status =
                        Some(koru_i18n::t(self.lang, koru_i18n::Key::AiKoruUnreachable).into());
                } else if let Some(failure) = &validation.failure {
                    self.status = Some(failure_text(failure, self.lang));
                } else {
                    self.status = Some(validation_status_text(validation.status).into());
                }
                self.validation = Some(validation);
                if let Some(provider) = self.pending_model_prefetch.take() {
                    self.send_model_prefetch(provider);
                }
            }
            Update::Pull(event) => {
                self.pull_status = Some(match event {
                    ModelPullEvent::Progress { status, .. } => {
                        self.pull_active = true;
                        status
                    }
                    ModelPullEvent::Completed { model } => {
                        self.pull_active = false;
                        self.waiting = false;
                        let provider = AiProviderId::parse(AiProviderId::OLLAMA)
                            .expect("built-in provider id");
                        self.send_command(Command::Validate(provider));
                        format!("{model} is ready")
                    }
                    ModelPullEvent::Failed { failure } => {
                        self.pull_active = false;
                        self.waiting = false;
                        failure_text(&failure, self.lang)
                    }
                    ModelPullEvent::Cancelled { model } => {
                        self.pull_active = false;
                        self.waiting = false;
                        format!("Stopped pulling {model}")
                    }
                });
            }
            Update::DocumentTitles(titles) => {
                // Another command may still be streaming; keep polling for it.
                self.waiting = self.attempt.is_some();
                let mut changed = false;
                for (document, title) in titles {
                    if !title.trim().is_empty()
                        && self.document_titles.get(&document) != Some(&title)
                    {
                        self.document_titles.insert(document, title);
                        changed = true;
                    }
                }
                // The shown text of every message naming it changes, so the
                // layout cache relays those cards and no others.
                self.messages_dirty |= changed;
            }
            Update::Exported(title) => {
                self.waiting = false;
                self.destination_open = None;
                self.status = Some(format!("Stored “{title}” in Koru."));
            }
            Update::Failure(message) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI received a client failure"
                );
                if self.pull_active {
                    self.pull_active = false;
                    self.pull_status = None;
                }
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.status = Some(message);
            }
        }
        true
    }

    fn remember_chat(&mut self, chat: &Chat, promote: bool) {
        if let Some(index) = self
            .chats
            .iter()
            .position(|candidate| candidate.id == chat.id)
        {
            self.chats[index] = chat.clone();
            if promote && index != 0 {
                let chat = self.chats.remove(index);
                self.chats.insert(0, chat);
            }
        } else if promote {
            self.chats.insert(0, chat.clone());
        } else {
            self.chats.push(chat.clone());
        }
    }

    fn upsert_tool_activity(&mut self, mut activity: ToolActivity) {
        let should_auto_approve = activity.status == ToolActivityStatus::ApprovalRequired
            && activity.approval == ToolApproval::Pending
            && self.session_auto_approve_chats.contains(&activity.chat_id)
            && self.session_approval_sent.insert(activity.id);
        let activity_id = activity.id;
        let Some(transcript) = &mut self.transcript else {
            return;
        };
        if transcript.chat.id != activity.chat_id {
            return;
        }
        let should_auto_approve = should_auto_approve && !is_agent_row(&activity);
        if !should_auto_approve
            && activity.status == ToolActivityStatus::ApprovalRequired
            && activity.approval == ToolApproval::Pending
        {
            self.expanded_tool_activity = Some(activity.id);
        }
        if let Some(existing) = transcript
            .tool_activities
            .iter_mut()
            .find(|existing| existing.id == activity.id)
        {
            if activity.display_target.is_none() {
                activity.display_target.clone_from(&existing.display_target);
            }
            *existing = activity;
        } else {
            transcript.tool_activities.push(activity);
            transcript
                .tool_activities
                .sort_by_key(|activity| activity.proposed_at);
        }
        self.messages_dirty = true;
        if should_auto_approve {
            self.expanded_tool_activity = None;
            self.send_command(Command::ApproveTool(activity_id));
        }
    }

    fn toggle_session_auto_approve(&mut self) -> bool {
        let Some(chat_id) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id)
        else {
            return false;
        };
        if !self.session_auto_approve_chats.insert(chat_id) {
            self.session_auto_approve_chats.remove(&chat_id);
            return true;
        }
        let pending: Vec<_> = self
            .transcript
            .as_ref()
            .into_iter()
            .flat_map(|transcript| &transcript.tool_activities)
            .filter(|activity| {
                activity.status == ToolActivityStatus::ApprovalRequired
                    && activity.approval == ToolApproval::Pending
            })
            .map(|activity| activity.id)
            .filter(|id| self.session_approval_sent.insert(*id))
            .collect();
        self.expanded_tool_activity = None;
        for tool in pending {
            self.send_command(Command::ApproveTool(tool));
        }
        true
    }

    fn auto_approve_tool_for_session(&mut self, tool: ToolInvocationId) -> bool {
        let Some(chat_id) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id)
        else {
            return false;
        };
        self.session_auto_approve_chats.insert(chat_id);
        self.session_approval_sent.insert(tool);
        self.expanded_tool_activity = None;
        self.send_command(Command::ApproveTool(tool))
    }

    fn update_tool_policy(&mut self, mode: ToolMode, workspace_ids: Vec<WorkspaceId>) -> bool {
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id)
        else {
            return false;
        };
        self.send_command(Command::UpdateToolPolicy(
            chat,
            UpdateChatToolPolicyRequest {
                mode,
                workspace_ids,
            },
        ))
    }

    fn prioritize_context_workspace(&mut self) {
        let Some(current) = self.context_workspace else {
            return;
        };
        let Some(index) = self
            .destinations
            .iter()
            .position(|destination| destination.workspace_id == current)
        else {
            return;
        };
        if index != 0 {
            let current = self.destinations.remove(index);
            self.destinations.insert(0, current);
        }
    }

    fn select_context_workspace_if_needed(&mut self) -> bool {
        let Some(workspace) = self.context_workspace else {
            return false;
        };
        let Some((chat, mode, mut workspace_ids)) = self.transcript.as_ref().map(|transcript| {
            (
                transcript.chat.id,
                transcript.tool_policy.mode,
                transcript.tool_policy.workspace_ids.clone(),
            )
        }) else {
            return false;
        };
        if workspace_ids.contains(&workspace) {
            return false;
        }
        workspace_ids.insert(0, workspace);
        if let Some(transcript) = &mut self.transcript {
            transcript.tool_policy.workspace_ids = workspace_ids.clone();
        }
        self.send_command(Command::UpdateToolPolicy(
            chat,
            UpdateChatToolPolicyRequest {
                mode,
                workspace_ids,
            },
        ))
    }

    fn toggle_tool_workspace(&mut self, workspace: WorkspaceId) -> bool {
        let Some(policy) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.tool_policy.clone())
        else {
            return false;
        };
        let mut workspaces = policy.workspace_ids;
        if let Some(index) = workspaces
            .iter()
            .position(|candidate| *candidate == workspace)
        {
            workspaces.remove(index);
        } else {
            workspaces.push(workspace);
            workspaces.sort();
        }
        self.update_tool_policy(policy.mode, workspaces)
    }

    fn create_chat(&mut self) -> bool {
        let Some(selection) = self.current_selection() else {
            self.setup_open = true;
            self.status = Some("Choose and validate a provider first.".into());
            return true;
        };
        self.send_command(Command::Create(CreateChatRequest {
            chat_id: ChatId::new(),
            title: DEFAULT_CHAT_TITLE.into(),
            model: selection,
        }))
    }

    fn current_selection(&self) -> Option<ModelSelection> {
        let settings = self.settings.as_ref()?;
        let provider = settings.selected_provider.clone()?;
        let (requested_model, default_reasoning) = match provider.as_str() {
            AiProviderId::OPENAI if settings.openai.credential_configured => {
                (settings.openai.model.clone()?, settings.openai.reasoning)
            }
            AiProviderId::OLLAMA => (settings.ollama.model.clone()?, settings.ollama.reasoning),
            AiProviderId::KORU => (AiProviderId::KORU_HOSTED_MODEL.to_owned(), self.reasoning),
            _ if agents::is_agent_provider(&provider) => {
                (AGENT_DEFAULT_MODEL.to_owned(), ReasoningChoice::Default)
            }
            _ => return None,
        };
        Some(ModelSelection {
            provider,
            requested_model,
            default_reasoning,
        })
    }

    /// Move between authored user turns without making transcript navigation a
    /// text-editing operation. The final downward step returns keyboard focus
    /// to the fixed composer.
    fn navigate_questions(&mut self, down: bool) -> bool {
        let area = self.conversation_rect();
        if self.messages_dirty || (self.message_width - area.w).abs() > 0.5 {
            self.rebuild_messages(area.w);
        }
        let questions = self
            .message_layouts
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .map(|message| message.top)
            .collect::<Vec<_>>();
        let content_extent = self.conversation_content_extent();
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] AI chat question-navigation questions={} current={:?} focus={:?} scroll={:.1}",
            questions.len(),
            self.question_navigation,
            self.focus,
            self.conversation_scroll,
        ));
        if questions.is_empty() {
            self.focus = Focus::Composer;
            self.question_navigation = None;
            self.question_navigation_anchor = None;
            return true;
        }

        let target = if down {
            match self.question_navigation {
                Some(index) if index + 1 < questions.len() => Some(index + 1),
                Some(_) => None,
                None if self.focus == Focus::Composer => None,
                None => questions
                    .iter()
                    .position(|top| *top > self.conversation_scroll + 1.0),
            }
        } else {
            match self.question_navigation {
                Some(index) => index.checked_sub(1).or(Some(0)),
                None if self.focus == Focus::Composer => Some(questions.len() - 1),
                None => questions
                    .iter()
                    .rposition(|top| *top < self.conversation_scroll - 1.0)
                    .or(Some(0)),
            }
        };

        let Some(index) = target else {
            self.focus = Focus::Composer;
            self.question_navigation = None;
            self.question_navigation_anchor = None;
            self.conversation_extent = content_extent;
            self.conversation_scroll = self
                .conversation_scroll
                .min((self.conversation_extent - area.h).max(0.0));
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] AI chat question-navigation target=composer scroll={:.1}",
                self.conversation_scroll,
            ));
            return true;
        };
        let target_top = questions[index];
        self.focus = Focus::None;
        self.response_selection = None;
        self.question_navigation = Some(index);
        self.question_navigation_anchor = Some(target_top);
        self.conversation_extent = content_extent.max(target_top + area.h);
        self.conversation_scroll = target_top;
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] AI chat question-navigation target={} top={target_top:.1}",
            index + 1,
        ));
        true
    }

    fn send_composer(&mut self) -> bool {
        if self.attempt.is_some() {
            return false;
        }
        // **A provisional dictation is not sent.** The words are still being
        // heard; the send waits for them to become text.
        if self.composer.composing() {
            return true;
        }
        let message = self.composer.message();
        let typed = message.markdown;
        if typed.trim().is_empty() && self.pending_quotes.is_empty() {
            return false;
        }
        let Some(chat_id) = self.transcript.as_ref().map(|t| t.chat.id) else {
            self.status = Some("The chat is still opening.".into());
            return true;
        };
        if typed.trim() == "/usage" {
            self.composer.clear();
            self.usage_report = None;
            self.messages_dirty = true;
            self.status = Some("Loading private local usage…".into());
            return self.send_command(Command::Usage(chat_id));
        }
        let markdown = compose_markdown(&self.pending_quotes, &typed);
        if markdown.is_empty() {
            return false;
        }
        // A chat still called *New chat* takes its name from this, its
        // first message (the CTO's note): the way ChatGPT names a chat,
        // without a model call — the first line, cut at a word.
        let auto_title = self
            .transcript
            .as_ref()
            .filter(|transcript| transcript.chat.title == DEFAULT_CHAT_TITLE)
            .and_then(|_| title_from_message(&typed));
        self.usage_report = None;
        // A chip's caption is the document's title as it was chosen: the sent
        // card reads it at once, before any lookup.
        for (document, title) in &message.documents {
            if !title.trim().is_empty() {
                self.document_titles.insert(*document, title.clone());
            }
        }
        let chips: Vec<DocumentId> = message
            .documents
            .iter()
            .map(|(document, _)| *document)
            .collect();
        let (explicit_refs, overflowed) = self.turn_refs(&chips);
        self.pending_user = Some(markdown.clone());
        self.pending_quotes.clear();
        self.staged_chat = None;
        self.comment_editor = None;
        self.pill_strip.clear_tooltip();
        self.pulse = Some(PulsingDots::new(Instant::now()));
        self.live_usage = None;
        self.live_sources.clear();
        self.open_searches.clear();
        self.live_reasoning.clear();
        self.reasoning_expanded = self.effective_reasoning() != Some(ReasoningChoice::Off);
        self.pending_assistant_message_id = None;
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        // The next layout pass knows the exact top of this optimistic user
        // card. Move there once; streaming below it must not keep moving the
        // reader after that initial, intentional navigation.
        self.scroll_to_pending_question = true;
        self.pending_question_anchor = None;
        self.draft.clear();
        self.composer.clear();
        self.forget_input_walk();
        self.sent_inputs.push((chat_id, markdown.clone()));
        self.messages_dirty = true;
        let request = SendTurnRequest {
            operation_id: uuid::Uuid::new_v4(),
            chat_id,
            surface: self.mode.wire(),
            markdown,
            explicit_refs,
            web_search: self.web_search_armed,
            // The chip sets the chat's persisted effort (R5); no per-turn
            // override rides the request.
            reasoning_override: None,
            system_prompt_override: self
                .debug_prompt_enabled
                .then(|| self.system_prompt.value())
                .filter(|prompt| !prompt.trim().is_empty()),
            replace_pinned_documents: self.mode == SurfaceMode::ContextPane,
        };
        let sent = if self.mode == SurfaceMode::ContextPane && self.context_document.is_some() {
            self.staged_embedded_send = Some(request);
            self.durable_observations = 0;
            self.waiting = true;
            true
        } else {
            self.send_command(Command::Send(request))
        };
        if sent && let Some(title) = auto_title {
            self.send_command(Command::Rename(chat_id, RenameChatRequest { title }));
        }
        // After the send, which clears the status line on success.
        if overflowed {
            self.too_many_documents();
        }
        sent
    }

    /// The documents a turn carries, as `explicit_refs`.
    ///
    /// This message's chips first, then the document the host is showing.
    /// In the context pane a turn **replaces** the chat's pinned documents
    /// rather than adding to them (`replace_pinned_documents`), so there every
    /// document an earlier message of this chat named by chip is carried again,
    /// newest first: a chip's document stays pinned in both hosts. Never more
    /// than the worker accepts ([`koru_ai_wire::MAX_CONTEXT_REFS`]); the flag
    /// says this message alone asked for more, so the caller can say so.
    fn turn_refs(&self, chips: &[DocumentId]) -> (Vec<ContextRef>, bool) {
        let mut documents: Vec<DocumentId> = Vec::new();
        let push = |documents: &mut Vec<DocumentId>, document: DocumentId| {
            if !documents.contains(&document) {
                documents.push(document);
            }
        };
        for document in chips {
            push(&mut documents, *document);
        }
        if let Some((document, _)) = &self.context_document {
            push(&mut documents, *document);
        }
        let asked = documents.len();
        if self.mode == SurfaceMode::ContextPane
            && let Some(transcript) = &self.transcript
        {
            for message in transcript
                .messages
                .iter()
                .rev()
                .filter(|message| message.role == ChatRole::User)
            {
                for document in doc_links::documents(&message.markdown) {
                    push(&mut documents, document);
                }
            }
        }
        documents.truncate(koru_ai_wire::MAX_CONTEXT_REFS);
        let refs = documents
            .into_iter()
            .map(|document_id| ContextRef::Document { document_id })
            .collect();
        (refs, asked > koru_ai_wire::MAX_CONTEXT_REFS)
    }

    /// Ask the reader thread, once each, for the titles of documents that
    /// chips in this chat name and nothing here knows yet. The pinned
    /// documents' own titles are taken first; the store is asked only for the
    /// rest, and never from this thread.
    fn request_document_titles(&mut self) {
        let Some(transcript) = &self.transcript else {
            return;
        };
        for pinned in &transcript.pinned_documents {
            if !pinned.display_title.trim().is_empty() {
                self.document_titles
                    .entry(pinned.document_id)
                    .or_insert_with(|| pinned.display_title.clone());
            }
        }
        let wanted: Vec<DocumentId> = transcript
            .messages
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .flat_map(|message| doc_links::documents(&message.markdown))
            .filter(|document| {
                !self.document_titles.contains_key(document)
                    && !self.titles_requested.contains(document)
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Straight to the reader rather than `send_command`: a background
        // lookup must neither clear the status line nor say "busy" on it. A
        // full queue simply asks again on the next rebuild.
        let Some(reader) = &self.reader else {
            return;
        };
        if reader.send(Command::DocumentTitles(wanted.clone())).is_ok() {
            self.waiting = true;
            self.titles_requested.extend(wanted);
        }
    }

    /// Say that some of this message's documents did not go with it.
    fn too_many_documents(&mut self) {
        let limit = koru_ai_wire::MAX_CONTEXT_REFS.to_string();
        self.status = Some(koru_i18n::fill(
            koru_i18n::t(self.lang, koru_i18n::Key::AiTooManyDocuments),
            &[&limit],
        ));
    }

    fn apply_settings_fields(&mut self, settings: &ProviderSettingsResponse) {
        if let Some(model) = &settings.openai.model {
            self.openai_model.set_text(&mut self.engine, model);
        } else {
            self.openai_model.set_text(&mut self.engine, "");
        }
        self.ollama_endpoint
            .set_text(&mut self.engine, &settings.ollama.endpoint);
        if let Some(model) = &settings.ollama.model {
            self.ollama_model.set_text(&mut self.engine, model);
        }
        self.reasoning = match selected_setup_provider(settings) {
            SetupProvider::OpenAi => settings.openai.reasoning,
            SetupProvider::Ollama => settings.ollama.reasoning,
            SetupProvider::Koru | SetupProvider::Agents => ReasoningChoice::Default,
        };
    }

    fn setup_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        if self.focus == Focus::OllamaModel
            && matches!(key, Key::Named(NamedKey::Enter))
            && !koru_keys::is_chord(mods)
            && self.ollama_pull_action_rect().is_some()
        {
            return self.pull_ollama_model();
        }
        if self.focus == Focus::SystemPrompt {
            return self.system_prompt.key(&mut self.engine, key, text, mods) == Typed::Handled;
        }
        let field = match self.focus {
            Focus::OpenAiKey => Some(&mut self.openai_key),
            Focus::OpenAiModel => Some(&mut self.openai_model),
            Focus::OllamaEndpoint => Some(&mut self.ollama_endpoint),
            Focus::OllamaModel => Some(&mut self.ollama_model),
            _ => None,
        };
        field.is_some_and(|field| field.key(&mut self.engine, key, text, mods) == Typed::Handled)
    }

    fn setup_mouse_down(&mut self, x: f32, y: f32) -> bool {
        if self.setup_open && self.transcript.is_some() && self.setup_close_button().contains(x, y)
        {
            self.setup_open = false;
            self.focus = Focus::Composer;
            return true;
        }
        if self
            .ollama_search_link
            .is_some_and(|rect| rect.contains(x, y))
        {
            if let Err(error) = koru_window::open_uri("https://ollama.com/search") {
                self.status = Some(format!("Could not open the Ollama model catalog: {error}"));
            }
            return true;
        }
        if self
            .ollama_pull_action_rect()
            .is_some_and(|rect| rect.contains(x, y))
        {
            return self.pull_ollama_model();
        }
        if self.model_options_open {
            if let Some(index) = self.model_option_at(x, y)
                && let Some(model) = self.model_options().get(index).cloned()
            {
                match self.setup_provider {
                    SetupProvider::OpenAi => self.openai_model.set_text(&mut self.engine, &model),
                    SetupProvider::Ollama => self.ollama_model.set_text(&mut self.engine, &model),
                    SetupProvider::Koru | SetupProvider::Agents => {}
                }
                self.model_options_open = false;
                self.focus = match self.setup_provider {
                    SetupProvider::OpenAi => Focus::OpenAiModel,
                    SetupProvider::Ollama => Focus::OllamaModel,
                    SetupProvider::Koru | SetupProvider::Agents => Focus::None,
                };
                return true;
            }
            if !self.field_rects()[1].contains(x, y) {
                self.model_options_open = false;
            }
        }
        if self.retry_link.is_some_and(|rect| rect.contains(x, y)) {
            let Some(provider) = self.setup_provider_id() else {
                return true;
            };
            return self.send_command(Command::Validate(provider));
        }
        if self.transcript.is_some() && self.clear_history_button().contains(x, y) {
            self.open_confirmation(DestructiveAction::ClearHistory);
            return true;
        }
        if self.transcript.is_some() && self.clear_usage_button().contains(x, y) {
            self.open_confirmation(DestructiveAction::ClearUsage);
            return true;
        }
        let cards = self.provider_cards();
        if cards[0].contains(x, y) {
            self.setup_provider = SetupProvider::Koru;
            self.model_options_open = false;
            self.model_option_offset = 0;
            self.reasoning = ReasoningChoice::Default;
            self.focus = Focus::None;
            self.status = Some(koru_i18n::t(self.lang, koru_i18n::Key::AiKoruHostedStatus).into());
            return true;
        }
        if cards[1].contains(x, y) {
            self.setup_provider = SetupProvider::OpenAi;
            self.model_options_open = false;
            self.model_option_offset = 0;
            self.reasoning = self
                .settings
                .as_ref()
                .map_or(ReasoningChoice::Default, |settings| {
                    settings.openai.reasoning
                });
            self.focus = Focus::OpenAiModel;
            return self.save_setup();
        }
        if cards[3].contains(x, y) {
            if self
                .settings
                .as_ref()
                .is_some_and(|settings| !settings.agents.is_empty())
            {
                self.setup_provider = SetupProvider::Agents;
                self.model_options_open = false;
                self.model_option_offset = 0;
                self.reasoning = ReasoningChoice::Default;
                self.focus = Focus::None;
                self.status = None;
                self.refresh_agent_status();
            }
            return true;
        }
        if cards[2].contains(x, y) {
            self.setup_provider = SetupProvider::Ollama;
            self.model_options_open = false;
            self.model_option_offset = 0;
            self.reasoning = self
                .settings
                .as_ref()
                .map_or(ReasoningChoice::Default, |settings| {
                    settings.ollama.reasoning
                });
            self.focus = Focus::OllamaEndpoint;
            return self.save_setup();
        }
        let fields = self.field_rects();
        if self.debug_prompt_enabled
            && let Some(prompt) = self.system_prompt_rect()
            && self.system_prompt.click(&mut self.engine, prompt, x, y)
        {
            self.focus = Focus::SystemPrompt;
            return true;
        }
        match self.setup_provider {
            SetupProvider::OpenAi => {
                if self.openai_credential_clear_button().contains(x, y)
                    && self.openai_credential_configured()
                    && !self.editing_openai_key
                {
                    self.pending_openai_settings = Some((
                        self.settings.as_ref().and_then(|s| s.openai.model.clone()),
                        self.reasoning,
                    ));
                    self.credential_request = Some(OpenAiCredentialRequest::Clear);
                    self.status = Some("Removing the saved key…".into());
                    return true;
                }
                if self.openai_credential_replace_button().contains(x, y)
                    && self.openai_credential_configured()
                    && !self.editing_openai_key
                {
                    self.editing_openai_key = true;
                    self.focus = Focus::OpenAiKey;
                    return true;
                }
                if self.openai_key.click(&mut self.engine, fields[0], x, y) {
                    self.editing_openai_key = true;
                    self.focus = Focus::OpenAiKey;
                    return true;
                }
                if self.openai_model.click(&mut self.engine, fields[1], x, y) {
                    self.focus = Focus::OpenAiModel;
                    self.model_option_offset = 0;
                    self.model_options_open = !self.model_options().is_empty();
                    return true;
                }
            }
            SetupProvider::Ollama => {
                if self
                    .ollama_endpoint
                    .click(&mut self.engine, fields[0], x, y)
                {
                    self.focus = Focus::OllamaEndpoint;
                    return true;
                }
                if self.ollama_model.click(&mut self.engine, fields[1], x, y) {
                    self.focus = Focus::OllamaModel;
                    self.model_option_offset = 0;
                    self.model_options_open = !self.model_options().is_empty();
                    return true;
                }
            }
            SetupProvider::Koru => {}
            SetupProvider::Agents => {
                if self.click_agent_row(x, y) {
                    return true;
                }
            }
        }
        if self.reasoning_button().contains(x, y) && !self.reasoning_choices().is_empty() {
            self.cycle_reasoning();
            return true;
        }
        if self.setup_save_button().contains(x, y) {
            return self.save_setup();
        }
        false
    }

    fn drag_setup_input(&mut self, x: f32) -> bool {
        let fields = self.field_rects();
        self.openai_key.drag(&mut self.engine, fields[0], x)
            || self.openai_model.drag(&mut self.engine, fields[1], x)
            || self.ollama_endpoint.drag(&mut self.engine, fields[0], x)
            || self.ollama_model.drag(&mut self.engine, fields[1], x)
            || self.system_prompt_rect().is_some_and(|rect| {
                self.system_prompt
                    .drag(&mut self.engine, rect, x, self.pointer.1)
            })
    }

    fn save_setup(&mut self) -> bool {
        match self.setup_provider {
            SetupProvider::Agents => {
                let Some(agent) = self.setup_agent.clone() else {
                    self.status =
                        Some(koru_i18n::t(self.lang, koru_i18n::Key::AiAgentPickOne).into());
                    return true;
                };
                if !self
                    .agent_status
                    .get(&agent)
                    .is_some_and(|status| status.selectable())
                {
                    self.status = self.agent_not_ready_line(&agent);
                    self.refresh_agent_status();
                    return true;
                }
                self.status = None;
                self.send_command(Command::SelectProvider(SelectProviderRequest {
                    provider: agent,
                }))
            }
            SetupProvider::Koru => {
                self.status =
                    Some(koru_i18n::t(self.lang, koru_i18n::Key::AiKoruHostedStatus).into());
                self.send_command(Command::SelectProvider(SelectProviderRequest {
                    provider: AiProviderId::parse(AiProviderId::KORU)
                        .expect("built-in provider id"),
                }))
            }
            SetupProvider::OpenAi => {
                let model = (!self.openai_model.is_empty()).then(|| self.openai_model.value());
                if !self.openai_key.is_empty() {
                    self.pending_openai_settings = Some((model, self.reasoning));
                    self.credential_request =
                        Some(OpenAiCredentialRequest::Save(self.openai_key.value()));
                    self.openai_key.set_text(&mut self.engine, "");
                    self.editing_openai_key = false;
                    self.status = Some("Saving the key securely…".into());
                    return true;
                }
                if self
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.openai.credential_configured)
                {
                    let Some(model) = model else {
                        self.status = Some("Choose an available OpenAI model.".into());
                        self.model_options_open = true;
                        return true;
                    };
                    return self.send_command(Command::UpdateOpenAi(UpdateOpenAiSettingsRequest {
                        model: Some(model),
                        reasoning: self.reasoning,
                    }));
                }
                self.status = Some("Enter an OpenAI project API key.".into());
                true
            }
            SetupProvider::Ollama => {
                let endpoint = self.ollama_endpoint.value();
                let model = self.ollama_model.value();
                self.send_command(Command::UpdateOllama(UpdateOllamaSettingsRequest {
                    endpoint,
                    model: (!model.is_empty()).then_some(model),
                    reasoning: self.reasoning,
                }))
            }
        }
    }

    fn pull_ollama_model(&mut self) -> bool {
        if self.pull_active {
            self.pull_status = Some("Stopping model pull…".into());
            return self.send_command(Command::CancelPull);
        }
        let model = self.ollama_model.value();
        if model.is_empty() {
            return false;
        }
        let handled = self.send_command(Command::Pull(PullOllamaModelRequest {
            model: model.clone(),
        }));
        if self.waiting {
            self.pull_active = true;
            self.pull_status = Some(format!("Starting {model}…"));
        }
        handled
    }

    fn reasoning_choices(&self) -> Vec<ReasoningChoice> {
        let Some(validation) = &self.validation else {
            return Vec::new();
        };
        if self.setup_provider_id().as_ref() != Some(&validation.provider) {
            return Vec::new();
        }
        validation
            .selected_model
            .as_ref()
            .map_or_else(Vec::new, |model| {
                choices_for_reasoning(&model.capabilities.reasoning)
            })
    }

    fn constrain_reasoning(&mut self, validation: &ProviderValidationResponse) {
        let Some(model) = &validation.selected_model else {
            return;
        };
        let choices = choices_for_reasoning(&model.capabilities.reasoning);
        if choices.is_empty() {
            self.reasoning = ReasoningChoice::Default;
        } else if !choices.contains(&self.reasoning) {
            self.reasoning = choices
                .contains(&model.selection.default_reasoning)
                .then_some(model.selection.default_reasoning)
                .unwrap_or(choices[0]);
        }
        if validation.provider.as_str() != AiProviderId::OPENAI || !model.capabilities.web_search {
            self.web_search_armed = false;
        }
    }

    fn cycle_reasoning(&mut self) {
        let choices = self.reasoning_choices();
        if let Some(index) = choices.iter().position(|choice| *choice == self.reasoning) {
            self.reasoning = choices[(index + 1) % choices.len()];
        } else if let Some(first) = choices.first() {
            self.reasoning = *first;
        }
    }

    fn content_rect(&self) -> Rect {
        let width = (self.viewport.w - PAD * 2.0).max(1.0).min(CONTENT_MAX_W);
        Rect::new(
            self.viewport.x + (self.viewport.w - width) * 0.5,
            self.viewport.y,
            width,
            self.viewport.h,
        )
    }

    /// Where the conversation may start: below the standalone window's
    /// header buttons, or a margin's worth below the top in a workspace,
    /// where the surface has no header row of its own.
    fn chat_top(&self) -> f32 {
        self.viewport.y
            + if self.hosts_header_buttons() {
                HEADER_H
            } else {
                PAD
            }
    }

    /// **A chat with nothing in it yet** — no message, none on its way, no
    /// answer streaming — opens on a greeting with the composer centred under
    /// it. A chat still loading from a link is not empty: it has messages
    /// the surface has not read yet.
    fn chat_is_empty(&self) -> bool {
        if self.pending_user.is_some()
            || self.attempt.is_some()
            || self.staged_embedded_send.is_some()
            || self.usage_report.is_some()
        {
            return false;
        }
        match &self.transcript {
            Some(transcript) => transcript.messages.is_empty(),
            None => self.pending_link_chat.is_none(),
        }
    }

    /// The empty chat's greeting, picked at random the first time this chat
    /// shows it and held while it is shown. A chat made for a surface that
    /// was waiting for one keeps the line it was already showing; another
    /// chat gets a new one.
    fn greeting(&mut self) -> Option<koru_i18n::Key> {
        if !self.chat_is_empty() {
            return None;
        }
        let chat = self.transcript.as_ref().map(|t| t.chat.id);
        let index = match self.greeting {
            Some((shown, index)) if shown == chat || shown.is_none() => index,
            _ => (uuid::Uuid::new_v4().as_u128() % GREETINGS.len() as u128) as usize,
        };
        self.greeting = Some((chat, index));
        Some(GREETINGS[index])
    }

    /// The composer's box: its text, one to eight lines, over the toolbar
    /// row, inside the padding. At the bottom of a chat with messages;
    /// centred, under the greeting, in an empty one. Everything on or around
    /// the composer — the send button, the toolbar, the `@` picker's room,
    /// the conversation above, the quiet row below — derives from this.
    fn composer_rect(&mut self) -> Rect {
        let content = self.content_rect();
        let width = content.w.max(MIN_COMPOSER_W);
        // One to eight visual lines of the composer's own text, then it
        // scrolls — measured by the editor at this width.
        let text_h = self
            .composer
            .text_height(
                (width - COMPOSER_PAD_X * 2.0).max(1.0),
                COMPOSER_MIN_LINES,
                COMPOSER_MAX_LINES,
            )
            .ceil();
        let height = COMPOSER_PAD_TOP
            + text_h
            + COMPOSER_TEXT_GAP
            + COMPOSER_TOOLBAR_H
            + COMPOSER_PAD_BOTTOM;
        // The quiet row under the box, when there is one, is part of the
        // block: the box stands on it rather than on the window's edge.
        let below = self.composer_footer_h();
        let bottom = self.viewport.y + self.viewport.h - PAD - below - height;
        let y = if self.chat_is_empty() {
            // The greeting, the composer and its row as one block, centred;
            // never lower than where the composer sits once the chat has begun.
            let above = role::title().metrics.line_height + GREETING_GAP + COMPOSER_GAP;
            (self.viewport.y + (self.viewport.h - above - height - below) * 0.5 + above).min(bottom)
        } else {
            bottom
        };
        let rect = Rect::new(content.x, y, width, height);
        self.composer_hit_rect = rect;
        if below > 0.0 {
            // The folder control is as wide as what it shows.
            let label = self.folder_label_shown();
            let icon = self
                .engine
                .icon_layout(icons::FOLDER, ActionButton::ICON_SIZE)
                .width();
            let text = self.engine.simple_layout(&label, role::meta()).width();
            self.folder_button_w = ActionButton::PAD_X * 2.0 + icon + ActionButton::GAP + text;
        }
        // The picker may float anywhere above the composer's text, below
        // the header: over the conversation, never over the toolbar.
        let text = self.composer_text_rect();
        let top = self.chat_top();
        let panels = Rect::new(
            self.viewport.x,
            top,
            self.viewport.w,
            (text.y + text.h - top).max(1.0),
        );
        self.composer.place(text, panels);
        rect
    }

    /// The composer's text box, padding included: everything above the
    /// toolbar row. The editor insets its words by the same padding
    /// ([`composer`]).
    fn composer_text_rect(&self) -> Rect {
        Rect::new(
            self.composer_hit_rect.x,
            self.composer_hit_rect.y,
            self.composer_hit_rect.w,
            (self.composer_hit_rect.h - COMPOSER_TOOLBAR_H - COMPOSER_PAD_BOTTOM).max(1.0),
        )
    }

    /// Whether the header carries *Chats*, *New chat* and *Settings*: the
    /// standalone window's does; in a workspace those doors are the rail's,
    /// the Chats heading's and the rail's again (the CTO's note), and the
    /// surface has no header row at all.
    fn hosts_header_buttons(&self) -> bool {
        self.mode == SurfaceMode::Standalone
    }

    fn conversation_rect(&mut self) -> Rect {
        let composer = self.composer_rect();
        let content = self.content_rect();
        let footer = self.footer_line().map(str::to_owned);
        let footer_h = footer.map_or(0.0, |line| {
            self.engine
                .simple_layout_wrapped(&line, role::meta(), content.w)
                .height()
                + 5.0
        });
        let context_document = self.context_document_pill();
        let top = self.chat_top();
        let rect = Rect::new(
            content.x,
            top,
            content.w,
            (composer.y
                - COMPOSER_GAP
                - pill_band_height(context_document.as_ref(), &self.pending_quotes)
                - footer_h
                - top)
                .max(1.0),
        );
        self.conversation_hit_rect = rect;
        rect
    }

    /// The transcript uses the same overlay scrollbar geometry as documents.
    /// Its vertical band follows the centred conversation, while its thumb
    /// sits at the host viewport's right edge like the document scrollbar.
    fn conversation_scrollbar(&self) -> Option<koru_ui::scrollbar::ScrollbarGeom> {
        if self.showing_setup() {
            return None;
        }
        let conversation = self.conversation_hit_rect;
        let band = Rect::new(
            self.viewport.x,
            conversation.y,
            self.viewport.w,
            conversation.h,
        );
        koru_ui::scrollbar::ScrollbarGeom::for_content(
            band,
            self.conversation_extent,
            self.conversation_scroll,
        )
    }

    /// The send (and stop) button, in the composer's bottom-right corner,
    /// inset by the composer's own padding like the toolbar beside it.
    fn send_button(&self) -> Rect {
        let composer = self.composer_hit_rect;
        let side = BUTTON_H;
        Rect::new(
            composer.x + composer.w - COMPOSER_PAD_X - side,
            composer.y + composer.h - COMPOSER_PAD_BOTTOM - side,
            side,
            side,
        )
    }

    /// The centered content column used by the settings/setup surface.
    ///
    /// Matches the chat conversation's maximum width so the settings page does
    /// not sprawl on a wide tab or wide pane.
    fn setup_content_rect(&self) -> Rect {
        let w = (self.viewport.w - PAD * 2.0).max(1.0).min(CONTENT_MAX_W);
        let x = self.viewport.x + (self.viewport.w - w) * 0.5;
        Rect::new(x, self.viewport.y, w, self.viewport.h)
    }

    fn settings_button(&self) -> Rect {
        Rect::new(
            self.viewport.x + self.viewport.w - PAD - 76.0,
            self.viewport.y + 7.0,
            76.0,
            BUTTON_H,
        )
    }

    fn history_button(&self) -> Rect {
        Rect::new(self.viewport.x + PAD, self.viewport.y + 7.0, 72.0, BUTTON_H)
    }

    fn new_chat_button(&self) -> Rect {
        Rect::new(
            self.settings_button().x - 92.0,
            self.viewport.y + 7.0,
            84.0,
            BUTTON_H,
        )
    }

    fn provider_cards(&self) -> [Rect; 4] {
        let content = self.setup_content_rect();
        let y = self.viewport.y + 76.0;
        [0, 1, 2, 3].map(|i| Rect::new(content.x, y + i as f32 * 56.0, content.w, 48.0))
    }

    fn field_rects(&self) -> [Rect; 2] {
        let content = self.setup_content_rect();
        let y = self.viewport.y + SETUP_FIELDS_Y;
        [
            Rect::new(content.x, y, content.w, FIELD_H),
            Rect::new(content.x, y + FIELD_H + 10.0, content.w, FIELD_H),
        ]
    }

    fn openai_credential_configured(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.openai.credential_configured)
    }

    fn setup_text_field_at(&self, x: f32, y: f32) -> bool {
        let fields = self.field_rects();
        match self.setup_provider {
            SetupProvider::OpenAi => {
                fields[1].contains(x, y)
                    || ((!self.openai_credential_configured() || self.editing_openai_key)
                        && fields[0].contains(x, y))
            }
            SetupProvider::Ollama => {
                fields[0].contains(x, y)
                    || (fields[1].contains(x, y)
                        && !self
                            .ollama_pull_action_rect()
                            .is_some_and(|rect| rect.contains(x, y)))
            }
            SetupProvider::Koru | SetupProvider::Agents => false,
        }
    }

    fn openai_credential_replace_button(&self) -> Rect {
        let field = self.field_rects()[0];
        Rect::new(field.x + field.w - 104.0, field.y, 62.0, field.h)
    }

    fn openai_credential_clear_button(&self) -> Rect {
        let field = self.field_rects()[0];
        Rect::new(field.x + field.w - 42.0, field.y, 42.0, field.h)
    }

    fn model_options(&self) -> Vec<String> {
        let mut models: Vec<_> = match self.setup_provider {
            SetupProvider::OpenAi => self
                .openai_models
                .iter()
                .filter(|model| likely_openai_text_model(model))
                .cloned()
                .collect(),
            SetupProvider::Ollama => self.ollama_models.clone(),
            SetupProvider::Koru | SetupProvider::Agents => Vec::new(),
        };
        sort_model_options(&mut models);
        models.dedup();
        models
    }

    fn chat_model_options(&self) -> Vec<String> {
        let Some(provider) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.current_model.provider.as_str())
        else {
            return Vec::new();
        };
        let mut models: Vec<_> = match provider {
            AiProviderId::OPENAI => self
                .openai_models
                .iter()
                .filter(|model| likely_openai_text_model(model))
                .cloned()
                .collect(),
            AiProviderId::OLLAMA => self.ollama_models.clone(),
            _ => Vec::new(),
        };
        sort_model_options(&mut models);
        models.dedup();
        models
    }

    fn change_chat_model(&mut self, requested_model: String) -> bool {
        let Some(transcript) = &self.transcript else {
            return false;
        };
        let mut model = transcript.chat.current_model.clone();
        model.requested_model = requested_model;
        if model.provider.as_str() != AiProviderId::OPENAI
            || !(model.requested_model.starts_with("gpt-5")
                || model.requested_model.starts_with("gpt-4.1"))
        {
            self.web_search_armed = false;
        }
        // A reasoning level belongs to a model's capability set. Carrying a
        // level from the previous model can make an otherwise valid switch
        // fail, so a model-only choice returns to the provider default.
        model.default_reasoning = ReasoningChoice::Default;
        self.send_command(Command::UpdateModel(
            transcript.chat.id,
            UpdateChatModelRequest { model },
        ))
    }

    fn chat_model_button(&self) -> Rect {
        let toolbar = self.composer_toolbar_rect();
        // As wide as its label wants, up to half the toolbar; the mode
        // control gives way to its left.
        let width = (self.chip_label_w + 44.0)
            .max(78.0)
            .min((toolbar.w * 0.5).max(78.0))
            .min(toolbar.w);
        Rect::new(toolbar.x + toolbar.w - width, toolbar.y, width, toolbar.h)
    }

    /// Whether the composer has its quiet row below: an agent chat's folder
    /// to show. Before the first message it chooses the folder; afterwards a
    /// chosen folder stays named, and a chat left in the home folder, the
    /// default, shows nothing — and the row takes no room.
    fn shows_composer_footer(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            agents::is_agent_provider(&transcript.chat.current_model.provider)
                && (self.folder_editable() || transcript.chat.agent_working_folder.is_some())
        })
    }

    /// The height the quiet row takes under the composer, its gap included:
    /// nothing when there is no row.
    fn composer_footer_h(&self) -> f32 {
        if self.shows_composer_footer() {
            COMPOSER_FOOTER_GAP + COMPOSER_FOOTER_H
        } else {
            0.0
        }
    }

    /// **The quiet row under the composer box**, as wide as the box and
    /// left-aligned with it. Derived from the box ([`Self::composer_rect`]),
    /// which leaves room for it, so drawing, hit-testing and the hover
    /// cursor all read the same place.
    fn composer_footer_rect(&self) -> Option<Rect> {
        if !self.shows_composer_footer() {
            return None;
        }
        let composer = self.composer_hit_rect;
        Some(Rect::new(
            composer.x,
            composer.y + composer.h + COMPOSER_FOOTER_GAP,
            composer.w,
            COMPOSER_FOOTER_H,
        ))
    }

    /// An agent chat's folder, in the quiet row under the composer — a
    /// folder icon and its name, outside the box like a checkout under a
    /// prompt (it sat in the toolbar until then). Before the first message
    /// it chooses the folder; the agent starts in it, so the choice closes
    /// with that message, and from then on a chosen folder stays named here,
    /// unpressable. A chat left in the home folder, the default, shows
    /// nothing here once it has begun.
    fn folder_button(&self) -> Option<Rect> {
        let row = self.composer_footer_rect()?;
        Some(Rect::new(
            row.x,
            row.y,
            self.folder_button_w.max(60.0).min(row.w),
            row.h,
        ))
    }

    /// Whether the folder can still be chosen: an agent chat with no message.
    fn folder_editable(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            agents::is_agent_provider(&transcript.chat.current_model.provider)
                && transcript.messages.is_empty()
        })
    }

    /// [`folder_label`](Self::folder_label) cut with an ellipsis to fit the
    /// composer's width beside the folder icon, so a long folder name never
    /// runs past the box it sits under.
    fn folder_label_shown(&mut self) -> String {
        let label = self.folder_label();
        let icon = self
            .engine
            .icon_layout(icons::FOLDER, ActionButton::ICON_SIZE)
            .width();
        let room = self.composer_hit_rect.w - ActionButton::PAD_X * 2.0 - icon - ActionButton::GAP;
        self.engine.elided(&label, role::meta(), room)
    }

    fn folder_label(&self) -> String {
        let folder = self
            .transcript
            .as_ref()
            .and_then(|transcript| transcript.chat.agent_working_folder.as_deref());
        match folder {
            Some(folder) => std::path::Path::new(folder)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| folder.to_owned()),
            None => koru_i18n::t(self.lang, koru_i18n::Key::AiAgentFolderHome).to_owned(),
        }
    }

    fn tool_mode_button(&self) -> Rect {
        let toolbar = self.composer_toolbar_rect();
        let model = self.chat_model_button();
        let width = (toolbar.w * 0.24).clamp(72.0, 105.0);
        Rect::new(
            (model.x - 6.0 - width).max(toolbar.x),
            toolbar.y,
            width.min((model.x - toolbar.x - 6.0).max(1.0)),
            toolbar.h,
        )
    }

    fn koru_chat(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            transcript.chat.current_model.provider.as_str() == AiProviderId::KORU
        })
    }

    fn reasoning_toggle_rect(&self) -> Option<Rect> {
        if self.live_reasoning.is_empty() {
            return None;
        }
        let content = self.content_rect();
        let composer = self.composer_hit_rect;
        Some(Rect::new(
            content.x,
            composer.y - COMPOSER_GAP - 22.0,
            content.w.min(220.0),
            20.0,
        ))
    }

    fn draw_reasoning_drawer(&mut self, canvas: &mut Scene, root: Affine, composer: Rect) {
        if self.live_reasoning.is_empty() {
            return;
        }
        let p = theme::p();
        let content = self.content_rect();
        let title = if self.attempt.is_some() && self.draft.is_empty() {
            koru_i18n::t(self.lang, koru_i18n::Key::AiThinking)
        } else {
            koru_i18n::t(self.lang, koru_i18n::Key::AiReasoning)
        };
        let label = self.engine.simple_layout(&title, role::meta());
        let y = composer.y - COMPOSER_GAP - label.height();
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            content.x,
            y,
            Some(if self.reasoning_expanded {
                p.accent
            } else {
                p.text_dim
            }),
        );
        if self.reasoning_expanded {
            let preview: String = self.live_reasoning.chars().take(400).collect();
            let body = self
                .engine
                .simple_layout_wrapped(&preview, role::meta(), content.w);
            scene::draw_layout(
                canvas,
                root,
                &body,
                content.x,
                y - body.height() - 4.0,
                Some(p.text_dim),
            );
        }
    }

    fn web_search_available(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            let model = &transcript.chat.current_model;
            model.provider.as_str() == AiProviderId::OPENAI
                && (model.requested_model.starts_with("gpt-5")
                    || model.requested_model.starts_with("gpt-4.1"))
        })
    }

    /// **The private mark**, the toolbar's first control: whether agents
    /// and connected assistants may read this chat. It sat left of the
    /// title; with the title gone it is the only door to the setting, so it
    /// moved here, where both hosts show it.
    fn privacy_button(&self) -> Option<Rect> {
        self.transcript.as_ref()?;
        let toolbar = self.composer_toolbar_rect();
        Some(Rect::new(toolbar.x, toolbar.y, toolbar.h, toolbar.h))
    }

    fn web_search_button(&self) -> Option<Rect> {
        if !self.web_search_available() {
            return None;
        }
        let toolbar = self.composer_toolbar_rect();
        let x = self
            .privacy_button()
            .map_or(toolbar.x, |privacy| privacy.x + privacy.w + 6.0);
        Some(Rect::new(x, toolbar.y, toolbar.h, toolbar.h))
    }

    /// The composer's toolbar row: inside its padding, left of the send
    /// button.
    fn composer_toolbar_rect(&self) -> Rect {
        let composer = self.composer_hit_rect;
        let send = self.send_button();
        let x = composer.x + COMPOSER_PAD_X;
        Rect::new(
            x,
            composer.y + composer.h - COMPOSER_PAD_BOTTOM - COMPOSER_TOOLBAR_H,
            (send.x - x - 6.0).max(1.0),
            COMPOSER_TOOLBAR_H,
        )
    }

    fn tool_access_button(&self) -> Rect {
        let toolbar = self.composer_toolbar_rect();
        let mode = self.tool_mode_button();
        let x = self
            .web_search_button()
            .or_else(|| self.privacy_button())
            .map_or(toolbar.x, |icon| icon.x + icon.w + 6.0);
        let width = (toolbar.w * 0.28)
            .clamp(78.0, 140.0)
            .min((mode.x - x - 6.0).max(1.0));
        Rect::new(x, toolbar.y, width, toolbar.h)
    }

    /// The effort section at the top of the chip's popover, when the model
    /// advertises a control.
    fn effort_section(&self) -> Option<Rect> {
        self.effort_control()?;
        let panel = self.chat_model_panel()?;
        Some(Rect::new(
            panel.x + 4.0,
            panel.y + 4.0,
            panel.w - 8.0,
            effort::SECTION_H,
        ))
    }

    fn effort_control(&self) -> Option<&ReasoningControl> {
        self.transcript
            .as_ref()
            .and_then(|transcript| transcript.reasoning_control.as_ref())
            .filter(|control| !effort::stops(control).is_empty())
    }

    /// What the chat's effort setting means under its model's control.
    fn effective_reasoning(&self) -> Option<ReasoningChoice> {
        let transcript = self.transcript.as_ref()?;
        let control = transcript.reasoning_control.as_ref()?;
        effort::resolve(control, transcript.chat.current_model.default_reasoning)
    }

    /// Which slider stop the pointer is on, for hover redraws.
    fn effort_stop_index_at(&self, x: f32, y: f32) -> Option<usize> {
        let section = self.effort_section()?;
        let control = self.effort_control()?;
        let slider = effort::slider(section, control);
        let choice = effort::stop_at(&slider, x, y)?;
        slider.stops.iter().position(|(_, stop)| *stop == choice)
    }

    /// The stop the slider shows: the press in flight, else the setting.
    fn effort_shown(&self) -> Option<ReasoningChoice> {
        self.effort_drag.or_else(|| self.effective_reasoning())
    }

    fn effort_section_h(&self) -> f32 {
        if self.effort_control().is_some() {
            effort::SECTION_H + 4.0
        } else {
            0.0
        }
    }

    /// Where the picker's rows start inside the panel: under the effort
    /// section when there is one.
    fn chat_model_rows_top(&self, panel: Rect) -> f32 {
        panel.y + 4.0 + self.effort_section_h()
    }

    /// Set the chat's effort: the persisted default the next turn uses.
    fn choose_effort(&mut self, choice: ReasoningChoice) -> bool {
        let Some(transcript) = &self.transcript else {
            return false;
        };
        if self.effective_reasoning() == Some(choice) {
            return true;
        }
        let mut model = transcript.chat.current_model.clone();
        model.default_reasoning = choice;
        self.send_command(Command::UpdateModel(
            transcript.chat.id,
            UpdateChatModelRequest { model },
        ))
    }

    /// The chip's text: the model, then its effort when the model has one.
    pub(crate) fn chip_label(&self) -> String {
        let Some(transcript) = &self.transcript else {
            return "Choose model".into();
        };
        let model = &transcript.chat.current_model;
        let name = if model.provider.as_str() == AiProviderId::KORU {
            AiProviderId::KORU_HOSTED_MODEL_LABEL.to_owned()
        } else if agents::is_agent_provider(&model.provider) {
            let agents = self
                .settings
                .as_ref()
                .map(|settings| settings.agents.as_slice())
                .unwrap_or_default();
            agent_chip_label(
                agents,
                &model.provider,
                &model.requested_model,
                self.listed_agent_models(&model.provider),
            )
        } else {
            model.requested_model.clone()
        };
        match self.effective_reasoning() {
            Some(choice) => format!("{name} · {}", effort::word(self.lang, choice)),
            None => name,
        }
    }

    fn chat_model_panel(&self) -> Option<Rect> {
        let count = self.chat_model_rows().len().min(MODEL_VISIBLE_ROWS);
        let effort = self.effort_control().is_some();
        if !self.chat_model_open || (count == 0 && !effort) {
            return None;
        }
        let button = self.chat_model_button();
        let height = count as f32 * 30.0 + 8.0 + self.effort_section_h();
        let width = self.composer_hit_rect.w.min(320.0).max(button.w);
        let x = (button.x + button.w - width)
            .max(self.viewport.x + 4.0)
            .min(self.viewport.x + self.viewport.w - width - 4.0);
        Some(Rect::new(
            x,
            self.composer_hit_rect.y - height - 4.0,
            width,
            height,
        ))
    }

    fn chat_model_scrollbar(&self) -> Option<koru_ui::scrollbar::ScrollbarGeom> {
        let panel = self.chat_model_panel()?;
        let shown = self.chat_model_rows().len().min(MODEL_VISIBLE_ROWS);
        let area = Rect::new(
            panel.x + 4.0,
            self.chat_model_rows_top(panel),
            panel.w - 8.0,
            shown as f32 * 30.0,
        );
        koru_ui::scrollbar::ScrollbarGeom::for_content(
            area,
            self.chat_model_rows().len() as f32 * 30.0,
            self.chat_model_offset as f32 * 30.0,
        )
    }

    fn chat_model_option_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = self.chat_model_panel()?;
        if !panel.contains(x, y) {
            return None;
        }
        let top = self.chat_model_rows_top(panel);
        if y < top {
            return None;
        }
        let visible = ((y - top) / 30.0).floor().max(0.0) as usize;
        let index = self.chat_model_offset + visible;
        let row = Rect::new(
            panel.x + 4.0,
            top + visible as f32 * 30.0,
            panel.w - 8.0,
            30.0,
        );
        (visible < MODEL_VISIBLE_ROWS && index < self.chat_model_rows().len() && row.contains(x, y))
            .then_some(index)
    }

    fn tool_access_label(&self) -> String {
        let Some(transcript) = &self.transcript else {
            return "Workspace access".into();
        };
        let count = transcript.tool_policy.workspace_ids.len();
        format!("{count} {}", if count == 1 { "space" } else { "spaces" })
    }

    fn tool_mode_label(&self) -> &'static str {
        let Some(transcript) = &self.transcript else {
            return "Manual";
        };
        if self
            .session_auto_approve_chats
            .contains(&transcript.chat.id)
        {
            "Auto session"
        } else if transcript.tool_policy.mode == ToolMode::Auto {
            "Automatic"
        } else {
            "Manual"
        }
    }

    fn model_options_panel(&self) -> Option<Rect> {
        let count = self.model_option_count().min(MODEL_VISIBLE_ROWS);
        if !self.model_options_open || count == 0 {
            return None;
        }
        let field = self.field_rects()[1];
        Some(Rect::new(
            field.x,
            field.y + field.h + 4.0,
            field.w,
            count as f32 * 30.0 + 8.0,
        ))
    }

    fn model_option_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = self.model_options_panel()?;
        if !panel.contains(x, y) {
            return None;
        }
        let visible = ((y - panel.y - 4.0) / 30.0).floor().max(0.0) as usize;
        let index = self.model_option_offset + visible;
        let row = Rect::new(
            panel.x + 4.0,
            panel.y + 4.0 + visible as f32 * 30.0,
            panel.w - 8.0,
            30.0,
        );
        (visible < MODEL_VISIBLE_ROWS && index < self.model_option_count() && row.contains(x, y))
            .then_some(index)
    }

    fn model_option_count(&self) -> usize {
        match self.setup_provider {
            SetupProvider::OpenAi => self
                .openai_models
                .iter()
                .filter(|model| likely_openai_text_model(model))
                .count(),
            SetupProvider::Ollama => self.ollama_models.len(),
            SetupProvider::Koru | SetupProvider::Agents => 0,
        }
    }

    fn validation_retryable(&self) -> bool {
        self.validation.as_ref().is_some_and(|validation| {
            self.setup_provider_id().as_ref() == Some(&validation.provider)
                && (matches!(
                    validation.status,
                    ProviderValidationStatus::Unavailable | ProviderValidationStatus::Failed
                ) || (self.setup_provider == SetupProvider::Ollama
                    && validation.status == ProviderValidationStatus::NoModels)
                    || validation
                        .failure
                        .as_ref()
                        .is_some_and(|failure| failure.retryable))
        })
    }

    fn setup_save_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x + content.w - 108.0,
            self.viewport.y
                + SETUP_FIELDS_Y
                + 134.0
                + self.ollama_catalog_gap()
                + self.agents_gap(),
            108.0,
            BUTTON_H,
        )
    }

    fn ollama_pull_action_rect(&self) -> Option<Rect> {
        if self.setup_provider != SetupProvider::Ollama {
            return None;
        }
        let model = self.ollama_model.value();
        if model.trim().is_empty() || self.ollama_models.iter().any(|known| known == &model) {
            return None;
        }
        let field = self.field_rects()[1];
        Some(Rect::new(field.x + field.w - 66.0, field.y, 66.0, field.h))
    }

    fn reasoning_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x,
            self.viewport.y + SETUP_FIELDS_Y + 86.0 + self.ollama_catalog_gap(),
            content.w,
            BUTTON_H,
        )
    }

    fn ollama_catalog_gap(&self) -> f32 {
        if self.setup_provider == SetupProvider::Ollama && self.ollama_models.is_empty() {
            26.0
        } else {
            0.0
        }
    }

    fn setup_close_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x + content.w - 70.0,
            self.viewport.y + 10.0,
            70.0,
            BUTTON_H,
        )
    }

    fn clear_history_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x,
            self.viewport.y + self.viewport.h - PAD - BUTTON_H,
            118.0,
            BUTTON_H,
        )
    }

    fn clear_usage_button(&self) -> Rect {
        Rect::new(
            self.clear_history_button().x + self.clear_history_button().w + 8.0,
            self.viewport.y + self.viewport.h - PAD - BUTTON_H,
            118.0,
            BUTTON_H,
        )
    }

    fn system_prompt_rect(&self) -> Option<Rect> {
        if !self.debug_prompt_enabled {
            return None;
        }
        let y = self.viewport.y + SETUP_FIELDS_Y + 242.0 + self.agents_gap();
        let bottom = if self.transcript.is_some() {
            self.clear_history_button().y - 10.0
        } else {
            self.viewport.y + self.viewport.h - PAD
        };
        let height = (bottom - y).min(110.0);
        (height >= 52.0).then(|| {
            let content = self.setup_content_rect();
            Rect::new(content.x, y, content.w, height)
        })
    }

    /// Stable identity for ordinary controls whose hover state is drawn from
    /// `pointer`. Repaint only when the pointer crosses between controls, not
    /// for every pixel of movement across the surface.
    fn control_hover_at(&self, x: f32, y: f32) -> Option<(u8, usize)> {
        if self.showing_setup() {
            if self.setup_open && self.transcript.is_some() {
                if self.setup_close_button().contains(x, y) {
                    return Some((10, 0));
                }
            }
            if let Some(index) = self
                .provider_cards()
                .iter()
                .position(|rect| rect.contains(x, y))
            {
                return Some((1, index));
            }
            if self.setup_provider == SetupProvider::Agents
                && let Some(index) = self.agent_row_at(x, y)
            {
                return Some((15, index));
            }
            if self.setup_save_button().contains(x, y) {
                return Some((2, 0));
            }
            if !self.reasoning_choices().is_empty() && self.reasoning_button().contains(x, y) {
                return Some((2, 1));
            }
            if self.transcript.is_some() {
                if self.clear_history_button().contains(x, y) {
                    return Some((2, 3));
                }
                if self.clear_usage_button().contains(x, y) {
                    return Some((2, 4));
                }
            }
            if self.openai_credential_configured() && !self.editing_openai_key {
                if self.openai_credential_replace_button().contains(x, y) {
                    return Some((2, 5));
                }
                if self.openai_credential_clear_button().contains(x, y) {
                    return Some((2, 6));
                }
            }
            if self
                .ollama_pull_action_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                return Some((3, 0));
            }
            if self.model_options_open
                && let Some(index) = self.model_option_at(x, y)
            {
                return Some((4, index));
            }
            None
        } else {
            if self
                .privacy_button()
                .is_some_and(|button| button.contains(x, y))
            {
                return Some((5, 8));
            }
            if self.chat_model_button().contains(x, y) {
                return Some((5, 5));
            }
            if self.folder_editable()
                && self
                    .folder_button()
                    .is_some_and(|button| button.contains(x, y))
            {
                return Some((5, 12));
            }
            if let Some(index) = self.effort_stop_index_at(x, y) {
                return Some((16, index));
            }
            if self
                .web_search_button()
                .is_some_and(|button| button.contains(x, y))
            {
                return Some((5, 7));
            }
            if self
                .reasoning_toggle_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                return Some((5, 11));
            }
            if self.tool_access_button().contains(x, y) {
                return Some((5, 6));
            }
            if self.tool_mode_button().contains(x, y) {
                return Some((5, 9));
            }
            if self.tool_mode_open {
                if let Some(modes) = self.tool_mode_buttons() {
                    for (index, rect) in modes.into_iter().enumerate() {
                        if rect.contains(x, y) {
                            return Some((13, index));
                        }
                    }
                }
                if self
                    .tool_mode_session_button()
                    .is_some_and(|rect| rect.contains(x, y))
                {
                    return Some((13, 2));
                }
            }
            if self.tool_access_open {
                if let Some(workspace) = self.tool_access_workspace_at(x, y)
                    && let Some(index) = self
                        .destinations
                        .iter()
                        .position(|candidate| candidate.workspace_id == workspace)
                {
                    return Some((14, index));
                }
            }
            if self.chat_model_open
                && let Some(index) = self.chat_model_option_at(x, y)
            {
                return Some((9, index));
            }
            let header = self.hosts_header_buttons();
            for (index, rect) in [
                self.history_button(),
                self.new_chat_button(),
                self.settings_button(),
                self.send_button(),
            ]
            .into_iter()
            .enumerate()
            {
                if (header || index == 3) && rect.contains(x, y) {
                    return Some((5, index + 1));
                }
            }
            if self.destination_open.is_some()
                && let Some(index) = self.destination_at(x, y)
            {
                return Some((8, index));
            }
            None
        }
    }

    fn draw_chat(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        // **No title row.** A chat's name is its tab's in a workspace and
        // its row's in the Chats list; the surface itself starts with the
        // conversation. The standalone window keeps its header buttons.
        if self.hosts_header_buttons() {
            let history = self.history_button();
            draw_surface_button(
                canvas,
                root,
                &mut self.engine,
                history,
                "Chats",
                ButtonRole::Secondary,
                history.contains(self.pointer.0, self.pointer.1),
                self.history_open,
            );
            for (rect, label) in [
                (self.new_chat_button(), "New chat"),
                (self.settings_button(), "Settings"),
            ] {
                draw_surface_button(
                    canvas,
                    root,
                    &mut self.engine,
                    rect,
                    label,
                    ButtonRole::Secondary,
                    rect.contains(self.pointer.0, self.pointer.1),
                    false,
                );
            }
        }
        self.draw_messages(canvas, root);
        let composer = self.composer_rect();
        self.draw_greeting(canvas, root);
        let pill_band = self.pill_band_rect();
        let context_document = self.context_document_pill();
        self.pill_strip.draw(
            canvas,
            root,
            &mut self.engine,
            context_document.as_ref(),
            &self.pending_quotes,
            pill_band,
            self.viewport,
            Instant::now(),
        );
        if let Some(line) = self.footer_line().map(str::to_owned) {
            let content = self.content_rect();
            let layout = self
                .engine
                .simple_layout_wrapped(&line, role::meta(), content.w);
            let context_document = self.context_document_pill();
            scene::draw_layout(
                canvas,
                root,
                &layout,
                content.x,
                composer.y
                    - COMPOSER_GAP
                    - pill_band_height(context_document.as_ref(), &self.pending_quotes)
                    - layout.height(),
                Some(p.text_dim),
            );
        }
        // **The composer is one component** in the inspector, every primitive
        // it paints inside it: the box, the text, the toolbar's controls and
        // the folder row under the box — so a review sees `KoruComposer` and
        // its parts rather than loose fills and strokes.
        let footer_row = self.composer_footer_rect();
        let whole = match footer_row {
            Some(row) => Rect::new(
                composer.x,
                composer.y,
                composer.w,
                row.y + row.h - composer.y,
            ),
            None => composer,
        };
        let _composer = koru_draw::inspect::scope(
            "KoruComposer",
            "component",
            root,
            scene::rect(whole.x, whole.y, whole.w, whole.h),
            |r| {
                r.property("radius", 8.0, "box corners");
                r.property("pad x", COMPOSER_PAD_X, "text and toolbar inset");
                r.property("pad top", COMPOSER_PAD_TOP, "above the text");
                r.property("pad bottom", COMPOSER_PAD_BOTTOM, "below the toolbar");
            },
        );
        {
            let _box = koru_draw::inspect::scope(
                "ComposerBox",
                "part",
                root,
                scene::rect(composer.x, composer.y, composer.w, composer.h),
                |_| {},
            );
            scene::fill(
                canvas,
                root,
                p.panel_bg,
                &scene::rrect(composer.x, composer.y, composer.w, composer.h, 8.0),
            );
            scene::stroke(
                canvas,
                root,
                if self.focus == Focus::Composer {
                    p.focus_ring
                } else {
                    p.panel_border
                },
                theme::BORDER_W,
                &scene::rrect(composer.x, composer.y, composer.w, composer.h, 8.0),
            );
        }
        {
            let text = self.composer_text_rect();
            let _text = koru_draw::inspect::scope(
                "ComposerText",
                "part",
                root,
                scene::rect(text.x, text.y, text.w, text.h),
                |r| r.property("editor", "koru-editor", "a private document"),
            );
            let focused = self.focus == Focus::Composer;
            self.composer.set_focused(focused);
            self.composer.draw(
                canvas,
                root,
                focused,
                self.pointer,
                koru_i18n::t(self.lang, koru_i18n::Key::AiComposerPlaceholder),
            );
        }
        {
            let toolbar = Rect::new(
                composer.x,
                composer.y + composer.h - COMPOSER_PAD_BOTTOM - COMPOSER_TOOLBAR_H,
                composer.w,
                COMPOSER_TOOLBAR_H,
            );
            let _toolbar = koru_draw::inspect::scope(
                "ComposerToolbar",
                "part",
                root,
                scene::rect(toolbar.x, toolbar.y, toolbar.w, toolbar.h),
                |_| {},
            );
            let send = self.send_button();
            let send_enabled = self.attempt.is_some()
                || !self.composer.is_blank()
                || !self.pending_quotes.is_empty();
            draw_composer_action(
                canvas,
                root,
                &mut self.engine,
                send,
                self.attempt.is_some(),
                send_enabled,
                send.contains(self.pointer.0, self.pointer.1),
            );
            let chip_label = self.chip_label();
            self.chip_label_w = self.engine.simple_layout(&chip_label, role::row()).width();
            let access_button = self.tool_access_button();
            let access_label = self.tool_access_label();
            ActionButton::new(&access_label, ActionStyle::Ghost, Some(icons::CARET_DOWN)).draw(
                canvas,
                root,
                &mut self.engine,
                access_button,
                access_button.contains(self.pointer.0, self.pointer.1),
            );
            let mode_button = self.tool_mode_button();
            ActionButton::new(
                self.tool_mode_label(),
                ActionStyle::Ghost,
                Some(icons::CARET_DOWN),
            )
            .draw(
                canvas,
                root,
                &mut self.engine,
                mode_button,
                mode_button.contains(self.pointer.0, self.pointer.1),
            );
            if let Some(privacy) = self.privacy_button() {
                let private = self
                    .transcript
                    .as_ref()
                    .is_some_and(|transcript| transcript.chat.private);
                IconButton::new(icons::DETECTIVE, privacy)
                    .with_state(IconButtonState {
                        hovered: privacy.contains(self.pointer.0, self.pointer.1),
                        active: private,
                        disabled: false,
                    })
                    .draw(canvas, root, &mut self.engine);
            }
            if let Some(search) = self.web_search_button() {
                IconButton::new(icons::GLOBE, search)
                    .with_state(IconButtonState {
                        hovered: search.contains(self.pointer.0, self.pointer.1),
                        active: self.web_search_armed,
                        disabled: false,
                    })
                    .draw(canvas, root, &mut self.engine);
            }
            self.draw_reasoning_drawer(canvas, root, composer);
            let model_button = self.chat_model_button();
            let koru_chat = self.koru_chat();
            let model_label = self.chip_label();
            let model_caret =
                (!koru_chat || self.effort_control().is_some()).then_some(icons::CARET_DOWN);
            ActionButton::new(&model_label, ActionStyle::Ghost, model_caret).draw(
                canvas,
                root,
                &mut self.engine,
                model_button,
                model_button.contains(self.pointer.0, self.pointer.1),
            );
        }
        if let Some(footer_row) = footer_row {
            let _row = koru_draw::inspect::scope(
                "ComposerFolderRow",
                "part",
                root,
                scene::rect(footer_row.x, footer_row.y, footer_row.w, footer_row.h),
                |_| {},
            );
            if let Some(folder) = self.folder_button() {
                // Quiet, under the box: the icon and the name in the meta face,
                // dim; a hover tint while the folder can still be chosen.
                let hovered =
                    self.folder_editable() && folder.contains(self.pointer.0, self.pointer.1);
                if hovered {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(folder.x, folder.y, folder.w, folder.h, 4.0),
                    );
                }
                let colour = if hovered { p.text } else { p.text_dim };
                let icon = self
                    .engine
                    .icon_layout(icons::FOLDER, ActionButton::ICON_SIZE);
                let shown = self.folder_label_shown();
                let label = self.engine.simple_layout(&shown, role::meta());
                let x = folder.x + ActionButton::PAD_X;
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &icon,
                    x,
                    folder.y + (folder.h - icon.height()) * 0.5,
                    Some(colour),
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &label,
                    x + icon.width() + ActionButton::GAP,
                    folder.y + (folder.h - label.height()) * 0.5,
                    Some(colour),
                );
            }
        }
        drop(_composer);
        if self.history_open {
            self.draw_chats_pane(canvas, root);
        }
        self.draw_chat_model_picker(canvas, root);
        self.draw_tool_mode_picker(canvas, root);
        self.draw_tool_access_picker(canvas, root);
        self.web_search_tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
        self.composer.draw_panels(canvas, root);
    }

    /// An empty chat's greeting: one line at the title size, centred over
    /// the composer — at the foot of the (empty) conversation, so it sits
    /// above the pills and the status line like the messages would.
    fn draw_greeting(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(key) = self.greeting() else {
            return;
        };
        let area = self.conversation_rect();
        let mut layout =
            self.engine
                .simple_layout_wrapped(koru_i18n::t(self.lang, key), role::title(), area.w);
        layout.align(
            parley::Alignment::Center,
            parley::AlignmentOptions::default(),
        );
        let y = (area.y + area.h - GREETING_GAP - layout.height()).max(area.y);
        scene::draw_layout(canvas, root, &layout, area.x, y, Some(theme::p().text));
    }

    fn draw_chat_model_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(panel) = self.chat_model_panel() else {
            return;
        };
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(6.0),
        );
        let p = theme::p();
        let rows_top = self.chat_model_rows_top(panel);
        if let (Some(section), Some(control), Some(shown)) = (
            self.effort_section(),
            self.effort_control().cloned(),
            self.effort_shown(),
        ) {
            effort::draw(
                canvas,
                root,
                &mut self.engine,
                self.lang,
                section,
                &control,
                shown,
                self.pointer,
            );
        }
        let (current_provider, current_model) = self
            .transcript
            .as_ref()
            .map(|transcript| {
                (
                    Some(transcript.chat.current_model.provider.clone()),
                    transcript.chat.current_model.requested_model.clone(),
                )
            })
            .unwrap_or((None, String::new()));
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.clone())
            .unwrap_or_default();
        for (index, picker_row) in self
            .chat_model_rows()
            .into_iter()
            .enumerate()
            .skip(self.chat_model_offset)
            .take(MODEL_VISIBLE_ROWS)
        {
            let visible = index.saturating_sub(self.chat_model_offset);
            let row = Rect::new(
                panel.x + 4.0,
                rows_top + visible as f32 * 30.0,
                panel.w - 8.0,
                30.0,
            );
            let view = self.picker_row_view(
                &picker_row,
                &agents,
                current_provider.as_ref(),
                &current_model,
            );
            let is_current = view.current;
            let selectable = picker_row.selectable(&self.agent_status);
            if (selectable && row.contains(self.pointer.0, self.pointer.1)) || is_current {
                scene::fill(
                    canvas,
                    root,
                    if is_current {
                        p.row_selected
                    } else {
                        p.hover_tint
                    },
                    &scene::rrect(row.x, row.y, row.w, row.h, 4.0),
                );
            }
            let line = self.engine.simple_layout(
                &view.label,
                if matches!(picker_row, PickerRow::Header) {
                    role::meta()
                } else {
                    role::row()
                },
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &line,
                row.x + 8.0 + view.level as f32 * 14.0,
                row.y + (row.h - line.height()) * 0.5,
                Some(if view.dim { p.text_dim } else { p.text }),
            );
            let mut right = row.x + row.w - 8.0;
            match view.trailing {
                agents::Trailing::None => {}
                agents::Trailing::Word(word) => {
                    let word = self.engine.simple_layout(&word, role::meta());
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &word,
                        right - word.width(),
                        row.y + (row.h - word.height()) * 0.5,
                        Some(p.text_dim),
                    );
                }
                agents::Trailing::Caret { expanded } => {
                    let caret = self.engine.icon_layout(
                        if expanded {
                            icons::CARET_DOWN
                        } else {
                            icons::CARET_RIGHT
                        },
                        12.0,
                    );
                    scene::draw_layout(
                        canvas,
                        root,
                        &caret,
                        right - caret.width(),
                        row.y + (row.h - caret.height()) * 0.5,
                        Some(p.text_dim),
                    );
                    right -= caret.width() + 8.0;
                }
            }
            if is_current {
                let check = self.engine.icon_layout(icons::CHECK, 13.0);
                scene::draw_layout(
                    canvas,
                    root,
                    &check,
                    right - check.width(),
                    row.y + (row.h - check.height()) * 0.5,
                    Some(p.text_dim),
                );
            }
        }
        if let Some(scrollbar) = self.chat_model_scrollbar() {
            let active = self.chat_model_scroll_drag.is_some()
                || scrollbar.in_gutter(self.pointer.0, self.pointer.1);
            koru_ui::scrollbar::draw(canvas, root, scrollbar, active);
        }
    }

    fn rebuild_messages(&mut self, width: f32) {
        let started = Instant::now();
        self.tool_activity_layouts.clear();
        self.tool_group_layouts.clear();
        self.request_document_titles();
        let untitled = koru_i18n::t(self.lang, koru_i18n::Key::AiDocumentLinkUntitled);
        let mut inputs = Vec::new();
        if let Some(transcript) = &self.transcript {
            let usage_by_message = transcript
                .usage
                .iter()
                .map(|entry| (entry.message_id, &entry.usage))
                .collect::<HashMap<_, _>>();
            for message in &transcript.messages {
                if !message.markdown.is_empty() || message.state != MessageState::Pending {
                    let wrap_width = message_wrap_width(message.role, width);
                    let usage_text = (message.role == ChatRole::Assistant
                        && message.state == MessageState::Complete)
                        .then(|| usage_by_message.get(&message.id))
                        .flatten()
                        .and_then(|usage| usage_line(usage, self.lang));
                    let display = if message.markdown.is_empty()
                        && message.state == MessageState::Failed
                    {
                        failure_label(message.error_class, self.lang)
                    } else if message.markdown.is_empty()
                        && message.state == MessageState::Interrupted
                    {
                        koru_i18n::t(self.lang, koru_i18n::Key::AiResponseInterrupted).to_owned()
                    } else {
                        message.markdown.clone()
                    };
                    // The person's own message carries its chips as
                    // `[[koru://doc/…]]`; they read as titled links.
                    let (display, links) = if message.role == ChatRole::User {
                        doc_links::titled(&display, &self.document_titles, untitled)
                    } else {
                        (display, Vec::new())
                    };
                    inputs.push(MessageInput {
                        id: message.id,
                        role: message.role,
                        state: message.state,
                        markdown: message.markdown.clone(),
                        display,
                        links,
                        wrap_width,
                        usage_text,
                        sources: transcript
                            .sources
                            .iter()
                            .find(|entry| entry.message_id == message.id)
                            .map(|entry| entry.sources.clone())
                            .unwrap_or_default(),
                        blocks: transcript
                            .blocks
                            .iter()
                            .filter(|block| block.message_id == message.id)
                            .map(|block| (block.id, block.markdown.clone()))
                            .collect(),
                        rating: transcript
                            .feedback
                            .iter()
                            .rev()
                            .find(|event| event.message_id == Some(message.id))
                            .map(|event| event.value),
                    });
                }
            }
            let now_ms = koru_ai_core::Timestamp::now().millis();
            for activity in &transcript.tool_activities {
                let (title, detail) = if is_agent_row(activity) {
                    (
                        agent_row_title(activity, self.lang),
                        agent_row_body(activity, self.lang, now_ms),
                    )
                } else {
                    (
                        tool_activity_title(activity),
                        tool_activity_detail(activity),
                    )
                };
                let detail = self.engine.simple_layout_wrapped(
                    &detail,
                    role::meta(),
                    (width - 28.0).max(1.0),
                );
                let actions_height = if activity.status == ToolActivityStatus::ApprovalRequired
                    && activity.approval == ToolApproval::Pending
                {
                    let choices = agents::offered_choice_count(activity);
                    if choices > 0 {
                        choices as f32 * (BUTTON_H + 6.0) + 6.0
                    } else {
                        BUTTON_H + 12.0
                    }
                } else {
                    0.0
                };
                let expanded_height =
                    (43.0 + detail.height() + actions_height).max(TOOL_ACTIVITY_MIN_EXPANDED_H);
                self.tool_activity_layouts.push(ToolActivityLayout {
                    activity: activity.clone(),
                    before_message: Some(activity.assistant_message_id),
                    top: 0.0,
                    title: self.engine.simple_layout_wrapped(
                        &title,
                        role::row(),
                        (width - 46.0).max(1.0),
                    ),
                    detail,
                    expanded_height,
                    hidden: self
                        .collapsed_tool_groups
                        .contains(&activity.assistant_message_id),
                });
            }
            // One box per turn, in the order the turns happened.
            let mut turns: Vec<MessageId> = Vec::new();
            for activity in &transcript.tool_activities {
                if !turns.contains(&activity.assistant_message_id) {
                    turns.push(activity.assistant_message_id);
                }
            }
            for message in turns {
                let rows: Vec<&ToolActivity> = transcript
                    .tool_activities
                    .iter()
                    .filter(|activity| activity.assistant_message_id == message)
                    .collect();
                let title = tool_groups::summary(&rows, self.lang);
                self.tool_group_layouts.push(ToolGroupLayout {
                    message,
                    top: 0.0,
                    title: self.engine.simple_layout_wrapped(
                        &title,
                        role::row(),
                        (width - 46.0).max(1.0),
                    ),
                });
            }
        }
        let old = std::mem::take(&mut self.message_layouts);
        let (mut messages, stats) =
            layout_cache::reconcile(&mut self.engine, old, inputs, render_chat_block);
        self.last_rebuild_stats = stats;
        if let Some(report) = &self.usage_report {
            let markdown = format_usage_report(report);
            messages.push(MessageLayout {
                id: None,
                top: 0.0,
                role: ChatRole::Assistant,
                state: MessageState::Complete,
                display: markdown.clone(),
                links: Vec::new(),
                wrap_width: (width - PAD * 2.0).max(1.0),
                layout: self.engine.simple_layout_wrapped(
                    &markdown,
                    role::ui(),
                    (width - PAD * 2.0).max(1.0),
                ),
                usage_text: None,
                usage: None,
                sources: None,
                markdown,
                blocks: Vec::new(),
                rating: None,
            });
        }
        if let Some(markdown) = &self.pending_user {
            let user_width = message_wrap_width(ChatRole::User, width);
            let (pending_display, pending_links) =
                doc_links::titled(markdown, &self.document_titles, untitled);
            messages.push(MessageLayout {
                id: None,
                top: 0.0,
                role: ChatRole::User,
                state: MessageState::Complete,
                markdown: markdown.clone(),
                // The just-sent card reads exactly as the durable one will.
                layout: layout_cache::display_layout(
                    &mut self.engine,
                    &pending_display,
                    &pending_links,
                    user_width,
                ),
                display: pending_display,
                links: pending_links,
                wrap_width: user_width,
                usage_text: None,
                usage: None,
                sources: None,
                blocks: Vec::new(),
                rating: None,
            });
            let assistant_width = message_wrap_width(ChatRole::Assistant, width);
            let usage_text = (!self.draft.is_empty())
                .then_some(self.live_usage.as_ref())
                .flatten()
                .and_then(|usage| usage_line(usage, self.lang));
            messages.push(MessageLayout {
                id: self.pending_assistant_message_id,
                top: 0.0,
                role: ChatRole::Assistant,
                state: MessageState::Pending,
                markdown: self.draft.clone(),
                links: Vec::new(),
                display: if self.draft.is_empty() && !self.open_searches.is_empty() {
                    "Searching the web…".to_owned()
                } else {
                    self.draft.clone()
                },
                wrap_width: assistant_width,
                layout: self.engine.simple_layout_wrapped(
                    if self.draft.is_empty() && !self.open_searches.is_empty() {
                        "Searching the web…"
                    } else {
                        &self.draft
                    },
                    role::ui(),
                    assistant_width,
                ),
                usage: usage_text.as_ref().map(|line| {
                    self.engine
                        .simple_layout_wrapped(line, role::hint(), assistant_width)
                }),
                usage_text,
                sources: sources::layout_sources(
                    &mut self.engine,
                    &self.live_sources,
                    assistant_width,
                ),
                blocks: Vec::new(),
                rating: None,
            });
        }
        self.message_layouts = messages;
        self.position_conversation_items();
        self.message_width = width;
        self.messages_dirty = false;
        self.response_selection = None;
        let image_urls = self
            .message_layouts
            .iter()
            .flat_map(|message| message.blocks.iter())
            .filter_map(|block| block.image.as_ref().map(|image| image.url.clone()))
            .collect::<Vec<_>>();
        for url in image_urls {
            self.ensure_remote_image(&url);
        }
        tracing::debug!(
            target: "koru_ai::ui",
            surface = match self.mode {
                SurfaceMode::Standalone => "standalone",
                SurfaceMode::ContextPane => "context_pane",
            },
            message_count = self.message_layouts.len(),
            block_count = self
                .message_layouts
                .iter()
                .map(|message| message.blocks.len())
                .sum::<usize>(),
            messages_reused = self.last_rebuild_stats.messages_reused,
            messages_laid_out = self.last_rebuild_stats.messages_laid_out,
            blocks_reused = self.last_rebuild_stats.blocks_reused,
            blocks_laid_out = self.last_rebuild_stats.blocks_laid_out,
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "AI Markdown layout rebuilt"
        );
    }

    /// Place durable activity at the turn where it happened. An inference
    /// attempt creates its assistant message before proposing tools, so the
    /// matching activity belongs immediately before that assistant response,
    /// not in a second list at the end of the transcript.
    fn position_conversation_items(&mut self) -> f32 {
        let mut top = 0.0;
        let mut positioned_tools = vec![false; self.tool_activity_layouts.len()];
        for message_index in 0..self.message_layouts.len() {
            let message_id = self.message_layouts[message_index].id;
            if let Some(message_id) = message_id {
                if let Some(group) = self
                    .tool_group_layouts
                    .iter_mut()
                    .find(|group| group.message == message_id)
                {
                    group.top = top;
                    top += tool_groups::GROUP_H + 6.0;
                }
                for (tool_index, tool) in self.tool_activity_layouts.iter_mut().enumerate() {
                    if tool.before_message == Some(message_id) {
                        if tool.hidden {
                            positioned_tools[tool_index] = true;
                            continue;
                        }
                        tool.top = top;
                        top += if self.expanded_tool_activity == Some(tool.activity.id) {
                            tool.expanded_height
                        } else {
                            TOOL_ACTIVITY_ROW_H
                        } + 6.0;
                        positioned_tools[tool_index] = true;
                    }
                }
            }
            self.message_layouts[message_index].top = top;
            top += message_height(&self.message_layouts[message_index]) + CARD_GAP;
        }
        // Rows whose turn is not (yet) a message in the list — an attempt's
        // first proposal before its assistant card lands — still sit under
        // their box, after the messages.
        let placed: Vec<MessageId> = self
            .message_layouts
            .iter()
            .filter_map(|message| message.id)
            .collect();
        for group_index in 0..self.tool_group_layouts.len() {
            let message = self.tool_group_layouts[group_index].message;
            if placed.contains(&message) {
                continue;
            }
            self.tool_group_layouts[group_index].top = top;
            top += tool_groups::GROUP_H + 6.0;
            for (tool_index, tool) in self.tool_activity_layouts.iter_mut().enumerate() {
                if positioned_tools[tool_index] || tool.before_message != Some(message) {
                    continue;
                }
                positioned_tools[tool_index] = true;
                if tool.hidden {
                    continue;
                }
                tool.top = top;
                top += if self.expanded_tool_activity == Some(tool.activity.id) {
                    tool.expanded_height
                } else {
                    TOOL_ACTIVITY_ROW_H
                } + 6.0;
            }
        }
        for (tool_index, tool) in self.tool_activity_layouts.iter_mut().enumerate() {
            if positioned_tools[tool_index] || tool.hidden {
                continue;
            }
            tool.top = top;
            top += if self.expanded_tool_activity == Some(tool.activity.id) {
                tool.expanded_height
            } else {
                TOOL_ACTIVITY_ROW_H
            } + 6.0;
        }
        top
    }

    fn conversation_content_extent(&self) -> f32 {
        let messages = self
            .message_layouts
            .iter()
            .map(|message| message.top + message_height(message) + CARD_GAP);
        let tools = self
            .tool_activity_layouts
            .iter()
            .filter(|tool| !tool.hidden)
            .map(|tool| tool.top + self.tool_activity_height(tool.activity.id) + 6.0);
        let groups = self
            .tool_group_layouts
            .iter()
            .map(|group| group.top + tool_groups::GROUP_H + 6.0);
        messages.chain(tools).chain(groups).fold(0.0, f32::max)
    }

    fn draw_messages(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.conversation_rect();
        if self.messages_dirty || (self.message_width - area.w).abs() > 0.5 {
            self.rebuild_messages(area.w);
            let content_extent = self.conversation_content_extent();
            let mut anchored_question = false;
            if self.scroll_to_pending_question
                && self.pending_user.is_some()
                && let Some(index) = self
                    .message_layouts
                    .iter()
                    .rposition(|message| message.role == ChatRole::User && message.id.is_none())
            {
                let top = self.message_layouts[index].top;
                self.pending_question_anchor = Some(top);
                self.conversation_scroll = top;
                self.scroll_to_pending_question = false;
                anchored_question = true;
            }
            if self.pending_user.is_none() {
                self.scroll_to_pending_question = false;
            }
            // Keep enough virtual tail for the newest question to remain at
            // the top, including after its optimistic turn becomes durable.
            // The next send replaces this anchor; opening another chat clears
            // it. Removing it on completion clamps short answers to the
            // natural bottom and produces a distracting second scroll.
            self.conversation_extent = self
                .pending_question_anchor
                .into_iter()
                .chain(self.question_navigation_anchor)
                .fold(content_extent, |extent, top| extent.max(top + area.h));
            // Streaming grows the document below the current reading
            // position. Never pull the reader to the new tail; only clamp a
            // position that became invalid because content got shorter.
            if self.scroll_loaded_chat_to_bottom {
                self.conversation_scroll = (self.conversation_extent - area.h).max(0.0);
                self.scroll_loaded_chat_to_bottom = false;
            } else if !anchored_question {
                self.conversation_scroll = self
                    .conversation_scroll
                    .min((self.conversation_extent - area.h).max(0.0));
            }
            if let Some((target, _)) = self.highlight
                && let Some(index) = self
                    .message_layouts
                    .iter()
                    .position(|message| message.blocks.iter().any(|block| block.id == target))
            {
                let top = self.message_layouts[index].top;
                self.conversation_scroll =
                    (top - area.h * 0.25).clamp(0.0, (self.conversation_extent - area.h).max(0.0));
            }
        }
        let clip = vello::kurbo::Rect::new(
            area.x as f64,
            area.y as f64,
            (area.x + area.w) as f64,
            (area.y + area.h) as f64,
        );
        canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        let p = theme::p();
        let now = Instant::now();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, y, h, message);
            if card.y + card.h >= area.y && card.y <= area.y + area.h {
                if message.role == ChatRole::User {
                    scene::fill(
                        canvas,
                        root,
                        p.row_selected,
                        &scene::rrect(card.x, card.y, card.w, card.h, 8.0),
                    );
                }
                if message.blocks.is_empty() {
                    if message.state == MessageState::Pending && message.display.is_empty() {
                        if let Some(pulse) = &self.pulse {
                            pulse.draw(
                                canvas,
                                root,
                                Rect::new(
                                    card.x + PAD,
                                    card.y + PAD,
                                    20.0,
                                    role::ui().metrics.line_height,
                                ),
                                now,
                                p.text_dim,
                            );
                        }
                    } else {
                        self.draw_response_selection(
                            canvas,
                            root,
                            message_index,
                            None,
                            card.x + PAD,
                            card.y + PAD,
                        );
                        scene::draw_layout(
                            canvas,
                            root,
                            &message.layout,
                            card.x + PAD,
                            card.y + PAD,
                            Some(p.text),
                        );
                    }
                } else {
                    let mut block_y = card.y + PAD;
                    for block in &message.blocks {
                        let highlighted =
                            self.highlight.is_some_and(|(target, _)| target == block.id);
                        if highlighted {
                            scene::fill(
                                canvas,
                                root,
                                p.row_selected,
                                &scene::rrect(
                                    card.x + 7.0,
                                    block_y - 3.0,
                                    card.w - 14.0,
                                    block.height() + 6.0,
                                    5.0,
                                ),
                            );
                        }
                        let block_index = message
                            .blocks
                            .iter()
                            .position(|candidate| candidate.id == block.id)
                            .unwrap_or(0);
                        if let Some(table) = &block.table {
                            koru_markdown::table::draw_grid(
                                canvas,
                                root,
                                table,
                                card.x + PAD,
                                block_y,
                                0.0,
                                None,
                            );
                        } else if let Some(image) = &block.image {
                            self.draw_remote_image(
                                canvas,
                                root,
                                image,
                                Rect::new(
                                    card.x + PAD,
                                    block_y,
                                    card.w - PAD * 2.0,
                                    block.height(),
                                ),
                                &block.layout,
                                Rect::new(
                                    card.x + PAD,
                                    block_y,
                                    card.w - PAD * 2.0,
                                    block.height(),
                                )
                                .contains(self.pointer.0, self.pointer.1),
                            );
                        } else {
                            self.draw_response_link_hover(
                                canvas,
                                root,
                                block,
                                card.x + PAD,
                                block_y,
                            );
                            self.draw_response_selection(
                                canvas,
                                root,
                                message_index,
                                Some(block_index),
                                card.x + PAD,
                                block_y,
                            );
                            scene::draw_layout(
                                canvas,
                                root,
                                &block.layout,
                                card.x + PAD,
                                block_y,
                                Some(p.text),
                            );
                        }
                        block_y += block.height() + BLOCK_GAP;
                    }
                }
                if let Some(usage) = &message.usage {
                    scene::draw_layout(
                        canvas,
                        root,
                        usage,
                        card.x + PAD,
                        card.y + PAD + message_content_height(message) + USAGE_GAP,
                        Some(p.text_dim),
                    );
                }
                if let Some(sources) = &message.sources {
                    let usage_h = message
                        .usage
                        .as_ref()
                        .map_or(0.0, |usage| usage.height() + USAGE_GAP);
                    scene::draw_layout(
                        canvas,
                        root,
                        &sources.layout,
                        card.x + PAD,
                        card.y + PAD + message_content_height(message) + usage_h + USAGE_GAP,
                        Some(p.accent),
                    );
                }
                if message.role == ChatRole::Assistant
                    && message.id.is_some()
                    && self.response_message_hover == Some(message_index)
                {
                    if message.state == MessageState::Complete {
                        for index in 0..6 {
                            let rect = action_rect(card, index);
                            let active = matches!(
                                (index, message.rating),
                                (1, Some(FeedbackValue::Good)) | (2, Some(FeedbackValue::Bad))
                            );
                            IconButton::new(response_action_glyph(index), rect)
                                .with_state(IconButtonState {
                                    hovered: rect.contains(self.pointer.0, self.pointer.1),
                                    active,
                                    disabled: index == 5 && message.blocks.is_empty(),
                                })
                                .draw(canvas, root, &mut self.engine);
                        }
                    } else if matches!(
                        message.state,
                        MessageState::Failed | MessageState::Interrupted
                    ) {
                        let rect = action_rect(card, 0);
                        IconButton::new(icons::REDO, rect)
                            .with_state(IconButtonState {
                                hovered: rect.contains(self.pointer.0, self.pointer.1),
                                active: false,
                                disabled: false,
                            })
                            .draw(canvas, root, &mut self.engine);
                    }
                }
            }
        }
        for group in &self.tool_group_layouts {
            let row = Rect::new(
                area.x,
                area.y - self.conversation_scroll + group.top,
                area.w,
                tool_groups::GROUP_H,
            );
            if row.y + row.h < area.y || row.y > area.y + area.h {
                continue;
            }
            let hovered = self.tool_group_hover == Some(group.message);
            scene::fill(
                canvas,
                root,
                if hovered { p.hover_tint } else { p.panel_bg },
                &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
            );
            let icon = self.engine.icon_layout(icons::WRENCH, 14.0);
            scene::draw_layout_first_line(
                canvas,
                root,
                &icon,
                row.x + 9.0,
                row.y + 7.0,
                Some(p.text_dim),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &group.title,
                row.x + 31.0,
                row.y + 6.0,
                Some(p.text),
            );
            let caret = self.engine.icon_layout(
                if self.collapsed_tool_groups.contains(&group.message) {
                    icons::CARET_RIGHT
                } else {
                    icons::CARET_DOWN
                },
                13.0,
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &caret,
                row.x + row.w - caret.width() - 9.0,
                row.y + 7.0,
                Some(p.text_dim),
            );
        }
        for tool in &self.tool_activity_layouts {
            if tool.hidden {
                continue;
            }
            let height = if self.expanded_tool_activity == Some(tool.activity.id) {
                tool.expanded_height
            } else {
                TOOL_ACTIVITY_ROW_H
            };
            let inset = if tool.before_message.is_some() {
                tool_groups::ROW_INSET
            } else {
                0.0
            };
            let row = Rect::new(
                area.x + inset,
                area.y - self.conversation_scroll + tool.top,
                area.w - inset,
                height,
            );
            if row.y + row.h >= area.y && row.y <= area.y + area.h {
                let hovered = self
                    .tool_activity_hover
                    .is_some_and(|(id, _)| id == tool.activity.id);
                scene::fill(
                    canvas,
                    root,
                    if hovered || self.expanded_tool_activity == Some(tool.activity.id) {
                        p.hover_tint
                    } else {
                        p.panel_bg
                    },
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
                scene::stroke(
                    canvas,
                    root,
                    p.panel_border,
                    theme::BORDER_W,
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
                let icon = self.engine.icon_layout(icons::WRENCH, 14.0);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &icon,
                    row.x + 9.0,
                    row.y + 9.0,
                    Some(p.text_dim),
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &tool.title,
                    row.x + 31.0,
                    row.y + 8.0,
                    Some(p.text),
                );
                let caret = self.engine.icon_layout(
                    if self.expanded_tool_activity == Some(tool.activity.id) {
                        icons::CARET_UP
                    } else {
                        icons::CARET_DOWN
                    },
                    13.0,
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &caret,
                    row.x + row.w - caret.width() - 9.0,
                    row.y + 9.0,
                    Some(p.text_dim),
                );
                if self.expanded_tool_activity == Some(tool.activity.id) {
                    scene::draw_layout(
                        canvas,
                        root,
                        &tool.detail,
                        row.x + 12.0,
                        row.y + 35.0,
                        Some(p.text_dim),
                    );
                    if tool.activity.status == ToolActivityStatus::ApprovalRequired
                        && tool.activity.approval == ToolApproval::Pending
                        && is_agent_row(&tool.activity)
                    {
                        agents::draw_agent_choices(
                            canvas,
                            root,
                            &mut self.engine,
                            self.lang,
                            self.pointer,
                            row,
                            &tool.activity,
                        );
                    } else if tool.activity.status == ToolActivityStatus::ApprovalRequired
                        && tool.activity.approval == ToolApproval::Pending
                    {
                        let [approve, auto_session, deny] = tool_activity_action_rects(row);
                        draw_surface_button(
                            canvas,
                            root,
                            &mut self.engine,
                            approve,
                            "Approve",
                            ButtonRole::Primary,
                            approve.contains(self.pointer.0, self.pointer.1),
                            false,
                        );
                        draw_surface_button(
                            canvas,
                            root,
                            &mut self.engine,
                            auto_session,
                            "Auto-approve this session",
                            ButtonRole::Secondary,
                            auto_session.contains(self.pointer.0, self.pointer.1),
                            false,
                        );
                        draw_surface_button(
                            canvas,
                            root,
                            &mut self.engine,
                            deny,
                            "Deny",
                            ButtonRole::Secondary,
                            deny.contains(self.pointer.0, self.pointer.1),
                            false,
                        );
                    }
                }
            }
        }
        canvas.pop_layer();
        if let Some(scrollbar) = self.conversation_scrollbar() {
            let active = self.conversation_scroll_drag.is_some()
                || scrollbar.in_gutter(self.pointer.0, self.pointer.1);
            koru_ui::scrollbar::draw(canvas, root, scrollbar, active);
        }
        self.draw_destination_picker(canvas, root);
        self.copy_feedback.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
    }

    fn response_action_at(&mut self, x: f32, y: f32) -> Option<ResponseAction> {
        let area = self.conversation_rect();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if let Some(message_id) = message.id
                && message.role == ChatRole::Assistant
                && self.response_message_hover == Some(message_index)
            {
                if message.state == MessageState::Complete {
                    for index in 0..6 {
                        if action_rect(card, index).contains(x, y) {
                            return match index {
                                0 => Some(ResponseAction::Copy(message.markdown.clone())),
                                1 => Some(ResponseAction::Rate(
                                    message_id,
                                    if message.rating == Some(FeedbackValue::Good) {
                                        FeedbackValue::Cleared
                                    } else {
                                        FeedbackValue::Good
                                    },
                                )),
                                2 => Some(ResponseAction::Rate(
                                    message_id,
                                    if message.rating == Some(FeedbackValue::Bad) {
                                        FeedbackValue::Cleared
                                    } else {
                                        FeedbackValue::Bad
                                    },
                                )),
                                3 => Some(ResponseAction::Branch(message_id)),
                                4 => Some(ResponseAction::Store(message_id)),
                                5 => message
                                    .blocks
                                    .first()
                                    .map(|block| ResponseAction::Link(block.id)),
                                _ => None,
                            };
                        }
                    }
                } else if matches!(
                    message.state,
                    MessageState::Failed | MessageState::Interrupted
                ) && action_rect(card, 0).contains(x, y)
                {
                    return Some(ResponseAction::Retry(message_id));
                }
            }
        }
        None
    }

    fn source_link_at(&self, x: f32, y: f32) -> Option<String> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for message in &self.message_layouts {
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, message_height(message), message);
            let Some(sources) = &message.sources else {
                continue;
            };
            let usage_h = message
                .usage
                .as_ref()
                .map_or(0.0, |usage| usage.height() + USAGE_GAP);
            let origin = (
                card.x + PAD,
                card.y + PAD + message_content_height(message) + usage_h + USAGE_GAP,
            );
            let rect = Rect::new(
                origin.0,
                origin.1,
                card.w - PAD * 2.0,
                sources.layout.height(),
            );
            if rect.contains(x, y) {
                return sources.hit(x - origin.0, y - origin.1).map(str::to_owned);
            }
        }
        None
    }

    fn tool_activity_height(&self, id: ToolInvocationId) -> f32 {
        if self.expanded_tool_activity == Some(id) {
            self.tool_activity_layouts
                .iter()
                .find(|tool| tool.activity.id == id)
                .map_or(TOOL_ACTIVITY_MIN_EXPANDED_H, |tool| tool.expanded_height)
        } else {
            TOOL_ACTIVITY_ROW_H
        }
    }

    /// The tool box heading under the pointer: the turn it belongs to.
    fn tool_group_at(&self, x: f32, y: f32) -> Option<MessageId> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        self.tool_group_layouts
            .iter()
            .find(|group| {
                Rect::new(
                    area.x,
                    area.y - self.conversation_scroll + group.top,
                    area.w,
                    tool_groups::GROUP_H,
                )
                .contains(x, y)
            })
            .map(|group| group.message)
    }

    /// Part 0 is the row, 1 is Approve, 2 is session auto-approve, and 3 is Deny.
    fn tool_activity_at(&self, x: f32, y: f32) -> Option<(ToolInvocationId, u8)> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for tool in &self.tool_activity_layouts {
            if tool.hidden {
                continue;
            }
            let height = self.tool_activity_height(tool.activity.id);
            let inset = if tool.before_message.is_some() {
                tool_groups::ROW_INSET
            } else {
                0.0
            };
            let row = Rect::new(
                area.x + inset,
                area.y - self.conversation_scroll + tool.top,
                area.w - inset,
                height,
            );
            if row.contains(x, y) {
                if self.expanded_tool_activity == Some(tool.activity.id)
                    && tool.activity.status == ToolActivityStatus::ApprovalRequired
                    && tool.activity.approval == ToolApproval::Pending
                    && is_agent_row(&tool.activity)
                {
                    if let Some(part) = agents::agent_choice_at(row, &tool.activity, x, y) {
                        return Some((tool.activity.id, part));
                    }
                    return Some((tool.activity.id, 0));
                }
                if self.expanded_tool_activity == Some(tool.activity.id)
                    && tool.activity.status == ToolActivityStatus::ApprovalRequired
                    && tool.activity.approval == ToolApproval::Pending
                {
                    let [approve, auto_session, deny] = tool_activity_action_rects(row);
                    if approve.contains(x, y) {
                        return Some((tool.activity.id, 1));
                    }
                    if auto_session.contains(x, y) {
                        return Some((tool.activity.id, 2));
                    }
                    if deny.contains(x, y) {
                        return Some((tool.activity.id, 3));
                    }
                }
                return Some((tool.activity.id, 0));
            }
        }
        None
    }

    fn response_action_hover_at(&mut self, x: f32, y: f32) -> Option<(usize, u8)> {
        let area = self.conversation_rect();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if message.role == ChatRole::Assistant
                && message.id.is_some()
                && self.response_message_hover == Some(message_index)
            {
                let action_count = if message.state == MessageState::Complete {
                    6
                } else if matches!(
                    message.state,
                    MessageState::Failed | MessageState::Interrupted
                ) {
                    1
                } else {
                    0
                };
                for index in 0..action_count {
                    if action_rect(card, index).contains(x, y) {
                        return Some((message_index, 64 + index as u8));
                    }
                }
            }
        }
        None
    }

    fn response_message_hover_at(
        &mut self,
        x: f32,
        y: f32,
        previous: Option<usize>,
    ) -> Option<usize> {
        let area = self.conversation_rect();
        for (index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if message.role == ChatRole::Assistant && message.id.is_some() {
                let actions = message
                    .state
                    .is_terminal()
                    .then_some(ACTION_H + 8.0)
                    .unwrap_or(0.0);
                let text = Rect::new(card.x, card.y, card.w, (card.h - actions).max(0.0));
                if text.contains(x, y) || (previous == Some(index) && card.contains(x, y)) {
                    return Some(index);
                }
            }
        }
        None
    }

    fn response_text_hit(&self, x: f32, y: f32) -> Option<ResponseTextHit> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if message.role == ChatRole::Assistant {
                if message.blocks.is_empty() {
                    let origin = (card.x + PAD, card.y + PAD);
                    let rect = Rect::new(
                        origin.0,
                        origin.1,
                        card.w - PAD * 2.0,
                        message.layout.height(),
                    );
                    if rect.contains(x, y) {
                        let selection =
                            Selection::from_point(&message.layout, x - origin.0, y - origin.1);
                        return Some(ResponseTextHit {
                            position: ResponsePosition {
                                message: message_index,
                                block: None,
                                byte: selection.focus().index(),
                                affinity: selection.focus().affinity(),
                            },
                            selection,
                            link: None,
                        });
                    }
                } else {
                    let mut block_y = card.y + PAD;
                    for (block_index, block) in message.blocks.iter().enumerate() {
                        let origin = (card.x + PAD, block_y);
                        let rect =
                            Rect::new(origin.0, origin.1, card.w - PAD * 2.0, block.height());
                        if block.image.is_some() && rect.contains(x, y) {
                            let selection =
                                Selection::from_point(&block.layout, x - origin.0, y - origin.1);
                            return Some(ResponseTextHit {
                                position: ResponsePosition {
                                    message: message_index,
                                    block: Some(block_index),
                                    byte: selection.focus().index(),
                                    affinity: selection.focus().affinity(),
                                },
                                selection,
                                link: block.image.as_ref().map(|image| image.url.clone()),
                            });
                        }
                        if block.table.is_none() && rect.contains(x, y) {
                            let selection =
                                Selection::from_point(&block.layout, x - origin.0, y - origin.1);
                            let link = Cluster::from_point_exact(
                                &block.layout,
                                x - origin.0,
                                y - origin.1,
                            )
                            .and_then(|(cluster, _)| {
                                let index = cluster.text_range().start;
                                block.links.iter().find(|link| link.range.contains(&index))
                            })
                            .map(|link| link.url.clone());
                            return Some(ResponseTextHit {
                                position: ResponsePosition {
                                    message: message_index,
                                    block: Some(block_index),
                                    byte: selection.focus().index(),
                                    affinity: selection.focus().affinity(),
                                },
                                selection,
                                link,
                            });
                        }
                        block_y += block.height() + BLOCK_GAP;
                    }
                }
            }
        }
        None
    }

    fn response_layout(
        &self,
        message_index: usize,
        block_index: Option<usize>,
    ) -> Option<(&Layout<Brush>, (f32, f32))> {
        let area = self.conversation_hit_rect;
        let message = self.message_layouts.get(message_index)?;
        let card = message_card(
            area,
            area.y - self.conversation_scroll + message.top,
            message_height(message),
            message,
        );
        match block_index {
            None => Some((&message.layout, (card.x + PAD, card.y + PAD))),
            Some(index) => {
                let block_y = card.y
                    + PAD
                    + message
                        .blocks
                        .get(..index)?
                        .iter()
                        .map(|block| block.height() + BLOCK_GAP)
                        .sum::<f32>();
                Some((&message.blocks.get(index)?.layout, (card.x + PAD, block_y)))
            }
        }
    }

    fn drag_response_selection(&mut self, x: f32, y: f32) -> bool {
        let Some(previous) = self
            .response_selection
            .as_ref()
            .filter(|selection| selection.dragging)
            .map(|selection| selection.focus)
        else {
            return false;
        };
        let Some(hit) = self.response_text_hit(x, y) else {
            return false;
        };
        let next = hit.position;
        let changed = next != previous;
        if let Some(target) = &mut self.response_selection {
            target.focus = next;
            if changed {
                target.pressed_link = None;
            }
        }
        changed
    }

    fn selected_response_text(&self) -> Option<String> {
        let selected = self.response_selection.as_ref()?;
        if selected.anchor == selected.focus {
            return None;
        }
        let mut pieces = Vec::new();
        for (message, block) in self.response_fragments() {
            let Some(text) = self.response_text(message, block) else {
                continue;
            };
            let Some(range) = self.response_selection_range(message, block, text.len()) else {
                continue;
            };
            if let Some(piece) = text.get(range)
                && !piece.is_empty()
            {
                pieces.push(piece);
            }
        }
        (!pieces.is_empty()).then(|| pieces.join("\n\n"))
    }

    fn draw_response_selection(
        &self,
        canvas: &mut Scene,
        root: Affine,
        message: usize,
        block: Option<usize>,
        ox: f32,
        oy: f32,
    ) {
        let Some(layout) = self
            .message_layouts
            .get(message)
            .and_then(|message| match block {
                Some(index) => message.blocks.get(index).map(|block| &block.layout),
                None => Some(&message.layout),
            })
        else {
            return;
        };
        let Some(text) = self.response_text(message, block) else {
            return;
        };
        let Some(range) = self.response_selection_range(message, block, text.len()) else {
            return;
        };
        let selection = Selection::new(
            Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
            Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
        );
        selection.geometry_with(layout, |bb, _| {
            scene::fill(
                canvas,
                root,
                theme::selection_color(),
                &scene::rect(
                    ox + bb.x0 as f32,
                    oy + bb.y0 as f32,
                    (bb.x1 - bb.x0) as f32,
                    (bb.y1 - bb.y0) as f32,
                ),
            );
        });
    }

    fn response_text(&self, message: usize, block: Option<usize>) -> Option<&str> {
        let message = self.message_layouts.get(message)?;
        match block {
            None if message.blocks.is_empty() => Some(&message.markdown),
            Some(index) => message.blocks.get(index).and_then(|block| {
                (block.table.is_none() && block.image.is_none()).then_some(block.text.as_str())
            }),
            None => None,
        }
    }

    fn response_fragments(&self) -> Vec<(usize, Option<usize>)> {
        let mut fragments = Vec::new();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            if message.role != ChatRole::Assistant {
                continue;
            }
            if message.blocks.is_empty() {
                fragments.push((message_index, None));
            } else {
                fragments.extend(
                    message
                        .blocks
                        .iter()
                        .enumerate()
                        .filter(|(_, block)| block.table.is_none() && block.image.is_none())
                        .map(|(index, _)| (message_index, Some(index))),
                );
            }
        }
        fragments
    }

    fn response_position_key(position: ResponsePosition) -> (usize, usize, usize) {
        (position.message, position.block.unwrap_or(0), position.byte)
    }

    fn response_selection_range(
        &self,
        message: usize,
        block: Option<usize>,
        text_len: usize,
    ) -> Option<std::ops::Range<usize>> {
        let selected = self.response_selection.as_ref()?;
        let (start, end) = if Self::response_position_key(selected.anchor)
            <= Self::response_position_key(selected.focus)
        {
            (selected.anchor, selected.focus)
        } else {
            (selected.focus, selected.anchor)
        };
        let fragment = (message, block.unwrap_or(0));
        let first = (start.message, start.block.unwrap_or(0));
        let last = (end.message, end.block.unwrap_or(0));
        if fragment < first || fragment > last {
            return None;
        }
        let from = if fragment == first {
            start.byte.min(text_len)
        } else {
            0
        };
        let to = if fragment == last {
            end.byte.min(text_len)
        } else {
            text_len
        };
        (from < to).then_some(from..to)
    }

    fn response_selection_contains(&self, x: f32, y: f32) -> bool {
        self.response_fragments()
            .into_iter()
            .any(|(message, block)| {
                let Some((layout, origin)) = self.response_layout(message, block) else {
                    return false;
                };
                let Some(text) = self.response_text(message, block) else {
                    return false;
                };
                let Some(range) = self.response_selection_range(message, block, text.len()) else {
                    return false;
                };
                let selection = Selection::new(
                    Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
                    Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
                );
                let mut contains = false;
                selection.geometry_with(layout, |bb, _| {
                    contains |= Rect::new(
                        origin.0 + bb.x0 as f32,
                        origin.1 + bb.y0 as f32,
                        (bb.x1 - bb.x0) as f32,
                        (bb.y1 - bb.y0) as f32,
                    )
                    .contains(x, y);
                });
                contains
            })
    }

    fn move_response_selection(&mut self, key: &Key) -> bool {
        let Some(focus) = self
            .response_selection
            .as_ref()
            .map(|selection| selection.focus)
        else {
            return false;
        };
        let Some((layout, _)) = self.response_layout(focus.message, focus.block) else {
            return false;
        };
        let current = Selection::from_byte_index(layout, focus.byte, focus.affinity);
        let desired_x = current.focus().geometry(layout, 0.0).x0 as f32;
        let vertical = matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown));
        let backwards = matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp));
        let moved = match key {
            Key::Named(NamedKey::ArrowLeft) => current.previous_visual(layout, false),
            Key::Named(NamedKey::ArrowRight) => current.next_visual(layout, false),
            Key::Named(NamedKey::ArrowUp) => current.previous_line(layout, false),
            Key::Named(NamedKey::ArrowDown) => current.next_line(layout, false),
            _ => return false,
        };
        let mut next = ResponsePosition {
            byte: moved.focus().index(),
            affinity: moved.focus().affinity(),
            ..focus
        };
        if next.byte == focus.byte {
            let fragments = self.response_fragments();
            let Some(index) = fragments
                .iter()
                .position(|&(message, block)| message == focus.message && block == focus.block)
            else {
                return false;
            };
            let neighbour = if backwards {
                index.checked_sub(1)
            } else {
                index.checked_add(1).filter(|&next| next < fragments.len())
            };
            let Some(neighbour) = neighbour else {
                return true;
            };
            let (message, block) = fragments[neighbour];
            let Some(text) = self.response_text(message, block) else {
                return false;
            };
            let byte = if backwards { text.len() } else { 0 };
            next = ResponsePosition {
                message,
                block,
                byte,
                affinity: if backwards {
                    Affinity::Upstream
                } else {
                    Affinity::Downstream
                },
            };
            if vertical {
                // Preserve the visual column when the line step crosses a
                // Markdown block, just as Parley does within one layout.
                if let Some((neighbour_layout, _)) = self.response_layout(message, block) {
                    let y = if backwards {
                        (neighbour_layout.height() - 0.5).max(0.0)
                    } else {
                        0.0
                    };
                    let cursor = Selection::from_point(neighbour_layout, desired_x, y).focus();
                    next.byte = cursor.index();
                    next.affinity = cursor.affinity();
                }
            }
        }
        if let Some(selection) = &mut self.response_selection {
            selection.focus = next;
            selection.pressed_link = None;
        }
        true
    }

    fn menu_viewport(&self) -> (f32, f32) {
        (
            self.viewport.x + self.viewport.w,
            self.viewport.y + self.viewport.h,
        )
    }

    fn draw_response_link_hover(
        &self,
        canvas: &mut Scene,
        root: Affine,
        block: &BlockLayout,
        ox: f32,
        oy: f32,
    ) {
        let Some((cluster, _)) =
            Cluster::from_point_exact(&block.layout, self.pointer.0 - ox, self.pointer.1 - oy)
        else {
            return;
        };
        let index = cluster.text_range().start;
        let Some(link) = block.links.iter().find(|link| link.range.contains(&index)) else {
            return;
        };
        let selection = Selection::new(
            Cursor::from_byte_index(&block.layout, link.range.start, Affinity::Downstream),
            Cursor::from_byte_index(&block.layout, link.range.end, Affinity::Upstream),
        );
        selection.geometry_with(&block.layout, |bb, _| {
            scene::fill(
                canvas,
                root,
                theme::p().hover_tint,
                &scene::rrect(
                    ox + bb.x0 as f32 - 1.0,
                    oy + bb.y0 as f32,
                    (bb.x1 - bb.x0) as f32 + 2.0,
                    (bb.y1 - bb.y0) as f32,
                    2.0,
                ),
            );
        });
    }

    fn ensure_remote_image(&mut self, url: &str) {
        if self.remote_images.contains_key(url) || !is_web_url(url) {
            return;
        }
        let result = Arc::new(Mutex::new(None));
        let worker_result = result.clone();
        let url = url.to_owned();
        let worker_url = url.clone();
        let _ = std::thread::Builder::new()
            .name("koru-ai-image".into())
            .spawn(move || {
                let loaded = load_remote_image(&worker_url);
                if let Ok(mut slot) = worker_result.lock() {
                    *slot = Some(loaded);
                }
            });
        self.remote_images.insert(
            url,
            RemoteImage {
                pending: Some(result),
                ready: None,
                failed: false,
            },
        );
    }

    fn maintain_remote_images(&mut self) -> bool {
        let mut changed = false;
        for image in self.remote_images.values_mut() {
            let completed = image
                .pending
                .as_ref()
                .and_then(|pending| pending.lock().ok()?.take());
            if let Some(completed) = completed {
                image.pending = None;
                match completed {
                    Ok(loaded) => image.ready = Some(loaded),
                    Err(_) => image.failed = true,
                }
                changed = true;
            }
        }
        changed
    }

    fn draw_remote_image(
        &self,
        canvas: &mut Scene,
        root: Affine,
        image: &ImageSpec,
        rect: Rect,
        fallback: &Layout<Brush>,
        hovered: bool,
    ) {
        let Some(loaded) = self
            .remote_images
            .get(&image.url)
            .and_then(|image| image.ready.as_ref())
        else {
            scene::fill(
                canvas,
                root,
                theme::p().code_bg,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
            );
            scene::draw_layout(
                canvas,
                root,
                fallback,
                rect.x + PAD,
                rect.y + PAD,
                Some(theme::p().text_dim),
            );
            if hovered {
                scene::stroke(
                    canvas,
                    root,
                    theme::p().focus_ring,
                    1.0,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            return;
        };
        let scale = (rect.w / loaded.width.max(1) as f32).min(rect.h / loaded.height.max(1) as f32);
        let width = loaded.width as f32 * scale;
        let height = loaded.height as f32 * scale;
        let x = rect.x + (rect.w - width) * 0.5;
        let y = rect.y + (rect.h - height) * 0.5;
        let transform = root
            * Affine::translate((x as f64, y as f64))
            * Affine::scale_non_uniform(
                width as f64 / loaded.width.max(1) as f64,
                height as f64 / loaded.height.max(1) as f64,
            );
        canvas.draw_image(&loaded.brush, transform);
        if hovered {
            scene::stroke(
                canvas,
                root,
                theme::p().focus_ring,
                1.0,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
            );
        }
    }

    fn perform_response_action(&mut self, action: ResponseAction) -> bool {
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.clone())
        else {
            return false;
        };
        match action {
            ResponseAction::Copy(markdown) => {
                koru_ui::clipboard::put_plain(&markdown);
                self.show_copy_feedback("Copied");
                true
            }
            ResponseAction::Rate(message, value) => self.send_command(Command::Feedback(
                message,
                FeedbackRequest {
                    event_id: FeedbackEventId::new(),
                    value,
                    surface: self.mode.wire(),
                },
            )),
            ResponseAction::Branch(message) => self.send_command(Command::Branch(
                chat.id,
                BranchChatRequest {
                    branch_chat_id: ChatId::new(),
                    through_message_id: message,
                    title: format!("{} — branch", chat.title),
                },
            )),
            ResponseAction::Store(message) => {
                if self.destinations.is_empty() {
                    self.status = Some("No writable Koru workspace is available.".into());
                } else {
                    self.destination_open = Some(message);
                }
                true
            }
            ResponseAction::Link(block) => {
                koru_ui::clipboard::put_plain(&koru_link::chat_link(
                    chat.id.as_uuid(),
                    Some(block.as_uuid()),
                ));
                self.show_copy_feedback("Link copied");
                true
            }
            ResponseAction::Retry(message) => {
                let (explicit_refs, _) = self.turn_refs(&[]);
                self.send_command(Command::Retry(
                    chat.id,
                    RetryTurnRequest {
                        operation_id: uuid::Uuid::new_v4(),
                        message_id: message,
                        surface: self.mode.wire(),
                        explicit_refs,
                        web_search: self.web_search_armed,
                        reasoning_override: None,
                        replace_pinned_documents: self.mode == SurfaceMode::ContextPane,
                    },
                ))
            }
        }
    }

    fn show_copy_feedback(&mut self, label: &str) {
        let (x, y) = self.pointer;
        self.copy_feedback.show_transient(
            TooltipContent::new(1, Rect::new(x, y, 1.0, 1.0), label),
            Instant::now(),
            Duration::from_millis(900),
        );
    }

    fn start_export(&mut self, through_message_id: MessageId) -> bool {
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.clone())
        else {
            return false;
        };
        let Some(destination) = self.destinations.get(self.selected_destination).cloned() else {
            self.status = Some("The selected Koru workspace is unavailable.".into());
            return true;
        };
        self.destination_open = None;
        self.send_command(Command::Export(
            chat.id,
            ExportChatRequest {
                export_id: DocumentExportId::new(),
                idempotency_key: uuid::Uuid::new_v4(),
                through_message_id,
                destination: DocumentDestination {
                    profile_id: destination.profile_id,
                    workspace_id: destination.workspace_id,
                    parent_document_id: None,
                    index: None,
                    title: chat.title,
                },
            },
        ))
    }

    fn draw_destination_picker(&mut self, canvas: &mut Scene, root: Affine) {
        if self.destination_open.is_none() {
            return;
        }
        let panel = destination_panel(self.viewport, self.destinations.len());
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let p = theme::p();
        for (index, destination) in self.destinations.iter().take(8).enumerate() {
            let row = destination_row(panel, index);
            if row.contains(self.pointer.0, self.pointer.1) || index == self.selected_destination {
                scene::fill(
                    canvas,
                    root,
                    p.row_selected,
                    &scene::rrect(row.x, row.y, row.w, row.h, 6.0),
                );
            }
            let label =
                self.engine
                    .simple_layout_wrapped(&destination.label, role::row(), row.w - 20.0);
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                row.x + 10.0,
                row.y + 9.0,
                Some(p.text),
            );
        }
    }

    fn destination_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = destination_panel(self.viewport, self.destinations.len());
        (0..self.destinations.len().min(8))
            .find(|index| destination_row(panel, *index).contains(x, y))
    }

    fn draw_setup(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let title = self.engine.simple_layout(
            if self.transcript.is_some() {
                "Settings"
            } else {
                "Set up Koru AI"
            },
            role::section_heading(),
        );
        let content = self.setup_content_rect();
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            content.x,
            self.viewport.y + 16.0,
            Some(p.text),
        );
        if self.setup_open && self.transcript.is_some() {
            let close = self.setup_close_button();
            draw_surface_button(
                canvas,
                root,
                &mut self.engine,
                close,
                "Done",
                ButtonRole::Secondary,
                close.contains(self.pointer.0, self.pointer.1),
                false,
            );
        }
        let cards = self.provider_cards();
        let koru_label = koru_i18n::t(self.lang, koru_i18n::Key::AiKoruProvider);
        let agents_label = koru_i18n::t(self.lang, koru_i18n::Key::AiLocalAgents);
        let has_agents = self
            .settings
            .as_ref()
            .is_some_and(|settings| !settings.agents.is_empty());
        for (index, (label, enabled, selected)) in [
            (koru_label, true, self.setup_provider == SetupProvider::Koru),
            ("OpenAI", true, self.setup_provider == SetupProvider::OpenAi),
            (
                "On this device — Ollama",
                true,
                self.setup_provider == SetupProvider::Ollama,
            ),
            (
                agents_label,
                has_agents,
                self.setup_provider == SetupProvider::Agents,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = cards[index];
            scene::fill(
                canvas,
                root,
                if selected {
                    p.row_selected
                } else if enabled && rect.contains(self.pointer.0, self.pointer.1) {
                    p.hover_tint
                } else {
                    p.panel_bg
                },
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
            );
            scene::stroke(
                canvas,
                root,
                if selected {
                    p.focus_ring
                } else {
                    p.panel_border
                },
                theme::BORDER_W,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
            );
            let line = self.engine.simple_layout(label, role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &line,
                rect.x + 12.0,
                rect.y + 13.0,
                Some(if enabled { p.text } else { p.text_dim }),
            );
        }
        let fields = self.field_rects();
        let style = InputStyle::inline();
        match self.setup_provider {
            SetupProvider::OpenAi => {
                let configured = self.openai_credential_configured();
                let mut key_field = fields[0];
                if configured && !self.editing_openai_key {
                    key_field.w = (key_field.w - 112.0).max(80.0);
                }
                self.openai_key.draw_masked(
                    canvas,
                    root,
                    &mut self.engine,
                    key_field,
                    style,
                    Some(if configured && !self.editing_openai_key {
                        "API key saved securely"
                    } else {
                        "Paste an OpenAI project API key"
                    }),
                    self.focus == Focus::OpenAiKey,
                );
                if configured && !self.editing_openai_key {
                    for (rect, label) in [
                        (self.openai_credential_replace_button(), "Replace"),
                        (self.openai_credential_clear_button(), "Clear"),
                    ] {
                        draw_surface_button(
                            canvas,
                            root,
                            &mut self.engine,
                            rect,
                            label,
                            if label == "Clear" {
                                ButtonRole::Destructive
                            } else {
                                ButtonRole::Secondary
                            },
                            rect.contains(self.pointer.0, self.pointer.1),
                            false,
                        );
                    }
                }
                self.openai_model.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    fields[1],
                    style,
                    Some("Model, for example gpt-5.1"),
                    self.focus == Focus::OpenAiModel,
                );
            }
            SetupProvider::Ollama => {
                self.ollama_endpoint.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    fields[0],
                    style,
                    Some("Ollama endpoint"),
                    self.focus == Focus::OllamaEndpoint,
                );
                let mut model_field = fields[1];
                if self.ollama_pull_action_rect().is_some() {
                    model_field.w = (model_field.w - 68.0).max(80.0);
                }
                self.ollama_model.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    model_field,
                    style,
                    Some("Installed model or model to pull"),
                    self.focus == Focus::OllamaModel,
                );
                if let Some(action) = self.ollama_pull_action_rect() {
                    if action.contains(self.pointer.0, self.pointer.1) {
                        scene::fill(
                            canvas,
                            root,
                            p.hover_tint,
                            &scene::rrect(action.x, action.y, action.w, action.h, 5.0),
                        );
                    }
                    let action_label = if self.pull_active { "Cancel" } else { "Pull" };
                    let label = self.engine.simple_layout(action_label, role::meta());
                    let enter = self.engine.icon_layout(icons::ENTER, 15.0);
                    let content_w = label.width() + enter.width() + 4.0;
                    let x = action.x + (action.w - content_w) * 0.5;
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &label,
                        x,
                        action.y + (action.h - label.height()) * 0.5,
                        Some(p.accent),
                    );
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &enter,
                        x + label.width() + 4.0,
                        action.y + (action.h - enter.height()) * 0.5,
                        Some(p.accent),
                    );
                }
            }
            SetupProvider::Agents => {
                self.draw_agent_rows(canvas, root, fields[0]);
            }
            SetupProvider::Koru => {
                let status = self.engine.simple_layout_wrapped(
                    &koru_i18n::t(self.lang, koru_i18n::Key::AiKoruHostedStatus),
                    role::meta(),
                    fields[0].w,
                );
                scene::draw_layout(
                    canvas,
                    root,
                    &status,
                    fields[0].x,
                    fields[0].y + 8.0,
                    Some(p.text_dim),
                );
            }
        }
        if !self.reasoning_choices().is_empty() {
            let reasoning = self.reasoning_button();
            let label = format!("Reasoning: {}", self.reasoning.as_token());
            draw_surface_button(
                canvas,
                root,
                &mut self.engine,
                reasoning,
                &label,
                ButtonRole::Secondary,
                reasoning.contains(self.pointer.0, self.pointer.1),
                false,
            );
        }
        let save = self.setup_save_button();
        let save_label = if self.transcript.is_some() {
            koru_i18n::t(self.lang, koru_i18n::Key::AiSaveAndCheck)
        } else {
            koru_i18n::t(self.lang, koru_i18n::Key::AiSaveAndStartChat)
        };
        draw_surface_button(
            canvas,
            root,
            &mut self.engine,
            save,
            &save_label,
            ButtonRole::Primary,
            save.contains(self.pointer.0, self.pointer.1),
            false,
        );
        self.ollama_search_link = None;
        if self.setup_provider == SetupProvider::Ollama && self.ollama_models.is_empty() {
            let link = self
                .engine
                .simple_layout("Browse Ollama models", role::meta());
            let x = content.x;
            let y = fields[1].y + fields[1].h + 8.0;
            let hit = Rect::new(x - 3.0, y - 3.0, link.width() + 6.0, link.height() + 6.0);
            if hit.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(hit.x, hit.y, hit.w, hit.h, 4.0),
                );
            }
            scene::draw_layout_first_line(canvas, root, &link, x, y, Some(p.accent));
            self.ollama_search_link = Some(hit);
        }
        if self.transcript.is_some() {
            for (rect, label) in [
                (self.clear_history_button(), "Clear history"),
                (self.clear_usage_button(), "Clear usage"),
            ] {
                draw_surface_button(
                    canvas,
                    root,
                    &mut self.engine,
                    rect,
                    label,
                    ButtonRole::Destructive,
                    rect.contains(self.pointer.0, self.pointer.1),
                    false,
                );
            }
        }
        self.retry_link = None;
        let status = self.pull_status.as_ref().or(self.status.as_ref()).cloned();
        if let Some(status) = status {
            let retryable = self.validation_retryable();
            let layout = self.engine.simple_layout_wrapped(
                &status,
                role::meta(),
                (content.w - if retryable { 52.0 } else { 0.0 }).max(1.0),
            );
            // Under the cards, and under the *Local agents* list when that is
            // what is open: the list grows with the agents installed, and the
            // line was drawn across its fourth row (the CTO's note).
            scene::draw_layout(
                canvas,
                root,
                &layout,
                content.x,
                self.viewport.y + 454.0 + self.ollama_catalog_gap() + self.agents_gap(),
                Some(p.text_dim),
            );
            if retryable {
                let retry = self.engine.simple_layout("Retry", role::meta());
                let x =
                    (content.x + layout.width() + 8.0).min(content.x + content.w - retry.width());
                let y = self.viewport.y + 454.0 + self.ollama_catalog_gap() + self.agents_gap();
                let hit = Rect::new(x - 3.0, y - 3.0, retry.width() + 6.0, retry.height() + 6.0);
                if hit.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(hit.x, hit.y, hit.w, hit.h, 4.0),
                    );
                }
                scene::draw_layout_first_line(canvas, root, &retry, x, y, Some(p.accent));
                self.retry_link = Some(hit);
            }
        }
        if let Some(prompt) = self.system_prompt_rect() {
            let label = self.engine.simple_layout(
                "System prompt override · this window · debug only",
                role::meta(),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                prompt.x,
                prompt.y - label.height() - 5.0,
                Some(p.text_dim),
            );
            scene::fill(
                canvas,
                root,
                p.panel_bg,
                &scene::rrect(prompt.x, prompt.y, prompt.w, prompt.h, 8.0),
            );
            scene::stroke(
                canvas,
                root,
                if self.focus == Focus::SystemPrompt {
                    p.focus_ring
                } else {
                    p.panel_border
                },
                theme::BORDER_W,
                &scene::rrect(prompt.x, prompt.y, prompt.w, prompt.h, 8.0),
            );
            self.system_prompt.draw(
                canvas,
                root,
                &mut self.engine,
                prompt,
                Some("Leave empty to use Koru's bundled system prompt"),
                self.focus == Focus::SystemPrompt,
            );
        }
        if let Some(panel) = self.model_options_panel() {
            scene::floating_panel(
                canvas,
                root,
                panel.x,
                panel.y,
                panel.w,
                panel.h,
                &scene::Panel::popover(6.0),
            );
            for (index, model) in self
                .model_options()
                .into_iter()
                .enumerate()
                .skip(self.model_option_offset)
                .take(MODEL_VISIBLE_ROWS)
            {
                let visible = index.saturating_sub(self.model_option_offset);
                let row = Rect::new(
                    panel.x + 4.0,
                    panel.y + 4.0 + visible as f32 * 30.0,
                    panel.w - 8.0,
                    30.0,
                );
                if row.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(row.x, row.y, row.w, row.h, 4.0),
                    );
                }
                let line = self.engine.simple_layout(&model, role::row());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &line,
                    row.x + 8.0,
                    row.y + (row.h - line.height()) * 0.5,
                    Some(p.text),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_surface_button(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    label: &str,
    role: ButtonRole,
    hovered: bool,
    selected: bool,
) {
    if selected {
        scene::fill(
            canvas,
            root,
            theme::p().row_selected,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, ActionButton::RADIUS),
        );
    }
    let style = match role {
        ButtonRole::Primary => ActionStyle::Primary,
        ButtonRole::Secondary => ActionStyle::Ghost,
        ButtonRole::Destructive => ActionStyle::Destructive,
    };
    ActionButton::new(label, style, None).draw(canvas, root, engine, rect, hovered);
}

fn draw_composer_action(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    stop: bool,
    enabled: bool,
    hovered: bool,
) {
    let p = theme::p();
    let fill = if stop {
        if hovered { p.hover_tint } else { p.panel_bg }
    } else if enabled {
        p.accent
    } else {
        p.row_selected
    };
    scene::fill(
        canvas,
        root,
        fill,
        &scene::rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5),
    );
    if stop {
        scene::stroke(
            canvas,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5),
        );
    }
    let glyph = engine.icon_layout(if stop { icons::SQUARE } else { icons::ARROW_UP }, 15.0);
    scene::draw_layout_first_line(
        canvas,
        root,
        &glyph,
        rect.x + (rect.w - glyph.width()) * 0.5,
        rect.y + (rect.h - glyph.height()) * 0.5,
        Some(if stop {
            p.text
        } else if enabled {
            p.on_accent
        } else {
            p.text_dim
        }),
    );
}

fn message_wrap_width(role: ChatRole, width: f32) -> f32 {
    let width = if role == ChatRole::User {
        width * USER_MESSAGE_MAX_FRACTION
    } else {
        width
    };
    (width - PAD * 2.0).max(1.0)
}

fn message_content_width(message: &MessageLayout) -> f32 {
    if message.blocks.is_empty() {
        message.layout.width()
    } else {
        message
            .blocks
            .iter()
            .map(BlockLayout::width)
            .fold(0.0, f32::max)
    }
}

fn message_card(area: Rect, y: f32, h: f32, message: &MessageLayout) -> Rect {
    if message.role != ChatRole::User {
        return Rect::new(area.x, y, area.w, h);
    }
    let max_width = (area.w * USER_MESSAGE_MAX_FRACTION).max(1.0);
    let width =
        (message_content_width(message) + PAD * 2.0).clamp(72.0_f32.min(max_width), max_width);
    Rect::new(area.x + area.w - width, y, width, h)
}

fn tool_activity_action_rects(row: Rect) -> [Rect; 3] {
    let approve_w = 78.0;
    let deny_w = 58.0;
    let auto_w = (row.w - approve_w - deny_w - 40.0).max(104.0);
    let y = row.y + row.h - BUTTON_H - 8.0;
    [
        Rect::new(row.x + 10.0, y, approve_w, BUTTON_H),
        Rect::new(row.x + 94.0, y, auto_w, BUTTON_H),
        Rect::new(row.x + row.w - deny_w - 10.0, y, deny_w, BUTTON_H),
    ]
}

fn tool_activity_title(activity: &ToolActivity) -> String {
    let words = activity.tool_name.replace('_', " ");
    let mut chars = words.chars();
    let title = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Workspace tool".into(),
    };
    let title = activity
        .display_target
        .as_deref()
        .map_or(title.clone(), |target| format!("{title} — {target}"));
    match tool_activity_duration(activity) {
        Some(duration) => format!("{title} - {duration}"),
        None => title,
    }
}

fn tool_activity_duration(activity: &ToolActivity) -> Option<String> {
    let (started, completed) = activity.started_at.zip(activity.completed_at)?;
    let elapsed_ms = completed.millis().checked_sub(started.millis())?;
    if elapsed_ms < 0 {
        return None;
    }
    // Tenths keep fast local tools meaningful without turning the compact row
    // into a stopwatch. Longer tools round to the whole-second form the row is
    // meant to summarize.
    if elapsed_ms >= 10_000 {
        return Some(format!("{}s", elapsed_ms.saturating_add(500) / 1_000));
    }
    let tenths = elapsed_ms.saturating_add(50) / 100;
    Some(if tenths % 10 == 0 {
        format!("{}s", tenths / 10)
    } else {
        format!("{}.{:01}s", tenths / 10, tenths % 10)
    })
}

fn pending_tool_activity(transcript: &ChatTranscript) -> Option<ToolInvocationId> {
    transcript
        .tool_activities
        .iter()
        .rev()
        .find(|activity| {
            activity.status == ToolActivityStatus::ApprovalRequired
                && activity.approval == ToolApproval::Pending
        })
        .map(|activity| activity.id)
}

fn tool_activity_detail(activity: &ToolActivity) -> String {
    let access = if activity.read_only { "Read" } else { "Change" };
    let status = match activity.status {
        ToolActivityStatus::Proposed => "proposed",
        ToolActivityStatus::ApprovalRequired => "waiting for approval",
        ToolActivityStatus::Running => "running",
        ToolActivityStatus::Complete => "complete",
        ToolActivityStatus::Denied => "denied",
        ToolActivityStatus::Failed => "failed",
        ToolActivityStatus::Cancelled => "cancelled",
    };
    let mut lines = vec![format!("{access} - {status}")];
    if let Some(target) = &activity.display_target {
        lines.push(format!("Document: {target}"));
    } else if let Some(document) = activity.target_document_id {
        lines.push(format!("Document: {document}"));
    }
    if let Some(message) = &activity.error_message {
        lines.push(format!("Failed: {message}"));
    }
    if let Some(bytes) = activity.result_bytes {
        lines.push(format!("Result: {}", format_result_bytes(bytes)));
    }
    lines.join("\n")
}

fn format_result_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        return format!("{bytes} B");
    }
    let kib = bytes as f64 / 1_024.0;
    if kib < 1_024.0 {
        return format!("{kib:.1} KB");
    }
    format!("{:.1} MB", kib / 1_024.0)
}

fn response_action_glyph(index: usize) -> char {
    match index {
        0 => icons::COPY,
        1 => icons::THUMBS_UP,
        2 => icons::THUMBS_DOWN,
        3 => icons::GIT_BRANCH,
        4 => icons::FLOPPY_DISK,
        _ => icons::LINK,
    }
}

fn render_chat_block(
    engine: &mut TextEngine,
    id: MessageBlockId,
    markdown: &str,
    max_w: f32,
) -> BlockLayout {
    if let Some(image) = generated_image_spec(markdown) {
        return BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            layout: engine.simple_layout_wrapped(&image.alt, role::ui(), max_w),
            text: image.alt.clone(),
            links: Vec::new(),
            table: None,
            image: Some(image),
        };
    }
    match koru_markdown::render_block(engine, markdown, max_w, &koru_markdown::Styles::ui()) {
        koru_markdown::Rendered::Text {
            text,
            layout,
            links,
        } => BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            text,
            layout,
            links,
            table: None,
            image: None,
        },
        koru_markdown::Rendered::Table(table) => BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            text: String::new(),
            layout: engine.simple_layout_wrapped("", role::ui(), max_w),
            links: Vec::new(),
            table: Some(table),
            image: None,
        },
        koru_markdown::Rendered::Image { url, alt } => BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            text: alt.clone(),
            layout: engine.simple_layout_wrapped(&alt, role::ui(), max_w),
            links: Vec::new(),
            table: None,
            image: Some(ImageSpec { url, alt }),
        },
        koru_markdown::Rendered::Rule => {
            let text = "\u{2500}".repeat(12);
            BlockLayout {
                id,
                markdown: markdown.to_owned(),
                wrap_width: max_w,
                layout: engine.simple_layout_wrapped(&text, role::ui(), max_w),
                text,
                links: Vec::new(),
                table: None,
                image: None,
            }
        }
    }
}

fn generated_image_spec(markdown: &str) -> Option<ImageSpec> {
    let trimmed = markdown.trim();
    let json = if trimmed.starts_with("```") {
        let start = trimmed.find('\n')? + 1;
        let end = trimmed.rfind("```")?;
        trimmed.get(start..end)?.trim()
    } else {
        trimmed
    };
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    (value.get("kind")?.as_str()? == "generated_image").then_some(ImageSpec {
        url: value.get("url")?.as_str()?.to_owned(),
        alt: value
            .get("alt")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("prompt").and_then(serde_json::Value::as_str))
            .unwrap_or("Generated image")
            .to_owned(),
    })
}

fn is_web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn load_remote_image(url: &str) -> ImageResult {
    const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;
    const MAX_IMAGE_EDGE: u32 = 1_600;
    if !is_web_url(url) {
        return Err("image URL is not HTTP or HTTPS".into());
    }
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_BYTES)
    {
        return Err("image is larger than 12 MB".into());
    }
    let bytes = response.bytes().map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("image is larger than 12 MB".into());
    }
    let mut rgba = image::load_from_memory(&bytes)
        .map_err(|error| error.to_string())?
        .to_rgba8();
    let (mut width, mut height) = rgba.dimensions();
    if width > MAX_IMAGE_EDGE || height > MAX_IMAGE_EDGE {
        let scale =
            (MAX_IMAGE_EDGE as f32 / width as f32).min(MAX_IMAGE_EDGE as f32 / height as f32);
        width = (width as f32 * scale).round().max(1.0) as u32;
        height = (height as f32 * scale).round().max(1.0) as u32;
        rgba = image::imageops::resize(&rgba, width, height, image::imageops::FilterType::Lanczos3);
    }
    let data = vello::peniko::ImageData {
        data: vello::peniko::Blob::new(Arc::new(rgba.into_raw())),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::Alpha,
        width,
        height,
    };
    Ok(LoadedImage {
        brush: vello::peniko::ImageBrush::new(data).with_quality(vello::peniko::ImageQuality::High),
        width,
        height,
    })
}

fn open_web_link(url: &str) -> Result<(), String> {
    if !is_web_url(url) {
        return Err(format!("refusing to open a non-web URL: {url}"));
    }
    if cfg!(any(test, feature = "no-client")) {
        return Ok(());
    }
    // **`koru_window::open_uri`, not `webbrowser`.** The workspace dropped that
    // crate deliberately: it launches the default browser with the URL on its
    // *command line*, where this asks the shell to open it. The allow-list
    // above is what makes this call safe, and it stays here because the URL is
    // remote input — it arrives in a model's response.
    koru_window::open_uri(url).map_err(|error| format!("could not open {url}: {error}"))
}

fn action_rect(card: Rect, index: usize) -> Rect {
    let gap = 2.0;
    Rect::new(
        card.x + PAD + index as f32 * (ACTION_H + gap),
        card.y + card.h - ACTION_H - 6.0,
        ACTION_H,
        ACTION_H,
    )
}

fn destination_panel(viewport: Rect, count: usize) -> Rect {
    let rows = count.clamp(1, 8);
    let width = (viewport.w - PAD * 4.0).clamp(180.0, 420.0);
    Rect::new(
        viewport.x + (viewport.w - width) * 0.5,
        viewport.y + 70.0,
        width,
        rows as f32 * HISTORY_ROW_H + 8.0,
    )
}

fn destination_row(panel: Rect, index: usize) -> Rect {
    Rect::new(
        panel.x + 4.0,
        panel.y + 4.0 + index as f32 * HISTORY_ROW_H,
        panel.w - 8.0,
        HISTORY_ROW_H,
    )
}

/// The sentence for a failed response, by its class, in the person's
/// language. A class the surface does not name reads as the generic one.
fn failure_label(class: Option<AiErrorClass>, lang: koru_i18n::Lang) -> String {
    let key = match class {
        Some(AiErrorClass::Configuration) => koru_i18n::Key::AiFailureConfiguration,
        Some(AiErrorClass::Authentication) => koru_i18n::Key::AiFailureAuthentication,
        Some(AiErrorClass::RateLimit) => koru_i18n::Key::AiFailureRateLimit,
        Some(AiErrorClass::ContextTooLarge) => koru_i18n::Key::AiFailureContextTooLarge,
        Some(AiErrorClass::ModelUnavailable) => koru_i18n::Key::AiFailureModelUnavailable,
        Some(AiErrorClass::Network) => koru_i18n::Key::AiFailureNetwork,
        Some(AiErrorClass::Cancelled) => koru_i18n::Key::AiFailureCancelled,
        Some(AiErrorClass::Storage) => koru_i18n::Key::AiFailureStorage,
        Some(AiErrorClass::Provider) | None => koru_i18n::Key::AiFailureGeneric,
    };
    koru_i18n::t(lang, key).to_owned()
}

/// A service failure as the status line reads it: the sentence its code
/// names, in the person's language, and — when the service said something
/// more specific (*the agent refused that model*) — its own words after
/// it. A code the surface does not know reads as the service's words alone,
/// so nothing a service can say goes unshown.
pub(crate) fn failure_text(failure: &koru_ai_wire::Failure, lang: koru_i18n::Lang) -> String {
    let key = match failure.code.as_str() {
        "ai_configuration" => Some(koru_i18n::Key::AiFailureConfiguration),
        "ai_authentication" => Some(koru_i18n::Key::AiFailureAuthentication),
        "ai_rate_limit" => Some(koru_i18n::Key::AiFailureRateLimit),
        "ai_context_too_large" => Some(koru_i18n::Key::AiFailureContextTooLarge),
        "ai_model_unavailable" => Some(koru_i18n::Key::AiFailureModelUnavailable),
        "ai_network" => Some(koru_i18n::Key::AiFailureNetwork),
        "ai_provider" => Some(koru_i18n::Key::AiFailureProvider),
        "ai_cancelled" => Some(koru_i18n::Key::AiFailureCancelled),
        "ai_storage" => Some(koru_i18n::Key::AiFailureStorage),
        "not_found" => Some(koru_i18n::Key::AiFailureNotFound),
        _ => None,
    };
    let message = failure.message.trim().trim_end_matches('.');
    match key {
        Some(key) => {
            let sentence = koru_i18n::t(lang, key);
            if message.is_empty() || message.eq_ignore_ascii_case(sentence) {
                sentence.to_owned()
            } else {
                format!("{sentence} · {message}")
            }
        }
        None => message.to_owned(),
    }
}

fn choices_for_reasoning(control: &ReasoningControl) -> Vec<ReasoningChoice> {
    match control {
        ReasoningControl::Unsupported => Vec::new(),
        ReasoningControl::Toggle { .. } => vec![
            ReasoningChoice::Default,
            ReasoningChoice::Off,
            ReasoningChoice::On,
        ],
        ReasoningControl::Levels {
            values,
            supports_off,
            ..
        } => {
            let mut choices = vec![ReasoningChoice::Default];
            if *supports_off {
                choices.push(ReasoningChoice::Off);
            }
            choices.extend(values.iter().copied().map(ReasoningChoice::Level));
            choices
        }
    }
}

#[cfg(test)]
fn provider_ready(settings: &ProviderSettingsResponse) -> bool {
    settings
        .selected_provider
        .as_ref()
        .is_some_and(|provider| match provider.as_str() {
            AiProviderId::OPENAI => {
                settings.openai.credential_configured && settings.openai.model.is_some()
            }
            AiProviderId::OLLAMA => settings.ollama.model.is_some(),
            AiProviderId::KORU => true,
            _ => false,
        })
}

fn selected_setup_provider(settings: &ProviderSettingsResponse) -> SetupProvider {
    match settings
        .selected_provider
        .as_ref()
        .map(AiProviderId::as_str)
    {
        Some(AiProviderId::OLLAMA) => SetupProvider::Ollama,
        Some(AiProviderId::KORU) => SetupProvider::Koru,
        Some(provider) if agents::is_agent_provider_str(provider) => SetupProvider::Agents,
        _ => SetupProvider::OpenAi,
    }
}

/// A chat's name from its first message (the CTO's note): the first line
/// that says something, its Markdown marks stripped, cut at a word before
/// `AUTO_TITLE_CHARS` with an ellipsis. `None` when nothing is left.
fn title_from_message(markdown: &str) -> Option<String> {
    let line = markdown
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(|c: char| matches!(c, '#' | '>' | '-' | '*' | '`'))
                .trim()
        })
        .find(|line| !line.is_empty())?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut title = String::new();
    for word in &words {
        let next = if title.is_empty() {
            (*word).to_owned()
        } else {
            format!("{title} {word}")
        };
        if next.chars().count() > AUTO_TITLE_CHARS {
            break;
        }
        title = next;
    }
    if title.is_empty() {
        title = line.chars().take(AUTO_TITLE_CHARS).collect();
    }
    let cut = title.chars().count() < line.chars().count();
    if cut {
        title.push('…');
    }
    Some(title)
}

fn validation_status_text(status: ProviderValidationStatus) -> &'static str {
    match status {
        ProviderValidationStatus::Unconfigured => "Finish configuring this provider.",
        ProviderValidationStatus::Unavailable => "The provider is not reachable.",
        ProviderValidationStatus::NoModels => "No installed models were found.",
        ProviderValidationStatus::Ready => "Provider ready.",
        ProviderValidationStatus::Failed => "Provider validation failed.",
    }
}

fn likely_openai_text_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    (model.starts_with("gpt-")
        || model.starts_with("chatgpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4"))
        && ![
            "audio",
            "realtime",
            "transcribe",
            "tts",
            "image",
            "embedding",
            "moderation",
        ]
        .iter()
        .any(|excluded| model.contains(excluded))
}

fn sort_model_options(models: &mut [String]) {
    models.sort_by(|a, b| {
        let family = |model: &str| (!model.starts_with("gpt-"), !model.starts_with('o'));
        family(a).cmp(&family(b)).then_with(|| b.cmp(a))
    });
}

fn preferred_openai_model(models: &[String]) -> Option<&str> {
    models
        .iter()
        .filter(|model| likely_openai_text_model(model))
        .max()
        .or_else(|| models.first())
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_ai_core::ReasoningLevel;
    use std::time::Duration;

    use koru_ai_core::{ProfileId, UsageTotals};

    fn transcript() -> ChatTranscript {
        let now = koru_ai_core::Timestamp::from_millis(1);
        let chat_id = ChatId::new();
        ChatTranscript {
            sources: Vec::new(),
            chat: Chat {
                id: chat_id,
                title: "Test chat".into(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                    requested_model: "gpt-test".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                created_at: now,
                updated_at: now,
            },
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            reasoning_control: None,
        }
    }

    fn destination(workspace_id: WorkspaceId, label: &str) -> DestinationOption {
        DestinationOption {
            profile_id: ProfileId::new(),
            workspace_id,
            label: label.into(),
        }
    }

    #[test]
    fn current_chat_id_follows_a_link_before_the_transcript_loads() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let id = ChatId::new();
        assert!(surface.current_chat_id().is_none());
        assert!(surface.open_link(id, None));
        assert_eq!(surface.current_chat_id(), Some(id));
    }

    #[test]
    fn drain_updates_is_a_no_op_without_a_reader() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        assert!(!surface.drain_updates());
        assert!(surface.listing_deadline().is_none());
    }

    #[test]
    fn recent_chats_takes_the_first_n_in_list_order() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let a = ChatId::new();
        let b = ChatId::new();
        let chat = |id, title: &str| Chat {
            id,
            title: title.into(),
            ..transcript().chat
        };
        surface.chats = vec![chat(a, "One"), chat(b, "Two")];
        assert_eq!(surface.recent_chats(1), vec![(a, "One".into())]);
        assert_eq!(
            surface.recent_chats(10),
            vec![(a, "One".into()), (b, "Two".into())]
        );
    }

    #[test]
    fn test_surface_starts_no_reader_or_maintenance_clock() {
        let surface = ChatSurface::new(SurfaceMode::Standalone);
        assert!(surface.reader.is_none());
        assert!(surface.maintenance_deadline().is_none());
    }

    #[test]
    fn dismiss_popovers_closes_every_transient_overlay() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        surface.history_open = true;
        surface.chat_model_open = true;
        surface.tool_access_open = true;
        surface.tool_mode_open = true;
        surface.model_options_open = true;
        surface.destination_open = Some(MessageId::new());
        surface.response_menu = Some(Menu::new(&mut surface.engine, (0.0, 0.0), vec![]));
        surface.response_menu_anchor = Some(Rect::new(0.0, 0.0, 0.0, 0.0));
        surface.confirm = Some(Confirm::new("", "", ""));

        assert!(surface.dismiss_popovers());
        assert!(!surface.history_open);
        assert!(!surface.chat_model_open);
        assert!(!surface.tool_access_open);
        assert!(!surface.tool_mode_open);
        assert!(!surface.model_options_open);
        assert!(surface.destination_open.is_none());
        assert!(surface.response_menu.is_none());
        assert!(surface.response_menu_anchor.is_none());
        assert!(surface.confirm.is_none());

        // Calling again is a no-op.
        assert!(!surface.dismiss_popovers());
    }

    #[test]
    fn context_document_attaches_with_title_and_updates_title_without_dismissal() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let doc_id = DocumentId::new();
        assert!(surface.set_context_document(Some((doc_id, "Launch plan".into()))));
        assert_eq!(
            surface.context_document,
            Some((doc_id, "Launch plan".into()))
        );

        // Title update alone does not re-attach or touch dismissal.
        assert!(surface.set_context_document(Some((doc_id, "Launch plan v2".into()))));
        assert_eq!(
            surface.context_document,
            Some((doc_id, "Launch plan v2".into()))
        );
        assert!(surface.dismissed_context_document.is_none());
    }

    #[test]
    fn removing_context_document_dismisses_it_until_document_changes() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let doc_a = DocumentId::new();
        surface.set_context_document(Some((doc_a, "A".into())));
        surface.remove_context_document();
        assert!(surface.context_document.is_none());
        assert_eq!(surface.dismissed_context_document, Some(doc_a));

        // Same document stays dismissed.
        assert!(!surface.set_context_document(Some((doc_a, "A updated".into()))));
        assert!(surface.context_document.is_none());

        // A different document clears dismissal and attaches.
        let doc_b = DocumentId::new();
        assert!(surface.set_context_document(Some((doc_b, "B".into()))));
        assert_eq!(surface.context_document.map(|(id, _)| id), Some(doc_b));
        assert!(surface.dismissed_context_document.is_none());
    }

    #[test]
    fn a_tab_without_a_document_clears_the_pill_without_recording_a_dismissal() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let doc_a = DocumentId::new();
        surface.set_context_document(Some((doc_a, "A".into())));

        // The new tab has no document; the previous tab's pill must not linger
        // (and must not be sent as context).
        assert!(surface.set_context_document(None));
        assert!(surface.context_document.is_none());

        // Nothing was dismissed, so returning to the document re-offers it.
        assert!(surface.set_context_document(Some((doc_a, "A".into()))));
        assert_eq!(surface.context_document.map(|(id, _)| id), Some(doc_a));
    }

    #[test]
    fn clearing_dismissal_re_attaches_the_current_document() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let doc_a = DocumentId::new();
        surface.set_context_document(Some((doc_a, "A".into())));
        surface.remove_context_document();
        assert!(surface.context_document.is_none());

        surface.clear_context_document_dismissal();
        assert!(surface.set_context_document(Some((doc_a, "A".into()))));
        assert_eq!(surface.context_document.map(|(id, _)| id), Some(doc_a));
    }

    #[test]
    fn send_composer_includes_context_document_reference() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript.clone());
        surface.composer.set_text("hello");

        let doc_id = DocumentId::new();
        surface.set_context_document(Some((doc_id, "Launch plan".into())));

        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: doc_id
                    }]
                );
            }
            _ => panic!("composer send must issue a turn"),
        }
    }

    #[test]
    fn send_composer_omits_context_document_after_removal() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript.clone());
        surface.composer.set_text("hello");

        let doc_id = DocumentId::new();
        surface.set_context_document(Some((doc_id, "Launch plan".into())));
        surface.remove_context_document();

        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert!(request.explicit_refs.is_empty()),
            _ => panic!("composer send must issue a turn"),
        }
    }

    #[test]
    fn context_pane_without_document_sends_immediately() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.composer.set_text("quick test");

        assert!(surface.send_composer());
        assert!(surface.staged_embedded_send.is_none());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(request.markdown, "quick test");
                assert!(request.explicit_refs.is_empty());
            }
            _ => panic!("composer send must issue a turn"),
        }
    }

    #[test]
    fn context_pane_with_document_stages_until_durable() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.composer.set_text("document question");

        let doc_id = DocumentId::new();
        surface.set_context_document(Some((doc_id, "Notes".into())));

        assert!(surface.send_composer());
        assert!(surface.staged_embedded_send.is_some());
        assert!(commands.try_recv().is_err());

        // First durable observation is not enough (needs 2 observations).
        assert!(!surface.submit_embedded_when_durable(true));
        assert!(surface.staged_embedded_send.is_some());
        assert!(commands.try_recv().is_err());

        // Second durable observation releases and submits the send.
        assert!(surface.submit_embedded_when_durable(true));
        assert!(surface.staged_embedded_send.is_none());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(request.markdown, "document question");
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: doc_id
                    }]
                );
            }
            _ => panic!("composer send must issue a turn"),
        }
    }

    /// **New chat opens a new chat, not the last one.** The boot is the
    /// first answer a fresh tab gets, and it carries the service's current
    /// chat; a tab that asked for a new chat drops it there too.
    #[test]
    fn a_new_chat_tab_does_not_take_the_last_chat_from_the_boot() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat());
        let last = transcript();
        surface.apply_update(Update::Boot {
            settings: ProviderSettingsResponse {
                selected_provider: Some(AiProviderId::parse("openai").unwrap()),
                openai: koru_ai_wire::OpenAiSettingsView {
                    model: Some("gpt-5.1".into()),
                    reasoning: ReasoningChoice::Default,
                    credential_configured: true,
                },
                ollama: koru_ai_wire::OllamaSettingsView {
                    endpoint: "http://localhost:11434".into(),
                    model: None,
                    reasoning: ReasoningChoice::Default,
                },
                agents: Vec::new(),
            },
            chats: vec![last.chat.clone()],
            transcript: Some(last.clone()),
            destinations: Vec::new(),
            destination_error: None,
        });
        assert!(
            surface.transcript.is_none(),
            "the last chat is not this tab's"
        );
        assert!(surface.fresh_chat_wanted, "the new chat is still owed");
        assert_eq!(surface.chats.len(), 1, "the list is still the list");
    }

    /// **A tab opened for a new chat gets one** (the CTO's note): the boot
    /// answers with the service's current chat, and before that answer there
    /// is no provider to create under. The intent is kept, the snapshot's
    /// chat is dropped, and the chat this tab asked for is the one it shows.
    #[test]
    fn a_tab_asked_for_a_new_chat_does_not_take_the_services_current_one() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat(), "the intent is kept");
        assert!(
            commands.try_recv().is_err(),
            "nothing to create under before the boot answers"
        );
        assert!(!surface.setup_open, "and no setup is forced open");
        let existing = transcript();
        surface.apply_update(Update::HistoryChanged {
            chats: vec![existing.chat.clone()],
            transcript: Some(existing.clone()),
        });
        assert!(
            surface.transcript.is_none(),
            "the service's current chat is not this tab's"
        );
        assert_eq!(surface.chats.len(), 1, "the list is still the list");
        let mut fresh = transcript();
        fresh.chat.id = ChatId::new();
        surface.apply_update(Update::Created(fresh.clone()));
        assert_eq!(
            surface.transcript.as_ref().map(|t| t.chat.id),
            Some(fresh.chat.id)
        );
        assert!(!surface.fresh_chat_wanted, "the debt is paid");
        surface.apply_update(Update::HistoryChanged {
            chats: vec![existing.chat.clone()],
            transcript: Some(existing.clone()),
        });
        assert_eq!(
            surface.transcript.as_ref().map(|t| t.chat.id),
            Some(existing.chat.id),
            "a later snapshot applies as it always did"
        );
    }

    /// **A chat's first message names it** (the CTO's note), the way ChatGPT
    /// names a chat: the first line, cut at a word, Markdown marks stripped.
    /// A chat already named keeps its name.
    #[test]
    fn the_first_message_names_a_new_chat() {
        assert_eq!(
            title_from_message("# Plan the launch\n\nmore"),
            Some("Plan the launch".into())
        );
        assert_eq!(
            title_from_message(
                "Write a release note for the workspace foundation that covers the rail and the floor"
            ),
            Some("Write a release note for the workspace…".into())
        );
        assert_eq!(title_from_message("   \n\n"), None);
        assert_eq!(
            title_from_message("> quoted question?"),
            Some("quoted question?".into())
        );

        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.title = DEFAULT_CHAT_TITLE.into();
        let id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        assert!(surface.key(
            &Key::Character("h".into()),
            Some("How do I pin a tab"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        let Command::Send(_) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
            panic!("expected a send command");
        };
        let Command::Rename(renamed, request) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("expected the rename after the send");
        };
        assert_eq!(renamed, id);
        assert_eq!(request.title, "How do I pin a tab");

        // Named already: no rename rides the next send.
        let mut chat = transcript();
        chat.chat.title = "Pins".into();
        surface.transcript = Some(chat);
        assert!(surface.key(
            &Key::Character("a".into()),
            Some("and again"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        let Command::Send(_) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
            panic!("expected a send command");
        };
        assert!(commands.try_recv().is_err(), "no rename for a named chat");
    }

    #[test]
    fn web_search_is_capability_gated_sticky_and_streams_visible_sources() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.current_model.requested_model = "gpt-5.4".into();
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer_rect();

        let button = surface.web_search_button().expect("gpt-5.4 can search");
        assert!(surface.mouse_down(button.x + button.w / 2.0, button.y + button.h / 2.0));
        assert!(surface.web_search_armed);
        assert!(surface.key(
            &Key::Character("q".into()),
            Some("question"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        let Command::Send(request) = commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("expected a send command");
        };
        assert!(request.web_search);

        let attempt_id = InferenceAttemptId::new();
        let update = |event| {
            Update::Event(koru_ai_wire::StreamEvent {
                version: koru_ai_wire::AI_STREAM_VERSION,
                sequence: 1,
                event,
            })
        };
        surface.apply_update(update(PublicAiEvent::WebSearch {
            attempt_id,
            call_id: "search-1".into(),
            phase: WebSearchPhase::Started,
            at: koru_ai_core::Timestamp::from_millis(2),
        }));
        let width = surface.conversation_rect().w;
        surface.rebuild_messages(width);
        assert_eq!(
            surface.message_layouts.last().unwrap().display,
            "Searching the web…"
        );

        surface.apply_update(update(PublicAiEvent::Citation {
            attempt_id,
            source: SourceLink {
                url: "https://example.com/source".into(),
                title: "Example source".into(),
            },
            at: koru_ai_core::Timestamp::from_millis(3),
        }));
        surface.rebuild_messages(width);
        let pending = surface.message_layouts.last().unwrap();
        assert_eq!(pending.sources.as_ref().unwrap().sources().count(), 1);

        surface.apply_update(update(PublicAiEvent::TextDelta {
            attempt_id,
            text: "Answer".into(),
            at: koru_ai_core::Timestamp::from_millis(4),
        }));
        surface.rebuild_messages(width);
        assert_eq!(surface.message_layouts.last().unwrap().display, "Answer");

        surface.transcript.as_mut().unwrap().chat.current_model = ModelSelection {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            requested_model: "qwen3:4b".into(),
            default_reasoning: ReasoningChoice::Default,
        };
        assert!(surface.web_search_button().is_none());
    }

    #[test]
    fn a_stream_event_relays_out_only_the_pending_turn() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        let message_id = MessageId::new();
        transcript.messages.push(koru_ai_core::Message {
            id: message_id,
            chat_id: transcript.chat.id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: "A durable answer".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
        });
        transcript.blocks.push(koru_ai_core::MessageBlock {
            id: MessageBlockId::new(),
            message_id,
            ordinal: 0,
            kind: koru_ai_core::MessageBlockKind::Paragraph,
            markdown: "A **durable** answer".into(),
            plain_text: "A durable answer".into(),
        });
        surface.transcript = Some(transcript);
        surface.pending_user = Some("What next?".into());
        surface.draft = "First".into();
        surface.rebuild_messages(400.0);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 1);

        surface.draft.push_str(" delta");
        surface.rebuild_messages(400.0);
        assert_eq!(surface.last_rebuild_stats.messages_reused, 1);
        assert_eq!(surface.last_rebuild_stats.blocks_reused, 1);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 0);

        surface.rebuild_messages(500.0);
        assert_eq!(surface.last_rebuild_stats.messages_reused, 0);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 1);
    }

    #[test]
    fn an_invalid_table_renders_as_its_source_text() {
        let source = "| A | B |\n| --- |";
        let mut engine = TextEngine::new();
        let block = render_chat_block(&mut engine, MessageBlockId::new(), source, 300.0);
        assert!(block.table.is_none());
        assert_eq!(block.text, source);
    }

    #[test]
    fn missing_authoritative_attempt_ends_recovery_with_retryable_status() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let attempt = InferenceAttemptId::new();
        surface.attempt = Some(attempt);
        surface.waiting = true;
        surface.pending_assistant_message_id = Some(MessageId::new());
        surface.pending_user = Some("Question that never became authoritative".into());
        surface.draft = "Partial answer".into();
        surface.pending_question_anchor = Some(42.0);
        surface.scroll_to_pending_question = true;
        surface.messages_dirty = false;

        assert!(surface.apply_update(Update::RecoveryFailed {
            attempt,
            message: "This response could not be recovered. You can retry it.".into(),
        }));

        assert_eq!(surface.attempt, None);
        assert!(!surface.waiting);
        assert_eq!(surface.pending_assistant_message_id, None);
        assert_eq!(surface.pending_user, None);
        assert!(surface.draft.is_empty());
        assert_eq!(surface.pending_question_anchor, None);
        assert!(!surface.scroll_to_pending_question);
        assert!(surface.messages_dirty);
        assert_eq!(
            surface.status.as_deref(),
            Some("This response could not be recovered. You can retry it.")
        );
    }

    #[test]
    fn stale_recovery_failure_does_not_clear_a_newer_attempt() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let stale = InferenceAttemptId::new();
        let current = InferenceAttemptId::new();
        surface.attempt = Some(current);
        surface.waiting = true;

        assert!(surface.apply_update(Update::RecoveryFailed {
            attempt: stale,
            message: "stale".into(),
        }));

        assert_eq!(surface.attempt, Some(current));
        assert!(surface.waiting);
        assert_ne!(surface.status.as_deref(), Some("stale"));
    }

    #[test]
    fn composer_grows_to_eight_lines_then_caps() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let base = surface.composer_rect().h;
        for _ in 0..12 {
            surface.composer.insert_break();
        }
        let grown = surface.composer_rect().h;
        // A composer line is a line of the app's text; a break is a new
        // paragraph, with the editor's small gap between paragraphs.
        let line = chat_line_height();
        let gaps = theme::TASK_GAP * (COMPOSER_MAX_LINES - 1) as f32;
        assert!(grown > base);
        assert!(
            grown
                <= line * COMPOSER_MAX_LINES as f32
                    + gaps
                    + COMPOSER_PAD_TOP
                    + COMPOSER_TEXT_GAP
                    + COMPOSER_TOOLBAR_H
                    + COMPOSER_PAD_BOTTOM
                    + 1.0
        );
        // Four lines are four lines: the composer grows until the eighth.
        let mut short = ChatSurface::new(SurfaceMode::Standalone);
        short.composer.set_text("1\n2\n3\n4");
        let four = short.composer_rect().h;
        assert!(four > base && four < grown);
    }

    #[test]
    fn alt_arrows_visit_questions_then_return_to_the_composer() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for (ordinal, role) in [
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::User,
            ChatRole::Assistant,
        ]
        .into_iter()
        .enumerate()
        {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal: ordinal as u32,
                role,
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.messages_dirty = true;
        let alt = ModifiersState::ALT;

        assert!(surface.key(&Key::Named(NamedKey::ArrowUp), None, alt));
        assert_eq!(surface.question_navigation, Some(2));
        assert_eq!(surface.focus, Focus::None);
        assert!(surface.key(&Key::Named(NamedKey::ArrowUp), None, alt));
        assert_eq!(surface.question_navigation, Some(1));
        assert!(surface.key(&Key::Named(NamedKey::ArrowDown), None, alt));
        assert_eq!(surface.question_navigation, Some(2));
        assert!(surface.key(&Key::Named(NamedKey::ArrowDown), None, alt));
        assert_eq!(surface.question_navigation, None);
        assert_eq!(surface.focus, Focus::Composer);
    }

    #[test]
    fn loaded_chat_scrolls_to_its_end_once() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for ordinal in 0..24 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.messages_dirty = true;
        surface.scroll_loaded_chat_to_bottom = true;

        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        assert!(surface.conversation_scroll > 0.0);
        let conversation_height = surface.conversation_rect().h;
        let expected_scroll = (surface.conversation_extent - conversation_height).max(0.0);
        assert_eq!(surface.conversation_scroll, expected_scroll,);
        assert!(!surface.scroll_loaded_chat_to_bottom);
    }

    #[test]
    fn authoritative_recovery_preserves_same_chat_scroll_position() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for ordinal in 0..24 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        let max_scroll = (surface.conversation_extent - surface.conversation_rect().h).max(0.0);
        surface.conversation_scroll = max_scroll * 0.4;
        let before = surface.conversation_scroll;
        let attempt = InferenceAttemptId::new();
        surface.attempt = Some(attempt);
        surface.waiting = true;
        let authoritative = surface.transcript.clone().unwrap();

        surface.apply_update(Update::Recovered {
            attempt,
            status: koru_ai_core::AttemptStatus::Complete,
            transcript: authoritative,
        });
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        assert!((surface.conversation_scroll - before).abs() < 0.01);
        assert!(!surface.waiting);
    }

    #[test]
    fn completed_short_answer_keeps_the_submitted_question_at_the_top() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for ordinal in 0..10 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("older message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.pending_user = Some("Another one".into());
        surface.scroll_to_pending_question = true;
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        let anchored = surface.conversation_scroll;
        assert_eq!(surface.pending_question_anchor, Some(anchored));

        let transcript = surface.transcript.as_mut().unwrap();
        for (ordinal, role, markdown) in [
            (10, ChatRole::User, "Another one"),
            (11, ChatRole::Assistant, "A short answer"),
        ] {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role,
                state: MessageState::Complete,
                markdown: markdown.into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.pending_user = None;
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        let natural_max =
            (surface.conversation_content_extent() - surface.conversation_rect().h).max(0.0);
        assert!(natural_max < anchored);
        assert!((surface.conversation_scroll - anchored).abs() < 0.01);
    }

    #[test]
    fn workspace_selection_replaces_the_complete_chat_grant_set() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat = transcript.chat.id;
        let workspace = WorkspaceId::new();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);

        assert!(surface.toggle_tool_workspace(workspace));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateToolPolicy(actual, request) => {
                assert_eq!(actual, chat);
                assert_eq!(request.mode, ToolMode::Manual);
                assert_eq!(request.workspace_ids, vec![workspace]);
            }
            _ => panic!("workspace selection must replace the chat policy"),
        }
    }

    #[test]
    fn embedded_context_workspace_is_first_and_initially_selected() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, commands, _updates) = Reader::test_pair();
        let current = WorkspaceId::new();
        let other = WorkspaceId::new();
        let mut transcript = transcript();
        let chat = transcript.chat.id;
        transcript.tool_policy.workspace_ids = vec![other];
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.destinations = vec![
            destination(other, "Other / Workspace"),
            destination(current, "Current / Workspace"),
        ];

        assert!(surface.set_context_workspace(Some(current)));
        assert_eq!(surface.destinations[0].workspace_id, current);
        assert_eq!(
            surface
                .transcript
                .as_ref()
                .unwrap()
                .tool_policy
                .workspace_ids,
            vec![current, other]
        );
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateToolPolicy(actual, request) => {
                assert_eq!(actual, chat);
                assert_eq!(request.mode, ToolMode::Manual);
                assert_eq!(request.workspace_ids, vec![current, other]);
            }
            _ => panic!("context selection must persist as an ordinary policy update"),
        }
        assert!(!surface.set_context_workspace(Some(current)));
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn composer_footer_separates_workspace_approval_and_model_controls() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        transcript.chat.current_model.requested_model = "gpt-5.5".into();
        surface.transcript = Some(transcript);

        let toolbar = surface.composer_toolbar_rect();
        let workspace = surface.tool_access_button();
        let approval = surface.tool_mode_button();
        let model = surface.chat_model_button();
        let privacy = surface.privacy_button().expect("a chat can be private");
        let search = surface
            .web_search_button()
            .expect("gpt-5.5 supports search");

        assert_eq!(privacy.x, toolbar.x);
        assert!(privacy.x + privacy.w < search.x);
        assert!((model.x + model.w - toolbar.x - toolbar.w).abs() < 0.01);
        assert!(search.x + search.w < workspace.x);
        assert!(workspace.x + workspace.w < approval.x);
        assert!(approval.x + approval.w < model.x);
        assert_eq!(surface.tool_access_label(), "0 spaces");
        assert_eq!(surface.tool_mode_label(), "Manual");
    }

    #[test]
    fn workspace_access_adds_search_only_for_large_lists_and_filters_rows() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.transcript = Some(transcript());
        surface.destinations = (0..6)
            .map(|index| destination(WorkspaceId::new(), &format!("Workspace {index}")))
            .collect();
        surface.tool_access_open = true;

        assert!(surface.tool_access_search_rect().is_some());
        surface
            .workspace_search
            .set_text(&mut surface.engine, "Workspace 4");
        let matches = surface.matching_workspace_indices();
        assert_eq!(matches, vec![4]);

        surface.destinations.truncate(5);
        assert!(surface.tool_access_search_rect().is_none());
    }

    #[test]
    fn chat_model_picker_is_wide_and_scrollable() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.transcript = Some(transcript());
        surface.openai_models = (0..12).map(|index| format!("gpt-5.{index}")).collect();
        surface.chat_model_open = true;

        let panel = surface.chat_model_panel().expect("model panel");
        let button = surface.chat_model_button();
        assert!(panel.w > button.w);
        assert!(surface.chat_model_scrollbar().is_some());
    }

    #[test]
    fn composer_owns_its_disabled_send_action() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        let composer = surface.composer_rect();
        let send = surface.send_button();
        assert!(composer.contains(send.x + send.w * 0.5, send.y + send.h * 0.5));
        assert!(!surface.send_composer());
        assert!(surface.mouse_down(send.x + send.w * 0.5, send.y + send.h * 0.5));
    }

    #[test]
    fn shift_enter_inserts_a_newline_in_the_composer() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.validated_ready = true;
        surface.composer.set_text("one");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::SHIFT,));
        assert_eq!(surface.composer.text(), "one\n");
    }

    #[test]
    fn ctrl_j_inserts_a_newline_in_the_composer() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.validated_ready = true;
        surface.composer.set_text("one");

        assert!(surface.key(&Key::Character("j".into()), None, ModifiersState::CONTROL,));
        assert_eq!(surface.composer.text(), "one\n");
    }

    fn type_into(surface: &mut ChatSurface, text: &str) {
        for ch in text.chars() {
            let s = ch.to_string();
            surface.key(
                &Key::Character(s.as_str().into()),
                Some(&s),
                ModifiersState::empty(),
            );
        }
    }

    /// `@`, the picker answered with one document, Enter: a chip.
    fn chip_into(surface: &mut ChatSurface, title: &str) -> DocumentId {
        let document = DocumentId::new();
        type_into(surface, "@");
        assert!(surface.composer.picker_open());
        surface.composer.offer(&[(document, title)]);
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        document
    }

    fn user_message(transcript: &ChatTranscript, markdown: &str) -> koru_ai_core::Message {
        koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: transcript.chat.id,
            ordinal: transcript.messages.len() as u32,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: markdown.into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(2),
            completed_at: None,
        }
    }

    #[test]
    fn picker_keys_come_before_send_and_the_chip_is_pinned_at_send() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;

        type_into(&mut surface, "Read ");
        let document = chip_into(&mut surface, "Budget");
        assert!(
            commands.try_recv().is_err(),
            "Enter chose the document; it did not send"
        );
        type_into(&mut surface, "now");
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(
                    request.markdown,
                    format!("Read [[koru://doc/{document}]] now")
                );
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: document
                    }]
                );
            }
            _ => panic!("the second Enter sends"),
        }
        assert!(surface.composer.is_blank(), "the composer empties at send");
    }

    #[test]
    fn several_chips_are_several_refs_and_never_more_than_the_worker_takes() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        let context = DocumentId::new();
        surface.set_context_document(Some((context, "Open page".into())));

        let chips: Vec<DocumentId> = (0..koru_ai_wire::MAX_CONTEXT_REFS + 1)
            .map(|i| chip_into(&mut surface, &format!("Doc {i}")))
            .collect();
        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(request.explicit_refs.len(), koru_ai_wire::MAX_CONTEXT_REFS);
                let expected: Vec<ContextRef> = chips
                    .iter()
                    .take(koru_ai_wire::MAX_CONTEXT_REFS)
                    .map(|document_id| ContextRef::Document {
                        document_id: *document_id,
                    })
                    .collect();
                assert_eq!(request.explicit_refs, expected, "chips first, in order");
            }
            _ => panic!("a send"),
        }
        assert!(
            surface
                .status
                .as_deref()
                .is_some_and(|status| status.contains("16")),
            "the person is told which went: {:?}",
            surface.status
        );
    }

    #[test]
    fn in_the_pane_an_earlier_chip_stays_pinned() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let earlier = DocumentId::new();
        let mut transcript = transcript();
        let message = user_message(&transcript, &format!("see [[koru://doc/{earlier}]]"));
        transcript.messages.push(message);
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.composer.set_text("and now?");

        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert!(request.replace_pinned_documents);
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: earlier
                    }]
                );
            }
            _ => panic!("a send"),
        }
    }

    #[test]
    fn send_waits_for_a_provisional_dictation() {
        use koru_ui::DictationTarget;
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.composer.set_text("Hello ");
        assert!(surface.dictation_composer().begin_composition(()));
        surface
            .dictation_composer()
            .update_composition((), "still talking");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        assert!(
            commands.try_recv().is_err(),
            "nothing is sent mid-dictation"
        );
        assert_eq!(surface.composer.text(), "Hello ");

        surface
            .dictation_composer()
            .commit_composition((), "still talking");
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.markdown, "Hello still talking"),
            _ => panic!("a send once the words are text"),
        }
    }

    #[test]
    fn the_sent_message_reads_the_chip_as_a_titled_link() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.lang = koru_i18n::Lang::En;

        type_into(&mut surface, "Summarise ");
        let document = chip_into(&mut surface, "Budget 2026");
        assert!(surface.send_composer());
        let _ = commands.recv_timeout(Duration::from_millis(50)).unwrap();

        // The just-sent card: the chip's caption, as a link to the document.
        surface.rebuild_messages(400.0);
        let pending = surface
            .message_layouts
            .iter()
            .find(|message| message.role == ChatRole::User)
            .expect("the optimistic card");
        assert_eq!(pending.display, "Summarise Budget 2026");
        assert_eq!(
            &pending.display[pending.links[0].range.clone()],
            "Budget 2026"
        );
        assert_eq!(pending.links[0].url, format!("koru://doc/{document}"));

        // The durable card in another window, which never saw the chip: the
        // fallback word, then the title once the reader finds it.
        let mut other = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _other_updates) = Reader::test_pair();
        other.reader = Some(reader);
        other.lang = koru_i18n::Lang::En;
        let mut transcript = transcript();
        let message = user_message(&transcript, &format!("Summarise [[koru://doc/{document}]]"));
        transcript.messages.push(message);
        other.transcript = Some(transcript);
        other.rebuild_messages(400.0);
        let durable = &other.message_layouts[0];
        assert_eq!(durable.display, "Summarise Document");
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::DocumentTitles(asked) => assert_eq!(asked, vec![document]),
            _ => panic!("the title is asked of the reader thread"),
        }
        assert!(
            other.apply_update(Update::DocumentTitles(vec![(
                document,
                "Budget 2026".into()
            )])) || other.messages_dirty
        );
        other.rebuild_messages(400.0);
        assert_eq!(other.message_layouts[0].display, "Summarise Budget 2026");
        let _ = updates;
    }

    #[test]
    fn a_document_link_asks_the_host_to_open_it_and_a_web_link_does_not() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let document = DocumentId::new();
        assert!(surface.follow_link(&format!("koru://doc/{document}")));
        assert_eq!(
            surface.take_open_document_request(),
            Some(koru_link::KoruLink::Document {
                document: document.as_uuid(),
                node: None
            })
        );
        assert!(surface.follow_link("https://example.com"));
        assert_eq!(surface.take_open_document_request(), None);
    }

    #[test]
    fn plain_enter_still_sends_the_composer() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.composer.set_text("hello");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.markdown, "hello"),
            _ => panic!("composer Enter must issue a turn"),
        }
    }

    #[test]
    fn pending_mutation_opens_and_exposes_approve_through_the_shared_reader() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let activity = ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: MessageId::new(),
            tool_name: "edit_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(2),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            display_target: None,
            live: None,
        };
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.upsert_tool_activity(activity.clone());
        surface.validated_ready = true;
        let area = surface.conversation_rect();
        surface.rebuild_messages(area.w);
        assert_eq!(surface.expanded_tool_activity, Some(activity.id));
        // The row sits under its turn's box heading, inset as a member.
        let placed = surface.tool_activity_layouts[0].top;
        assert!(placed >= tool_groups::GROUP_H);
        let row = Rect::new(
            area.x + tool_groups::ROW_INSET,
            area.y + placed,
            area.w - tool_groups::ROW_INSET,
            surface.tool_activity_height(activity.id),
        );
        surface.mouse_move(row.x + 4.0, row.y + 4.0);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default);
        let approve = tool_activity_action_rects(row)[0];

        assert!(surface.mouse_down(approve.x + 2.0, approve.y + 2.0));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ApproveTool(id) if id == activity.id
        ));

        let mut completed = activity;
        completed.status = ToolActivityStatus::Complete;
        completed.approval = ToolApproval::Approved;
        completed.started_at = Some(koru_ai_core::Timestamp::from_millis(2));
        completed.completed_at = Some(koru_ai_core::Timestamp::from_millis(2_252));
        assert_eq!(tool_activity_title(&completed), "Edit document - 2.3s");
        completed.completed_at = Some(koru_ai_core::Timestamp::from_millis(10_252));
        assert_eq!(tool_activity_title(&completed), "Edit document - 10s");

        completed.status = ToolActivityStatus::Failed;
        completed.display_target = Some("Launch plan".into());
        completed.error_message = Some("The requested node no longer exists.".into());
        assert_eq!(
            tool_activity_title(&completed),
            "Edit document — Launch plan - 10s"
        );
        let detail = tool_activity_detail(&completed);
        assert!(detail.contains("Document: Launch plan"));
        assert!(detail.contains("Failed: The requested node no longer exists."));
    }

    #[test]
    fn running_activity_keeps_a_previously_resolved_document_title() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let transcript = transcript();
        let mut activity = ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: MessageId::new(),
            tool_name: "edit_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(1),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            display_target: Some("Launch plan".into()),
            live: None,
        };
        surface.transcript = Some(transcript);
        surface.upsert_tool_activity(activity.clone());

        activity.status = ToolActivityStatus::Running;
        activity.approval = ToolApproval::Approved;
        activity.started_at = Some(koru_ai_core::Timestamp::from_millis(2));
        activity.display_target = None;
        surface.upsert_tool_activity(activity);

        assert_eq!(
            surface.transcript.as_ref().unwrap().tool_activities[0]
                .display_target
                .as_deref(),
            Some("Launch plan")
        );

        let mut failed = surface.transcript.as_ref().unwrap().tool_activities[0].clone();
        failed.status = ToolActivityStatus::Failed;
        failed.completed_at = Some(koru_ai_core::Timestamp::from_millis(3));
        failed.error_message = Some("The requested node no longer exists.".into());
        surface.expanded_tool_activity = None;
        surface.upsert_tool_activity(failed);
        assert_eq!(surface.expanded_tool_activity, None);
    }

    #[test]
    fn tool_activity_is_positioned_inside_its_turn() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        let user_id = MessageId::new();
        let assistant_id = MessageId::new();
        transcript.messages = vec![
            koru_ai_core::Message {
                id: user_id,
                chat_id: transcript.chat.id,
                ordinal: 0,
                role: ChatRole::User,
                state: MessageState::Complete,
                markdown: "Which workspaces can I use?".into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(1)),
            },
            koru_ai_core::Message {
                id: assistant_id,
                chat_id: transcript.chat.id,
                ordinal: 1,
                role: ChatRole::Assistant,
                state: MessageState::Complete,
                markdown: "One workspace is available.".into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(2),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(5)),
            },
        ];
        transcript.tool_activities.push(ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: assistant_id,
            tool_name: "list_workspaces".into(),
            read_only: true,
            status: ToolActivityStatus::Complete,
            approval: ToolApproval::NotRequired,
            // Placement follows the durable message id, even when timestamps
            // are insufficient or misleading.
            proposed_at: koru_ai_core::Timestamp::from_millis(0),
            started_at: Some(koru_ai_core::Timestamp::from_millis(0)),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(0)),
            result_bytes: Some(17),
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            display_target: None,
            live: None,
        });
        surface.transcript = Some(transcript);
        let conversation_width = surface.conversation_rect().w;
        surface.rebuild_messages(conversation_width);

        let user = surface
            .message_layouts
            .iter()
            .find(|message| message.id == Some(user_id))
            .unwrap();
        let assistant = surface
            .message_layouts
            .iter()
            .find(|message| message.id == Some(assistant_id))
            .unwrap();
        let tool = &surface.tool_activity_layouts[0];
        assert!(user.top < tool.top);
        assert!(tool.top < assistant.top);
    }

    #[test]
    fn live_tool_activity_precedes_the_pending_response() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        let assistant_id = MessageId::new();
        transcript.tool_activities.push(ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: assistant_id,
            tool_name: "list_workspaces".into(),
            read_only: true,
            status: ToolActivityStatus::Complete,
            approval: ToolApproval::NotRequired,
            proposed_at: koru_ai_core::Timestamp::from_millis(1),
            started_at: Some(koru_ai_core::Timestamp::from_millis(1)),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
            result_bytes: Some(17),
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            display_target: None,
            live: None,
        });
        surface.transcript = Some(transcript);
        surface.pending_user = Some("What can I access?".into());
        surface.pending_assistant_message_id = Some(assistant_id);
        let conversation_width = surface.conversation_rect().w;
        surface.rebuild_messages(conversation_width);

        let pending = surface
            .message_layouts
            .iter()
            .find(|message| message.id == Some(assistant_id))
            .unwrap();
        assert!(surface.tool_activity_layouts[0].top < pending.top);
    }

    #[test]
    fn session_auto_approval_approves_current_and_future_mutations() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat = transcript.chat.id;
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        assert!(surface.toggle_session_auto_approve());
        assert!(surface.session_auto_approve_chats.contains(&chat));

        let activity = ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: chat,
            assistant_message_id: MessageId::new(),
            tool_name: "create_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(1),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            display_target: None,
            live: None,
        };
        surface.upsert_tool_activity(activity.clone());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ApproveTool(id) if id == activity.id
        ));
        surface.upsert_tool_activity(activity);
        assert!(commands.recv_timeout(Duration::from_millis(10)).is_err());
    }

    #[test]
    fn reasoning_choices_are_capability_driven() {
        use koru_ai_core::ReasoningLevel;

        assert!(choices_for_reasoning(&ReasoningControl::Unsupported).is_empty());
        assert_eq!(
            choices_for_reasoning(&ReasoningControl::Toggle { default_on: true }),
            vec![
                ReasoningChoice::Default,
                ReasoningChoice::Off,
                ReasoningChoice::On,
            ]
        );
        assert_eq!(
            choices_for_reasoning(&ReasoningControl::Levels {
                values: vec![ReasoningLevel::Low, ReasoningLevel::High],
                default: ReasoningLevel::Low,
                supports_off: true,
            }),
            vec![
                ReasoningChoice::Default,
                ReasoningChoice::Off,
                ReasoningChoice::Level(ReasoningLevel::Low),
                ReasoningChoice::Level(ReasoningLevel::High),
            ]
        );
    }

    #[test]
    fn provider_readiness_needs_the_provider_specific_prerequisites() {
        let openai = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse("openai").unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: Some("gpt-5.1".into()),
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        assert!(provider_ready(&openai));
        let mut missing_key = openai;
        missing_key.openai.credential_configured = false;
        assert!(!provider_ready(&missing_key));

        let koru = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::KORU).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        assert!(provider_ready(&koru));
        assert_eq!(selected_setup_provider(&koru), SetupProvider::Koru);
    }

    #[test]
    fn an_empty_agent_chat_offers_its_folder_and_the_host_answers_it() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.chat.current_model.requested_model = AGENT_DEFAULT_MODEL.into();
        let chat = transcript.chat.id;
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.composer_rect();

        let button = surface
            .folder_button()
            .expect("an agent chat with no message offers its folder");
        assert_eq!(surface.folder_label(), "Home folder");
        assert!(surface.mouse_down(button.x + 2.0, button.y + 2.0));
        let request = surface.take_working_folder_request().unwrap();
        assert_eq!(request.chat, chat);
        assert!(request.start.is_none());
        assert!(surface.take_working_folder_request().is_none());

        assert!(surface.working_folder_chosen(chat, std::path::PathBuf::from("D:/work/koru")));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateFolder(id, request) => {
                assert_eq!(id, chat);
                assert_eq!(request.folder.as_deref(), Some("D:/work/koru"));
            }
            _ => panic!("expected the folder update command"),
        }

        let mut chosen = surface.transcript.clone().unwrap().chat;
        chosen.agent_working_folder = Some("D:/work/koru".into());
        surface.apply_update(Update::FolderUpdated(chosen));
        assert_eq!(surface.folder_label(), "koru");

        // With a message on the chat the chooser is gone: the agent started there.
        let transcript = surface.transcript.as_mut().unwrap();
        transcript.messages.push(koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: chat,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "hello".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(2),
            completed_at: None,
        });
        // With a message on the chat the choice is closed — the agent started
        // there — but the chosen folder stays named, and a press asks nothing.
        assert!(!surface.folder_editable());
        // The composer is at the bottom now that the chat has begun.
        surface.composer_rect();
        let button = surface
            .folder_button()
            .expect("a chosen folder stays named after the first message");
        assert_eq!(surface.folder_label(), "koru");
        surface.mouse_down(button.x + 2.0, button.y + 2.0);
        assert!(surface.take_working_folder_request().is_none());
        // Left in the home folder, a begun chat names none.
        surface
            .transcript
            .as_mut()
            .unwrap()
            .chat
            .agent_working_folder = None;
        assert!(surface.folder_button().is_none());
    }

    /// **An agent chat's folder sits under the composer, not in it.** The
    /// quiet row is left-aligned with the box and below it, inside the
    /// window in both of the composer's places; a press there asks for a
    /// folder and a press in the box does not; the toolbar closes up where
    /// the folder used to be. A chat with no folder to show gives the row
    /// no room.
    #[test]
    fn the_folder_sits_under_the_composer_left_aligned() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.chat.current_model.requested_model = AGENT_DEFAULT_MODEL.into();
        let chat = transcript.chat.id;
        surface.transcript = Some(transcript);
        surface.validated_ready = true;

        // Centred: the greeting, the box and the row are one block.
        let composer = surface.composer_rect();
        let folder = surface
            .folder_button()
            .expect("an empty agent chat offers its folder");
        let row = surface.composer_footer_rect().expect("the row is shown");
        assert_eq!(folder.x, composer.x);
        assert_eq!(row.x, composer.x);
        assert_eq!(folder.y, composer.y + composer.h + COMPOSER_FOOTER_GAP);
        assert!(folder.w < composer.w);
        let above = role::title().metrics.line_height + GREETING_GAP + COMPOSER_GAP;
        let middle = (composer.y - above + folder.y + folder.h) * 0.5;
        assert!((middle - surface.viewport.h * 0.5).abs() < 1.0);
        let (fx, fy) = (folder.x + folder.w * 0.5, folder.y + folder.h * 0.5);
        assert!(!composer.contains(fx, fy), "the folder is outside the box");

        // The toolbar no longer holds it: the mode stands next to the chip.
        let mode = surface.tool_mode_button();
        let model = surface.chat_model_button();
        assert!((mode.x + mode.w + 6.0 - model.x).abs() < 0.01);
        assert!(!mode.contains(fx, fy) && !model.contains(fx, fy));

        // Hit where it is drawn, and only there.
        assert_eq!(surface.control_hover_at(fx, fy), Some((5, 12)));
        let text = surface.composer_text_rect();
        surface.mouse_down(text.x + 40.0, text.y + text.h * 0.5);
        assert!(surface.take_working_folder_request().is_none());
        assert!(surface.mouse_down(fx, fy));
        assert_eq!(surface.take_working_folder_request().unwrap().chat, chat);

        // At the bottom, the row stands on the window's padding.
        surface.pending_user = Some("hello".to_owned());
        let composer = surface.composer_rect();
        let folder = surface.folder_button().expect("still choosable");
        assert_eq!(folder.x, composer.x);
        assert_eq!(folder.y + folder.h, surface.viewport.h - PAD);
        let conversation = surface.conversation_rect();
        assert!(conversation.y + conversation.h < composer.y);

        // Begun in the home folder: no row, and no room kept for it.
        surface.pending_user = None;
        let message = user_message(surface.transcript.as_ref().unwrap(), "hello");
        surface.transcript.as_mut().unwrap().messages.push(message);
        let composer = surface.composer_rect();
        assert!(surface.composer_footer_rect().is_none());
        assert!(surface.folder_button().is_none());
        assert_eq!(composer.y + composer.h, surface.viewport.h - PAD);
    }

    #[test]
    fn a_running_agent_check_is_followed_up_while_the_picker_shows_it() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.settings = Some(ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OPENAI).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: vec![koru_ai_wire::AgentProviderView {
                id: AiProviderId::parse("agent-claude").unwrap(),
                label: "Claude".into(),
            }],
        });
        let claude = AiProviderId::parse("agent-claude").unwrap();
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::Checking);
        let start = Instant::now();

        // Nothing shows the agents: nothing is asked.
        surface.follow_agent_checks(start + Duration::from_secs(5));
        assert!(commands.try_recv().is_err());

        // The picker shows them and a check is running: asked, then not
        // again until the follow-up interval has passed.
        surface.chat_model_open = true;
        surface.follow_agent_checks(start + Duration::from_secs(5));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Validate(provider) if provider == claude
        ));
        surface.follow_agent_checks(Instant::now());
        assert!(commands.try_recv().is_err());

        // A settled check stops the follow-up.
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::CheckFailed);
        surface.follow_agent_checks(Instant::now() + Duration::from_secs(5));
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn koru_card_selects_without_creating_a_chat_and_save_selects_the_provider() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.lang = koru_i18n::Lang::En;
        let cards = surface.provider_cards();
        assert_eq!(cards.len(), 4);
        assert!(surface.setup_mouse_down(cards[0].x + 8.0, cards[0].y + 8.0));
        assert_eq!(surface.setup_provider, SetupProvider::Koru);
        assert!(surface.transcript.is_none());
        assert!(commands.recv_timeout(Duration::from_millis(10)).is_err());

        let save = surface.setup_save_button();
        assert!(surface.setup_mouse_down(save.x + 4.0, save.y + 4.0));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::SelectProvider(request) => {
                assert_eq!(request.provider.as_str(), AiProviderId::KORU);
            }
            _ => panic!("expected SelectProvider"),
        }
        assert!(surface.transcript.is_none());
        assert!(surface.web_search_button().is_none());

        surface.settings = Some(ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::KORU).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        });
        let selection = surface.current_selection().expect("koru selection");
        assert_eq!(selection.provider.as_str(), AiProviderId::KORU);
        assert_eq!(selection.requested_model, AiProviderId::KORU_HOSTED_MODEL);
    }

    #[test]
    fn the_chip_reads_the_effort_and_a_stop_sets_the_chats_default() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.lang = koru_i18n::Lang::En;
        let mut chat = transcript();
        chat.chat.current_model.requested_model = "gpt-5.5".into();
        chat.reasoning_control = Some(ReasoningControl::Levels {
            values: vec![
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
            ],
            default: ReasoningLevel::Medium,
            supports_off: false,
        });
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer_rect();
        assert_eq!(surface.chip_label(), "gpt-5.5 · Medium");

        let chip = surface.chat_model_button();
        assert!(surface.mouse_down(chip.x + 2.0, chip.y + 2.0));
        let section = surface
            .effort_section()
            .expect("a model with levels has a slider");
        let control = surface.effort_control().cloned().unwrap();
        let slider = effort::slider(section, &control);
        let (high_x, _) = slider.stops[2];
        assert!(surface.mouse_down(high_x, slider.track_y));
        assert_eq!(
            surface.effort_shown(),
            Some(ReasoningChoice::Level(ReasoningLevel::High))
        );
        assert!(surface.mouse_up());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateModel(id, request) => {
                assert_eq!(id, chat_id);
                assert_eq!(request.model.requested_model, "gpt-5.5");
                assert_eq!(
                    request.model.default_reasoning,
                    ReasoningChoice::Level(ReasoningLevel::High)
                );
            }
            _ => panic!("expected the model update"),
        }
        let mut updated = surface.transcript.clone().unwrap().chat;
        updated.current_model.default_reasoning = ReasoningChoice::Level(ReasoningLevel::High);
        surface.apply_update(Update::ModelUpdated(updated, Some(control)));
        assert_eq!(surface.chip_label(), "gpt-5.5 · High");
        assert!(!surface.chat_model_open);

        // Sending carries no per-turn override: the chat's setting is the effort.
        surface.composer.set_text("hello");
        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.reasoning_override, None),
            _ => panic!("expected send"),
        }
    }

    #[test]
    fn a_koru_chat_folds_its_thinking_toggle_into_the_chip() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.lang = koru_i18n::Lang::En;
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse(AiProviderId::KORU).unwrap();
        chat.chat.current_model.requested_model = AiProviderId::KORU_HOSTED_MODEL.into();
        chat.reasoning_control = Some(ReasoningControl::Toggle { default_on: true });
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer_rect();
        assert_eq!(
            surface.chip_label(),
            format!("{} · On", AiProviderId::KORU_HOSTED_MODEL_LABEL)
        );
        assert!(surface.web_search_button().is_none());

        let chip = surface.chat_model_button();
        assert!(surface.mouse_down(chip.x + 2.0, chip.y + 2.0));
        assert!(
            surface.chat_model_open,
            "the chip opens even with no model list"
        );
        let section = surface.effort_section().unwrap();
        let slider = effort::slider(section, &ReasoningControl::Toggle { default_on: true });
        let (off_x, _) = slider.stops[0];
        assert!(surface.mouse_down(off_x, slider.track_y));
        assert!(surface.mouse_up());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateModel(_, request) => {
                assert_eq!(request.model.default_reasoning, ReasoningChoice::Off);
            }
            _ => panic!("expected the model update"),
        }
    }

    #[test]
    fn selected_ollama_catalog_refresh_revokes_and_restores_readiness() {
        let settings = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OLLAMA).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: Some("qwen3.5:4b".into()),
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.apply_settings_fields(&settings);
        surface.settings = Some(settings);
        surface.setup_provider = SetupProvider::Ollama;
        surface.setup_open = true;
        surface.validated_ready = true;
        surface.transcript = Some(transcript());

        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::NoModels,
            models: Vec::new(),
            selected_model: None,
            failure: None,
        }));
        assert!(!surface.validated_ready);
        assert!(surface.setup_open);
        assert!(surface.validation_retryable());
        assert!(surface.ollama_models.is_empty());

        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::Ready,
            models: vec!["qwen3.5:4b".into(), "gemma4:e4b".into()],
            selected_model: None,
            failure: None,
        }));
        assert!(surface.validated_ready);
        assert!(!surface.setup_open);
        assert_eq!(
            surface.ollama_models,
            ["qwen3.5:4b".to_owned(), "gemma4:e4b".to_owned()]
        );
    }

    #[test]
    fn previewing_another_provider_does_not_replace_selected_readiness() {
        let settings = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OPENAI).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: Some("qwen3.5:4b".into()),
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.apply_settings_fields(&settings);
        surface.settings = Some(settings);
        surface.setup_provider = SetupProvider::Ollama;
        surface.setup_open = true;
        surface.validated_ready = true;

        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::Ready,
            models: vec!["qwen3.5:4b".into()],
            selected_model: None,
            failure: None,
        }));

        assert!(surface.validated_ready);
        assert!(surface.setup_open);
        assert_eq!(
            surface.status.as_deref(),
            Some("Provider ready. Save & check to use it.")
        );
    }

    #[test]
    fn ollama_pull_can_cancel_before_first_progress_and_retry_after_terminal() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface
            .ollama_model
            .set_text(&mut surface.engine, "qwen3.5:4b");

        assert!(surface.pull_ollama_model());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Pull(PullOllamaModelRequest { model }) if model == "qwen3.5:4b"
        ));
        assert!(surface.pull_active);
        assert!(surface.waiting);
        assert_eq!(surface.pull_status.as_deref(), Some("Starting qwen3.5:4b…"));

        assert!(surface.pull_ollama_model());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::CancelPull
        ));
        assert_eq!(surface.pull_status.as_deref(), Some("Stopping model pull…"));

        surface.apply_update(Update::Pull(ModelPullEvent::Cancelled {
            model: "qwen3.5:4b".into(),
        }));
        assert!(!surface.pull_active);
        assert!(!surface.waiting);
        assert_eq!(
            surface.pull_status.as_deref(),
            Some("Stopped pulling qwen3.5:4b")
        );

        assert!(surface.pull_ollama_model());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Pull(PullOllamaModelRequest { model }) if model == "qwen3.5:4b"
        ));
        assert!(surface.pull_active);
    }

    #[test]
    fn openai_discovery_prefers_a_recent_text_model() {
        let models = vec![
            "text-embedding-3-large".into(),
            "gpt-5.2-realtime".into(),
            "gpt-5.1".into(),
            "gpt-5.6".into(),
        ];
        assert_eq!(preferred_openai_model(&models), Some("gpt-5.6"));
        assert!(!likely_openai_text_model("gpt-5.2-realtime"));
        assert!(!likely_openai_text_model("text-embedding-3-large"));
    }

    #[test]
    fn openai_picker_does_not_hide_gpt_models_behind_o_models() {
        let mut models = vec![
            "o4-mini".to_owned(),
            "o3".to_owned(),
            "gpt-5.5".to_owned(),
            "gpt-5.6".to_owned(),
            "gpt-4.1".to_owned(),
        ];
        sort_model_options(&mut models);
        assert_eq!(&models[..3], ["gpt-5.6", "gpt-5.5", "gpt-4.1"]);
    }

    /// **The title row is gone** in both hosts: nothing is drawn or hit at
    /// the top of a workspace pane, whose conversation starts a margin below
    /// the top, and the standalone window keeps only its header buttons.
    #[test]
    fn the_chat_title_is_neither_drawn_nor_hit() {
        for mode in [SurfaceMode::ContextPane, SurfaceMode::Standalone] {
            let mut surface = ChatSurface::new(mode);
            let mut chat = transcript();
            chat.messages.push(user_message(&chat, "hello"));
            surface.transcript = Some(chat);
            surface.validated_ready = true;
            surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
            let top = surface.viewport.y + 7.0 + BUTTON_H * 0.5;
            // Where the title's words were: the middle of the top row.
            let x = surface.viewport.x + surface.viewport.w * 0.5;
            assert!(!surface.mouse_double_click(x, top), "{mode:?}");
            assert_eq!(surface.control_hover_at(x, top), None, "{mode:?}");
            assert_eq!(surface.focus, Focus::Composer, "{mode:?}");
        }
        let mut pane = ChatSurface::new(SurfaceMode::ContextPane);
        pane.transcript = Some(transcript());
        assert_eq!(pane.conversation_rect().y, pane.viewport.y + PAD);
        let mut window = ChatSurface::new(SurfaceMode::Standalone);
        window.transcript = Some(transcript());
        window.validated_ready = true;
        assert_eq!(window.conversation_rect().y, window.viewport.y + HEADER_H);
        let history = window.history_button();
        assert_eq!(
            window.control_hover_at(history.x + 4.0, history.y + 4.0),
            Some((5, 1)),
            "the standalone header keeps Chats"
        );
    }

    #[test]
    fn private_chat_toggle_uses_the_bounded_client_and_updates_visible_state() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat_id = transcript.chat.id;
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.validated_ready = true;

        surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
        let privacy = surface
            .privacy_button()
            .expect("the private mark is on the composer's toolbar");
        assert!(surface.composer_hit_rect.contains(privacy.x, privacy.y));
        assert!(
            !surface.mouse_double_click(privacy.x + privacy.w * 0.5, privacy.y + privacy.h * 0.5,)
        );
        assert!(surface.mouse_down(privacy.x + privacy.w * 0.5, privacy.y + privacy.h * 0.5,));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdatePrivacy(actual, request) => {
                assert_eq!(actual, chat_id);
                assert!(request.private);
            }
            _ => panic!("privacy toggle must use the background API client"),
        }

        let mut chat = surface.transcript.as_ref().unwrap().chat.clone();
        chat.private = true;
        assert!(surface.apply_update(Update::PrivacyUpdated(chat)));
        assert!(surface.transcript.as_ref().unwrap().chat.private);
        assert!(surface.chats[0].private);
    }

    #[test]
    fn desktop_controls_keep_the_arrow_while_editable_text_uses_the_ibeam() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;

        surface.composer_rect();
        let send = surface.send_button();
        surface.mouse_move(send.x + 4.0, send.y + send.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default);

        let text = surface.composer_text_rect();
        surface.mouse_move(text.x + 8.0, text.y + text.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Text);

        surface.setup_open = true;
        let clear = surface.clear_history_button();
        surface.mouse_move(clear.x + 4.0, clear.y + clear.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default);
        let model = surface.field_rects()[1];
        surface.mouse_move(model.x + 8.0, model.y + model.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Text);
    }

    #[test]
    fn usage_command_is_local_and_does_not_create_a_turn() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat_id = transcript.chat.id;
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.validated_ready = true;

        assert!(surface.key(
            &Key::Character("/".into()),
            Some(" /usage "),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));

        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Usage(actual) => assert_eq!(actual, chat_id),
            _ => panic!("/usage must not create a provider turn"),
        }
        assert!(surface.composer.is_blank());
        assert!(surface.pending_user.is_none());
        assert!(surface.staged_embedded_send.is_none());
    }

    #[test]
    fn destructive_confirmation_defaults_to_cancel() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.open_confirmation(DestructiveAction::ClearHistory);

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert!(commands.try_recv().is_err());

        surface.open_confirmation(DestructiveAction::ClearHistory);
        assert!(surface.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(),));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ClearHistory
        ));
    }

    #[test]
    fn usage_copy_distinguishes_unavailable_measurements_from_zero() {
        let chat_id = ChatId::new();
        let report = UsageReport {
            chat_id: Some(chat_id),
            current_chat: Some(UsageTotals {
                attempts: 1,
                context_payload_bytes: 0,
                ..UsageTotals::default()
            }),
            groups: Vec::new(),
        };

        let text = format_usage_report(&report);
        assert!(text.starts_with("Usage — private and local\n"));
        assert!(text.contains("\n\nCurrent chat\n"));
        assert!(text.contains("request unavailable B"));
        assert!(text.contains("Monetary cost is not estimated."));
        assert!(!text.contains('$'));
    }

    #[test]
    fn generated_image_blocks_accept_the_documented_fenced_shape() {
        let markdown = r#"```json
{
  "kind": "generated_image",
  "url": "https://cdn.example.com/generated/image.png",
  "alt": "A generated Koru image."
}
```"#;
        let image = generated_image_spec(markdown).expect("generated image block");
        assert_eq!(image.url, "https://cdn.example.com/generated/image.png");
        assert_eq!(image.alt, "A generated Koru image.");
    }

    #[test]
    fn non_web_image_and_link_targets_are_refused() {
        assert!(!is_web_url("file:///C:/secret.png"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(is_web_url("https://example.com/image.png"));
        assert!(open_web_link("https://example.com/source").is_ok());
    }

    #[test]
    fn the_composer_accepts_a_provisional_multiline_dictation() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        use koru_ui::DictationTarget;
        let composer = surface.dictation_composer();
        assert!(composer.begin_composition(()));
        composer.update_composition((), "Draft a response");
        assert_eq!(composer.text(), "", "the partial is provisional");
        composer.commit_composition((), "Draft a response\nwith sources");
        assert_eq!(composer.text(), "Draft a response\nwith sources");
    }

    fn press(surface: &mut ChatSurface, key: NamedKey) -> bool {
        surface.key(&Key::Named(key), None, ModifiersState::empty())
    }

    /// **Escape twice stops the work in progress**; once only says how. The
    /// second press has to come inside the window, and a popover open over
    /// the composer takes the press first.
    #[test]
    fn escape_twice_stops_the_turn_and_once_only_says_how() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        let attempt = InferenceAttemptId::new();
        surface.attempt = Some(attempt);
        let hint = koru_i18n::t(surface.lang, koru_i18n::Key::AiPressEscAgainToStop);

        assert!(press(&mut surface, NamedKey::Escape));
        assert!(
            commands.try_recv().is_err(),
            "one Escape never stops a turn"
        );
        assert_eq!(surface.footer_line(), Some(hint), "the hint says how");
        assert!(surface.maintenance_deadline().is_some(), "and will go away");
        assert!(press(&mut surface, NamedKey::Escape));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Cancel(cancelled) => assert_eq!(cancelled, attempt),
            _ => panic!("the second Escape cancels the attempt"),
        }
        assert_ne!(surface.footer_line(), Some(hint));

        // Past the window the hint goes, and the next Escape starts over.
        assert!(press(&mut surface, NamedKey::Escape));
        surface.stop_armed_at =
            Instant::now().checked_sub(STOP_ESCAPE_WINDOW + Duration::from_millis(10));
        assert!(
            surface.maintain(Instant::now()),
            "the hint's going is a frame"
        );
        assert!(surface.stop_armed_at.is_none());
        assert_ne!(surface.footer_line(), Some(hint));
        assert!(press(&mut surface, NamedKey::Escape));
        assert!(
            commands.try_recv().is_err(),
            "a late second press only arms"
        );
        assert_eq!(surface.footer_line(), Some(hint));

        // An open popover takes the press first.
        surface.stop_armed_at = None;
        surface.tool_mode_open = true;
        assert!(press(&mut surface, NamedKey::Escape));
        assert!(!surface.tool_mode_open);
        assert!(surface.stop_armed_at.is_none());

        // With no turn running, Escape is not the stop's.
        surface.attempt = None;
        press(&mut surface, NamedKey::Escape);
        press(&mut surface, NamedKey::Escape);
        assert!(surface.stop_armed_at.is_none());
        assert!(commands.try_recv().is_err());
    }

    /// **Up walks back through what was asked, Down walks forward**, and
    /// past the newest gives back the message being typed. The source is the
    /// chat's own messages and what this surface sent since; the same
    /// message twice in a row is one step.
    #[test]
    fn up_and_down_walk_the_messages_asked_and_give_the_draft_back() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        for markdown in ["first", "second", "second"] {
            let message = user_message(&chat, markdown);
            chat.messages.push(message);
        }
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        type_into(&mut surface, "third");
        assert!(press(&mut surface, NamedKey::Enter));
        assert!(surface.composer.is_blank(), "sent");
        surface.pending_user = None;
        type_into(&mut surface, "draft");

        let walk = |surface: &mut ChatSurface, key: NamedKey| {
            press(surface, key);
            surface.composer.text()
        };
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "third",
            "sent, not read back yet"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowUp), "second");
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "first",
            "one step for a repeat"
        );
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "first",
            "the oldest holds"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "second");
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "third");
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowDown),
            "draft",
            "the draft comes back"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "draft");
        assert!(surface.history_cursor.is_none());

        // A recalled message is edited at its end.
        walk(&mut surface, NamedKey::ArrowUp);
        type_into(&mut surface, "!");
        assert_eq!(surface.composer.text(), "third!");

        // Inside a longer message the arrows move the caret.
        surface.forget_input_walk();
        surface.composer.set_text("line one\nline two");
        assert_eq!(walk(&mut surface, NamedKey::ArrowUp), "line one\nline two");
        assert!(
            surface.history_cursor.is_none(),
            "the caret moved up a line"
        );
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "third",
            "from the first line, Up recalls"
        );

        // The `@` picker's keys come first.
        surface.forget_input_walk();
        surface.composer.clear();
        type_into(&mut surface, "@");
        assert!(surface.composer.picker_open());
        press(&mut surface, NamedKey::ArrowUp);
        assert!(surface.history_cursor.is_none());
        assert_eq!(surface.composer.text(), "@");
    }

    /// **An empty chat greets the person**, with one line picked when the
    /// chat starts and held while it shows; a message ends it, and another
    /// chat picks again.
    #[test]
    fn an_empty_chat_greets_once_and_holds_the_line() {
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        surface.validated_ready = true;
        let line = surface.greeting().expect("a chat being made greets");
        for _ in 0..4 {
            surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
            assert_eq!(surface.greeting(), Some(line), "the line holds still");
        }
        surface.transcript = Some(transcript());
        assert_eq!(
            surface.greeting(),
            Some(line),
            "the chat it waited for keeps it"
        );

        surface.pending_user = Some("hello".into());
        assert_eq!(surface.greeting(), None, "a message on its way ends it");
        surface.pending_user = None;
        let message = user_message(surface.transcript.as_ref().unwrap(), "hello");
        surface.transcript.as_mut().unwrap().messages.push(message);
        assert_eq!(surface.greeting(), None);

        let other = transcript();
        let other_id = other.chat.id;
        surface.transcript = Some(other);
        assert!(surface.greeting().is_some());
        assert_eq!(surface.greeting.map(|(chat, _)| chat), Some(Some(other_id)));

        let mut loading = ChatSurface::new(SurfaceMode::ContextPane);
        loading.open_link(ChatId::new(), None);
        assert_eq!(
            loading.greeting(),
            None,
            "a chat still loading is not empty"
        );
    }

    /// **The composer starts in the middle, then moves to the bottom.** While
    /// the chat is empty the greeting and the composer are one centred block;
    /// once a message is on its way the composer sits at the bottom. The send
    /// button, the toolbar, the text and the conversation follow it, inside
    /// the composer's padding, in both places.
    #[test]
    fn the_composer_starts_centred_and_moves_to_the_bottom() {
        for mode in [SurfaceMode::ContextPane, SurfaceMode::Standalone] {
            let mut surface = ChatSurface::new(mode);
            surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
            surface.transcript = Some(transcript());
            surface.validated_ready = true;

            let centred = surface.composer_rect();
            let bottom = surface.viewport.h - PAD - centred.h;
            let above = role::title().metrics.line_height + GREETING_GAP + COMPOSER_GAP;
            let middle = (centred.y - above + centred.y + centred.h) * 0.5;
            assert!((middle - surface.viewport.h * 0.5).abs() < 1.0, "{mode:?}");
            assert!(centred.y < bottom - 100.0, "{mode:?}");
            for begun in [false, true] {
                surface.pending_user = begun.then(|| "hello".to_owned());
                let composer = surface.composer_rect();
                let send = surface.send_button();
                let toolbar = surface.composer_toolbar_rect();
                let text = surface.composer_text_rect();
                assert_eq!(text.y, composer.y);
                assert_eq!(toolbar.x, composer.x + COMPOSER_PAD_X);
                assert_eq!(send.x + send.w, composer.x + composer.w - COMPOSER_PAD_X);
                assert_eq!(
                    toolbar.y + toolbar.h,
                    composer.y + composer.h - COMPOSER_PAD_BOTTOM
                );
                assert_eq!(
                    send.y + send.h,
                    composer.y + composer.h - COMPOSER_PAD_BOTTOM
                );
                assert!(text.y + text.h <= toolbar.y);
                let conversation = surface.conversation_rect();
                assert!(conversation.y + conversation.h < composer.y);
            }
            assert_eq!(surface.composer_rect().y, bottom, "{mode:?} at the bottom");

            // Pressed where it is drawn: the text in the middle takes focus.
            surface.pending_user = None;
            let text = {
                surface.composer_rect();
                surface.composer_text_rect()
            };
            surface.focus = Focus::None;
            assert!(surface.mouse_down(text.x + 40.0, text.y + text.h * 0.5));
            assert_eq!(surface.focus, Focus::Composer, "{mode:?}");
        }
    }

    /// **The composer is one component in the inspector**, its box, text,
    /// toolbar and folder row parts under it, and every primitive it paints
    /// — the box's fill and stroke among them — inside one of those.
    #[test]
    fn the_composer_is_one_component_with_its_parts_inside() {
        use koru_draw::inspect;
        let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.chat.current_model.requested_model = AGENT_DEFAULT_MODEL.into();
        transcript.messages.clear();
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.setup_open = false;
        let root = Affine::IDENTITY;
        let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 900.0, 600.0));
        surface.draw(&mut Scene::new(), root);
        let records = capture.finish().records;
        let composer = records
            .iter()
            .position(|r| r.name == "KoruComposer")
            .expect("the composer is a component");
        let under = |mut i: usize| {
            while let Some(parent) = records[i].parent {
                if parent == composer {
                    return true;
                }
                i = parent;
            }
            false
        };
        for part in [
            "ComposerBox",
            "ComposerText",
            "ComposerToolbar",
            "ComposerFolderRow",
        ] {
            let i = records
                .iter()
                .position(|r| r.name == part)
                .unwrap_or_else(|| panic!("{part} is drawn"));
            assert_eq!(
                records[i].parent,
                Some(composer),
                "{part} is the composer's part"
            );
        }
        let the_box = records
            .iter()
            .position(|r| r.name == "ComposerBox")
            .unwrap();
        let primitives: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.parent == Some(the_box))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !primitives.is_empty(),
            "the box's fill and stroke are inside it"
        );
        assert!(primitives.into_iter().all(under));
    }
}
