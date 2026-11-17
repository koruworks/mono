// SPDX-License-Identifier: MPL-2.0

//! The closed list of messages, their encoding, and the decoding of what
//! comes back.
//!
//! **A message that is not a [`Command`] cannot be sent.** There is no way
//! to encode a method by name, and no command carries free text except the
//! one address a navigation opens. The one script that can be evaluated is a
//! constant of this crate ([`ConstantScript`]).
//!
//! **Decoding reduces.** A frame is read twice: once for its head (a reply's
//! number or an event's name), once for the few fields Koru uses. Request
//! headers, request bodies, dialog text and every other field are skipped
//! without being built into a value. An address becomes its site
//! ([`AddressSite`]) inside the decode function, with one exception kept
//! only in memory: the address of the top frame, which is the current page.
//! A decode failure carries a listed message name and a length, and nothing
//! of the frame.
//!
//! Every name and field here is "verify at P7.1" (assumptions 2, 9, 10, 17).
//! The codec tests use hand-written frames: they show what Koru sends, and
//! nothing about what a browser answers.

use std::fmt;

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::installed::BrowserVersion;
use crate::origin::{AddressSite, PageAddress};

// --------------------------------------------------------------------- ids

/// The longest identifier a browser may hand back.
pub const MAX_ID_BYTES: usize = 128;

fn plain_token(text: &str, limit: usize) -> bool {
    (1..=limit).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_graphic())
}

macro_rules! token_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
        #[serde(try_from = "String")]
        pub struct $name(String);

        impl $name {
            /// 1 to 128 printable ASCII bytes, no space.
            pub fn new(text: &str) -> Option<Self> {
                plain_token(text, MAX_ID_BYTES).then(|| Self(text.to_owned()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = &'static str;

            fn try_from(text: String) -> Result<Self, Self::Error> {
                if plain_token(&text, MAX_ID_BYTES) {
                    Ok(Self(text))
                } else {
                    Err("identifier")
                }
            }
        }
    };
}

token_id!(
    /// One tab.
    TargetId
);
token_id!(
    /// One attachment to a tab.
    SessionId
);
token_id!(
    /// One frame of a page.
    FrameId
);
token_id!(
    /// One paused request.
    RequestId
);

/// The number a command is sent with and its reply comes back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(pub u64);

/// One script world of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContextId(pub i64);

// ------------------------------------------------------------------ script

/// A script that may be evaluated. It can only be one of this crate's own
/// constants: there is no constructor, so text from a model or a page can
/// never become one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ConstantScript {
    name: &'static str,
    text: &'static str,
}

impl ConstantScript {
    /// **Provisional**, for the qualification's step A5. It returns the
    /// title, the top document's visible text and its anchors' text and
    /// addresses, each bounded. It does not read a form field's value, a
    /// cookie, storage or a frame. The script Koru ships is written with the
    /// session, against what this one is seen to return.
    pub const READ: Self = Self {
        name: "read",
        text: "(() => { const all = document.querySelectorAll('a[href]'); const links = []; \
               for (const a of all) { if (links.length >= 200) break; \
               links.push({ text: (a.innerText || '').slice(0, 200), address: a.href }); } \
               const text = document.body ? document.body.innerText : ''; \
               return { title: (document.title || '').slice(0, 300), \
               text: text.slice(0, 65536), total: text.length, links: links, \
               more_links: all.length > links.length }; })()",
    };

    pub fn name(self) -> &'static str {
        self.name
    }

    pub fn text(self) -> &'static str {
        self.text
    }
}

impl fmt::Debug for ConstantScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConstantScript({})", self.name)
    }
}

// ---------------------------------------------------------------- commands

/// Every message Koru can send, by kind. The list is section 4.4 of the
/// contract and a test holds it to that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandKind {
    GetVersion,
    SetDiscoverTargets,
    GetTargets,
    AttachToTarget,
    CreateTarget,
    CloseTarget,
    EnablePage,
    SetLifecycleEvents,
    Navigate,
    StopLoading,
    DismissDialog,
    EnableInterception,
    DisableInterception,
    ContinueRequest,
    FailRequest,
    CreateIsolatedWorld,
    Evaluate,
    DenyDownloads,
    BringToFront,
    Close,
}

impl CommandKind {
    pub const ALL: [Self; 20] = [
        Self::GetVersion,
        Self::SetDiscoverTargets,
        Self::GetTargets,
        Self::AttachToTarget,
        Self::CreateTarget,
        Self::CloseTarget,
        Self::EnablePage,
        Self::SetLifecycleEvents,
        Self::Navigate,
        Self::StopLoading,
        Self::DismissDialog,
        Self::EnableInterception,
        Self::DisableInterception,
        Self::ContinueRequest,
        Self::FailRequest,
        Self::CreateIsolatedWorld,
        Self::Evaluate,
        Self::DenyDownloads,
        Self::BringToFront,
        Self::Close,
    ];

    /// The name on the wire.
    pub const fn name(self) -> &'static str {
        match self {
            Self::GetVersion => "Browser.getVersion",
            Self::SetDiscoverTargets => "Target.setDiscoverTargets",
            Self::GetTargets => "Target.getTargets",
            Self::AttachToTarget => "Target.attachToTarget",
            Self::CreateTarget => "Target.createTarget",
            Self::CloseTarget => "Target.closeTarget",
            Self::EnablePage => "Page.enable",
            Self::SetLifecycleEvents => "Page.setLifecycleEventsEnabled",
            Self::Navigate => "Page.navigate",
            Self::StopLoading => "Page.stopLoading",
            Self::DismissDialog => "Page.handleJavaScriptDialog",
            Self::EnableInterception => "Fetch.enable",
            Self::DisableInterception => "Fetch.disable",
            Self::ContinueRequest => "Fetch.continueRequest",
            Self::FailRequest => "Fetch.failRequest",
            Self::CreateIsolatedWorld => "Page.createIsolatedWorld",
            Self::Evaluate => "Runtime.evaluate",
            Self::DenyDownloads => "Browser.setDownloadBehavior",
            Self::BringToFront => "Page.bringToFront",
            Self::Close => "Browser.close",
        }
    }
}

/// One message to send. Each variant holds only what Koru decides; every
/// other parameter is fixed in [`Command::encode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The browser's version, for the floor.
    GetVersion,
    /// Be told when a tab opens or closes.
    SetDiscoverTargets {
        discover: bool,
    },
    GetTargets,
    /// Attach to one tab, in the flat mode where its messages carry a
    /// session.
    AttachToTarget {
        target: TargetId,
    },
    /// A new tab on the empty page. It takes no address.
    CreateTarget,
    CloseTarget {
        target: TargetId,
    },
    EnablePage,
    SetLifecycleEvents {
        enabled: bool,
    },
    /// Open one address: a starting address, or a link of the current
    /// observation. The only free text any command carries.
    Navigate {
        address: PageAddress,
    },
    StopLoading,
    /// Dismiss a dialog. Never accepts one and never types into one.
    DismissDialog,
    /// Pause every document request before it is sent.
    EnableInterception,
    DisableInterception,
    /// Let a paused request go, unchanged.
    ContinueRequest {
        request: RequestId,
    },
    /// Refuse a paused request.
    FailRequest {
        request: RequestId,
    },
    /// A script world of Koru's own in one frame.
    CreateIsolatedWorld {
        frame: FrameId,
    },
    /// Evaluate one of this crate's constant scripts, by value.
    Evaluate {
        script: ConstantScript,
        context: ContextId,
    },
    /// Refuse every download.
    DenyDownloads,
    BringToFront,
    /// Ask the browser to close.
    Close,
}

/// The name of the isolated world. Fixed.
const WORLD_NAME: &str = "koru-read";

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

impl Command {
    pub const fn kind(&self) -> CommandKind {
        match self {
            Self::GetVersion => CommandKind::GetVersion,
            Self::SetDiscoverTargets { .. } => CommandKind::SetDiscoverTargets,
            Self::GetTargets => CommandKind::GetTargets,
            Self::AttachToTarget { .. } => CommandKind::AttachToTarget,
            Self::CreateTarget => CommandKind::CreateTarget,
            Self::CloseTarget { .. } => CommandKind::CloseTarget,
            Self::EnablePage => CommandKind::EnablePage,
            Self::SetLifecycleEvents { .. } => CommandKind::SetLifecycleEvents,
            Self::Navigate { .. } => CommandKind::Navigate,
            Self::StopLoading => CommandKind::StopLoading,
            Self::DismissDialog => CommandKind::DismissDialog,
            Self::EnableInterception => CommandKind::EnableInterception,
            Self::DisableInterception => CommandKind::DisableInterception,
            Self::ContinueRequest { .. } => CommandKind::ContinueRequest,
            Self::FailRequest { .. } => CommandKind::FailRequest,
            Self::CreateIsolatedWorld { .. } => CommandKind::CreateIsolatedWorld,
            Self::Evaluate { .. } => CommandKind::Evaluate,
            Self::DenyDownloads => CommandKind::DenyDownloads,
            Self::BringToFront => CommandKind::BringToFront,
            Self::Close => CommandKind::Close,
        }
    }

    /// The parameters as JSON text, or `None` for a command that has none.
    fn parameters(&self) -> Option<String> {
        Some(match self {
            Self::GetVersion
            | Self::GetTargets
            | Self::EnablePage
            | Self::StopLoading
            | Self::DisableInterception
            | Self::BringToFront
            | Self::Close => return None,
            Self::SetDiscoverTargets { discover } => format!("{{\"discover\":{discover}}}"),
            Self::AttachToTarget { target } => {
                format!(
                    "{{\"targetId\":{},\"flatten\":true}}",
                    quoted(target.as_str())
                )
            }
            Self::CreateTarget => "{\"url\":\"about:blank\"}".to_owned(),
            Self::CloseTarget { target } => {
                format!("{{\"targetId\":{}}}", quoted(target.as_str()))
            }
            Self::SetLifecycleEvents { enabled } => format!("{{\"enabled\":{enabled}}}"),
            Self::Navigate { address } => format!("{{\"url\":{}}}", quoted(address.as_str())),
            Self::DismissDialog => "{\"accept\":false}".to_owned(),
            Self::EnableInterception => "{\"patterns\":[{\"urlPattern\":\"*\",\
                 \"resourceType\":\"Document\",\"requestStage\":\"Request\"}]}"
                .to_owned(),
            Self::ContinueRequest { request } => {
                format!("{{\"requestId\":{}}}", quoted(request.as_str()))
            }
            Self::FailRequest { request } => format!(
                "{{\"requestId\":{},\"errorReason\":\"BlockedByClient\"}}",
                quoted(request.as_str())
            ),
            Self::CreateIsolatedWorld { frame } => format!(
                "{{\"frameId\":{},\"worldName\":{}}}",
                quoted(frame.as_str()),
                quoted(WORLD_NAME)
            ),
            Self::Evaluate { script, context } => format!(
                "{{\"expression\":{},\"contextId\":{},\"returnByValue\":true}}",
                quoted(script.text()),
                context.0
            ),
            Self::DenyDownloads => "{\"behavior\":\"deny\"}".to_owned(),
        })
    }

    /// The frame to send. `session` is the tab's attachment for a command
    /// addressed to a page, and `None` for one addressed to the browser.
    pub fn encode(&self, id: CommandId, session: Option<&SessionId>) -> Vec<u8> {
        let mut frame = format!(
            "{{\"id\":{},\"method\":{}",
            id.0,
            quoted(self.kind().name())
        );
        if let Some(parameters) = self.parameters() {
            frame.push_str(",\"params\":");
            frame.push_str(&parameters);
        }
        if let Some(session) = session {
            frame.push_str(",\"sessionId\":");
            frame.push_str(&quoted(session.as_str()));
        }
        frame.push('}');
        frame.into_bytes()
    }

    /// The line a qualification transcript keeps for this command: its
    /// name, its identifiers, and a navigation's address reduced to its
    /// site.
    pub fn record(&self, id: CommandId, session: Option<&SessionId>) -> String {
        let mut line = format!("> {} {}", id.0, self.kind().name());
        match self {
            Self::SetDiscoverTargets { discover } => {
                line.push_str(&format!(" discover={discover}"))
            }
            Self::AttachToTarget { target } | Self::CloseTarget { target } => {
                line.push_str(&format!(" target={}", target.as_str()));
            }
            Self::SetLifecycleEvents { enabled } => line.push_str(&format!(" enabled={enabled}")),
            Self::Navigate { address } => {
                line.push_str(&format!(" site={}", address.site().record()));
            }
            Self::ContinueRequest { request } | Self::FailRequest { request } => {
                line.push_str(&format!(" request={}", request.as_str()));
            }
            Self::CreateIsolatedWorld { frame } => {
                line.push_str(&format!(" frame={}", frame.as_str()));
            }
            Self::Evaluate { script, context } => {
                line.push_str(&format!(" script={} context={}", script.name(), context.0));
            }
            Self::GetVersion
            | Self::GetTargets
            | Self::CreateTarget
            | Self::EnablePage
            | Self::StopLoading
            | Self::DismissDialog
            | Self::EnableInterception
            | Self::DisableInterception
            | Self::DenyDownloads
            | Self::BringToFront
            | Self::Close => {}
        }
        if let Some(session) = session {
            line.push_str(&format!(" session={}", session.as_str()));
        }
        line
    }
}

// ------------------------------------------------------------------ events

/// Every event Koru decodes. Any other event is counted and dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventName {
    TargetCreated,
    TargetDestroyed,
    TargetCrashed,
    FrameNavigated,
    NavigatedWithinDocument,
    Lifecycle,
    DialogOpening,
    RequestPaused,
    InspectorCrashed,
}

impl EventName {
    pub const ALL: [Self; 9] = [
        Self::TargetCreated,
        Self::TargetDestroyed,
        Self::TargetCrashed,
        Self::FrameNavigated,
        Self::NavigatedWithinDocument,
        Self::Lifecycle,
        Self::DialogOpening,
        Self::RequestPaused,
        Self::InspectorCrashed,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::TargetCreated => "Target.targetCreated",
            Self::TargetDestroyed => "Target.targetDestroyed",
            Self::TargetCrashed => "Target.targetCrashed",
            Self::FrameNavigated => "Page.frameNavigated",
            Self::NavigatedWithinDocument => "Page.navigatedWithinDocument",
            Self::Lifecycle => "Page.lifecycleEvent",
            Self::DialogOpening => "Page.javascriptDialogOpening",
            Self::RequestPaused => "Fetch.requestPaused",
            Self::InspectorCrashed => "Inspector.targetCrashed",
        }
    }

    fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.name() == name)
    }
}

/// What kind of thing a tab entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Page,
    Other,
}

/// One tab, as the browser lists it. Its address is reduced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    pub target: TargetId,
    pub kind: TargetKind,
    pub site: AddressSite,
    pub attached: bool,
    /// Whether a page opened it, rather than Koru or the person.
    pub opened_by_page: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStage {
    Init,
    DomContentLoaded,
    Load,
    NetworkAlmostIdle,
    NetworkIdle,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMethod {
    Get,
    Post,
    Other,
}

/// One decoded event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    TargetCreated(TargetInfo),
    TargetDestroyed {
        target: TargetId,
    },
    TargetCrashed {
        target: TargetId,
    },
    /// A frame committed a document. `address` is kept only for the top
    /// frame: it is the current page.
    FrameNavigated {
        frame: FrameId,
        top: bool,
        site: AddressSite,
        address: Option<PageAddress>,
    },
    /// A frame changed its address without loading a document. `address` is
    /// kept only when the frame is the one the scope names as the top frame.
    NavigatedWithinDocument {
        frame: FrameId,
        site: AddressSite,
        address: Option<PageAddress>,
    },
    Lifecycle {
        frame: FrameId,
        stage: LifecycleStage,
    },
    /// A dialog opened. Its text is not read.
    DialogOpening {
        kind: DialogKind,
    },
    /// A request is paused. Its headers and body are not read.
    RequestPaused {
        request: RequestId,
        frame: FrameId,
        site: AddressSite,
        /// Whether it is a document request, top-level or a frame's.
        document: bool,
        method: RequestMethod,
    },
    /// The attached tab crashed.
    InspectorCrashed,
}

impl Event {
    pub const fn name(&self) -> EventName {
        match self {
            Self::TargetCreated(_) => EventName::TargetCreated,
            Self::TargetDestroyed { .. } => EventName::TargetDestroyed,
            Self::TargetCrashed { .. } => EventName::TargetCrashed,
            Self::FrameNavigated { .. } => EventName::FrameNavigated,
            Self::NavigatedWithinDocument { .. } => EventName::NavigatedWithinDocument,
            Self::Lifecycle { .. } => EventName::Lifecycle,
            Self::DialogOpening { .. } => EventName::DialogOpening,
            Self::RequestPaused { .. } => EventName::RequestPaused,
            Self::InspectorCrashed => EventName::InspectorCrashed,
        }
    }

    /// The line a qualification transcript keeps for this event.
    pub fn record(&self) -> String {
        let name = self.name().name();
        match self {
            Self::TargetCreated(info) => format!("! {name} {}", info.record()),
            Self::TargetDestroyed { target } | Self::TargetCrashed { target } => {
                format!("! {name} target={}", target.as_str())
            }
            Self::FrameNavigated {
                frame, top, site, ..
            } => format!(
                "! {name} frame={} top={top} site={}",
                frame.as_str(),
                site.record()
            ),
            Self::NavigatedWithinDocument { frame, site, .. } => {
                format!("! {name} frame={} site={}", frame.as_str(), site.record())
            }
            Self::Lifecycle { frame, stage } => {
                format!("! {name} frame={} stage={stage:?}", frame.as_str())
            }
            Self::DialogOpening { kind } => format!("! {name} kind={kind:?}"),
            Self::RequestPaused {
                request,
                frame,
                site,
                document,
                method,
            } => format!(
                "! {name} request={} frame={} site={} document={document} method={method:?}",
                request.as_str(),
                frame.as_str(),
                site.record()
            ),
            Self::InspectorCrashed => format!("! {name}"),
        }
    }
}

impl TargetInfo {
    fn record(&self) -> String {
        format!(
            "target={} kind={:?} site={} attached={} opened_by_page={}",
            self.target.as_str(),
            self.kind,
            self.site.record(),
            self.attached,
            self.opened_by_page
        )
    }
}

// ----------------------------------------------------------------- replies

/// What the browser says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// The protocol's own version, as digits and dots.
    pub protocol: String,
    /// The product version, when the product text ends in one.
    pub product_version: Option<BrowserVersion>,
    /// The product text, bounded and plain.
    pub product: String,
}

/// How a navigation started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationStart {
    Started,
    /// Koru's own refusal of the request came back.
    Blocked,
    /// It was aborted, as a download or a replaced navigation is.
    Aborted,
    /// Any other error. Its text is not kept.
    Failed,
}

/// A value a constant script returned. It may hold page text and addresses,
/// so its `Debug` shows nothing of it.
#[derive(Clone, PartialEq)]
pub struct PageValue(serde_json::Value);

impl PageValue {
    pub fn json(&self) -> &serde_json::Value {
        &self.0
    }

    pub fn into_json(self) -> serde_json::Value {
        self.0
    }
}

impl fmt::Debug for PageValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PageValue(..)")
    }
}

/// What an evaluation gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Evaluation {
    Value(PageValue),
    /// The script threw, or returned nothing that can be passed by value.
    /// What it threw is not read.
    Threw,
}

/// A decoded reply to one command.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The command was done and its reply holds nothing Koru uses.
    Done,
    Version(VersionInfo),
    Targets(Vec<TargetInfo>),
    Attached {
        session: SessionId,
    },
    Created {
        target: TargetId,
    },
    Navigated {
        frame: FrameId,
        start: NavigationStart,
    },
    World {
        context: ContextId,
    },
    Evaluated(Evaluation),
}

impl Reply {
    /// The line a qualification transcript keeps for this reply.
    pub fn record(&self, id: CommandId) -> String {
        let id = id.0;
        match self {
            Self::Done => format!("< {id} done"),
            Self::Version(info) => format!(
                "< {id} version protocol={} product={:?}",
                info.protocol, info.product
            ),
            Self::Targets(targets) => {
                let mut line = format!("< {id} targets count={}", targets.len());
                for info in targets {
                    line.push_str(&format!(" [{}]", info.record()));
                }
                line
            }
            Self::Attached { session } => format!("< {id} attached session={}", session.as_str()),
            Self::Created { target } => format!("< {id} created target={}", target.as_str()),
            Self::Navigated { frame, start } => {
                format!("< {id} navigated frame={} start={start:?}", frame.as_str())
            }
            Self::World { context } => format!("< {id} world context={}", context.0),
            Self::Evaluated(Evaluation::Value(_)) => format!("< {id} evaluated value"),
            Self::Evaluated(Evaluation::Threw) => format!("< {id} evaluated threw"),
        }
    }
}

/// The browser refused a command. Its message is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub code: i64,
}

// ------------------------------------------------------------------ decode

/// Why a frame could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeReason {
    /// Not JSON, or not an object.
    NotJson,
    /// Neither a reply nor an event.
    NoHead,
    /// The fields Koru reads are missing or of another type.
    Shape,
}

/// A decode failure: which listed message it was, how long the frame was,
/// and a reason token. Never any part of the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeFailure {
    /// A name from the closed lists, or `"frame"` when the frame's head
    /// could not be read.
    pub message: &'static str,
    pub length: usize,
    pub reason: DecodeReason,
}

/// What a frame is, before its body is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameHead {
    /// The reply to the command sent with this number.
    Reply {
        id: CommandId,
        session: Option<SessionId>,
    },
    /// An event Koru decodes.
    Event {
        name: EventName,
        session: Option<SessionId>,
    },
    /// Any other event. Its name is kept only when it is a plain protocol
    /// name, for the qualification's record; its body is never read.
    Unlisted { name: Option<String> },
}

/// What decoding needs to know about the page it is decoding for.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodeScope<'a> {
    /// The attached tab's top frame, once known.
    pub top_frame: Option<&'a FrameId>,
}

#[derive(Deserialize)]
struct RawHead {
    id: Option<u64>,
    method: Option<String>,
    #[serde(rename = "sessionId")]
    session: Option<SessionId>,
}

#[derive(Deserialize)]
struct ReplyOf<T> {
    result: Option<T>,
    error: Option<RawRefusal>,
}

#[derive(Deserialize)]
struct RawRefusal {
    code: i64,
}

#[derive(Deserialize)]
struct EventOf<T> {
    params: T,
}

#[derive(Deserialize)]
struct RawVersion {
    #[serde(rename = "protocolVersion")]
    protocol: String,
    product: String,
}

#[derive(Deserialize)]
struct RawTarget {
    #[serde(rename = "targetId")]
    target: TargetId,
    #[serde(rename = "type")]
    kind: String,
    url: String,
    #[serde(default)]
    attached: bool,
    #[serde(rename = "openerId")]
    opener: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct RawTargets {
    #[serde(rename = "targetInfos")]
    targets: Vec<RawTarget>,
}

#[derive(Deserialize)]
struct RawTargetEvent {
    #[serde(rename = "targetInfo")]
    target: RawTarget,
}

#[derive(Deserialize)]
struct RawTargetRef {
    #[serde(rename = "targetId")]
    target: TargetId,
}

#[derive(Deserialize)]
struct RawAttached {
    #[serde(rename = "sessionId")]
    session: SessionId,
}

#[derive(Deserialize)]
struct RawNavigated {
    #[serde(rename = "frameId")]
    frame: FrameId,
    #[serde(rename = "errorText")]
    error: Option<String>,
}

#[derive(Deserialize)]
struct RawWorld {
    #[serde(rename = "executionContextId")]
    context: i64,
}

#[derive(Deserialize)]
struct RawEvaluated {
    result: RawRemoteObject,
    #[serde(rename = "exceptionDetails")]
    exception: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct RawRemoteObject {
    value: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawFrameEvent {
    frame: RawFrame,
}

#[derive(Deserialize)]
struct RawFrame {
    id: FrameId,
    #[serde(rename = "parentId")]
    parent: Option<IgnoredAny>,
    url: String,
}

#[derive(Deserialize)]
struct RawWithinDocument {
    #[serde(rename = "frameId")]
    frame: FrameId,
    url: String,
}

#[derive(Deserialize)]
struct RawLifecycle {
    #[serde(rename = "frameId")]
    frame: FrameId,
    name: String,
}

#[derive(Deserialize)]
struct RawDialog {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct RawPaused {
    #[serde(rename = "requestId")]
    request: RequestId,
    #[serde(rename = "request")]
    sent: RawRequest,
    #[serde(rename = "frameId")]
    frame: FrameId,
    #[serde(rename = "resourceType")]
    resource: String,
}

/// The two fields of a paused request Koru reads. Its headers, its body and
/// everything else are skipped unread.
#[derive(Deserialize)]
struct RawRequest {
    url: String,
    method: String,
}

impl RawTarget {
    fn reduce(self) -> TargetInfo {
        TargetInfo {
            target: self.target,
            kind: if self.kind == "page" {
                TargetKind::Page
            } else {
                TargetKind::Other
            },
            site: AddressSite::of(&self.url),
            attached: self.attached,
            opened_by_page: self.opener.is_some(),
        }
    }
}

fn plain_text(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(limit)
        .collect()
}

/// Read a frame's head: whether it is a reply, a listed event or anything
/// else. The body is not read here.
pub fn classify(frame: &[u8]) -> Result<FrameHead, DecodeFailure> {
    let failure = |reason| DecodeFailure {
        message: "frame",
        length: frame.len(),
        reason,
    };
    let head: RawHead =
        serde_json::from_slice(frame).map_err(|_| failure(DecodeReason::NotJson))?;
    match (head.id, head.method) {
        (Some(id), None) => Ok(FrameHead::Reply {
            id: CommandId(id),
            session: head.session,
        }),
        (None, Some(method)) => Ok(match EventName::named(&method) {
            Some(name) => FrameHead::Event {
                name,
                session: head.session,
            },
            None => FrameHead::Unlisted {
                name: protocol_name(&method).then_some(method),
            },
        }),
        _ => Err(failure(DecodeReason::NoHead)),
    }
}

/// `Domain.name` in letters only: what an event's name looks like, and
/// nothing a page could put its own text into.
fn protocol_name(name: &str) -> bool {
    name.len() <= 64
        && name.split_once('.').is_some_and(|(domain, event)| {
            !domain.is_empty()
                && !event.is_empty()
                && domain.bytes().all(|b| b.is_ascii_alphabetic())
                && event.bytes().all(|b| b.is_ascii_alphabetic())
        })
}

fn body<'a, T: Deserialize<'a>>(
    frame: &'a [u8],
    message: &'static str,
) -> Result<T, DecodeFailure> {
    serde_json::from_slice(frame).map_err(|_| DecodeFailure {
        message,
        length: frame.len(),
        reason: DecodeReason::Shape,
    })
}

/// Decode a listed event, reducing every address in it.
pub fn decode_event(
    frame: &[u8],
    name: EventName,
    scope: DecodeScope<'_>,
) -> Result<Event, DecodeFailure> {
    let message = name.name();
    Ok(match name {
        EventName::TargetCreated => {
            let raw: EventOf<RawTargetEvent> = body(frame, message)?;
            Event::TargetCreated(raw.params.target.reduce())
        }
        EventName::TargetDestroyed => {
            let raw: EventOf<RawTargetRef> = body(frame, message)?;
            Event::TargetDestroyed {
                target: raw.params.target,
            }
        }
        EventName::TargetCrashed => {
            let raw: EventOf<RawTargetRef> = body(frame, message)?;
            Event::TargetCrashed {
                target: raw.params.target,
            }
        }
        EventName::FrameNavigated => {
            let raw: EventOf<RawFrameEvent> = body(frame, message)?;
            let frame = raw.params.frame;
            let top = frame.parent.is_none();
            Event::FrameNavigated {
                site: AddressSite::of(&frame.url),
                address: top.then(|| PageAddress::reported(&frame.url)).flatten(),
                frame: frame.id,
                top,
            }
        }
        EventName::NavigatedWithinDocument => {
            let raw: EventOf<RawWithinDocument> = body(frame, message)?;
            let top = scope.top_frame == Some(&raw.params.frame);
            Event::NavigatedWithinDocument {
                site: AddressSite::of(&raw.params.url),
                address: top
                    .then(|| PageAddress::reported(&raw.params.url))
                    .flatten(),
                frame: raw.params.frame,
            }
        }
        EventName::Lifecycle => {
            let raw: EventOf<RawLifecycle> = body(frame, message)?;
            Event::Lifecycle {
                frame: raw.params.frame,
                stage: match raw.params.name.as_str() {
                    "init" => LifecycleStage::Init,
                    "DOMContentLoaded" => LifecycleStage::DomContentLoaded,
                    "load" => LifecycleStage::Load,
                    "networkAlmostIdle" => LifecycleStage::NetworkAlmostIdle,
                    "networkIdle" => LifecycleStage::NetworkIdle,
                    _ => LifecycleStage::Other,
                },
            }
        }
        EventName::DialogOpening => {
            let raw: EventOf<RawDialog> = body(frame, message)?;
            Event::DialogOpening {
                kind: match raw.params.kind.as_str() {
                    "alert" => DialogKind::Alert,
                    "confirm" => DialogKind::Confirm,
                    "prompt" => DialogKind::Prompt,
                    "beforeunload" => DialogKind::BeforeUnload,
                    _ => DialogKind::Other,
                },
            }
        }
        EventName::RequestPaused => {
            let raw: EventOf<RawPaused> = body(frame, message)?;
            let paused = raw.params;
            Event::RequestPaused {
                request: paused.request,
                frame: paused.frame,
                site: AddressSite::of(&paused.sent.url),
                document: paused.resource == "Document",
                method: match paused.sent.method.as_str() {
                    "GET" => RequestMethod::Get,
                    "POST" => RequestMethod::Post,
                    _ => RequestMethod::Other,
                },
            }
        }
        EventName::InspectorCrashed => {
            // Nothing in it is read; it only has to be an object.
            let _: IgnoredAny = body(frame, message)?;
            Event::InspectorCrashed
        }
    })
}

/// Decode the reply to a command of `kind`. The outer result is whether the
/// frame could be read at all; the inner one is the browser's own answer.
pub fn decode_reply(
    frame: &[u8],
    kind: CommandKind,
) -> Result<Result<Reply, Refusal>, DecodeFailure> {
    let message = kind.name();
    let shape = || DecodeFailure {
        message,
        length: frame.len(),
        reason: DecodeReason::Shape,
    };
    fn answer<T>(
        raw: ReplyOf<T>,
        shape: impl Fn() -> DecodeFailure,
        reduce: impl FnOnce(T) -> Reply,
    ) -> Result<Result<Reply, Refusal>, DecodeFailure> {
        match (raw.error, raw.result) {
            (Some(refusal), _) => Ok(Err(Refusal { code: refusal.code })),
            (None, Some(result)) => Ok(Ok(reduce(result))),
            (None, None) => Err(shape()),
        }
    }
    match kind {
        CommandKind::GetVersion => {
            answer(body::<ReplyOf<RawVersion>>(frame, message)?, shape, |raw| {
                let product = plain_text(&raw.product, 64);
                Reply::Version(VersionInfo {
                    protocol: raw
                        .protocol
                        .chars()
                        .filter(|c| c.is_ascii_digit() || *c == '.')
                        .take(16)
                        .collect(),
                    product_version: product
                        .rsplit_once('/')
                        .and_then(|(_, version)| BrowserVersion::parse(version)),
                    product,
                })
            })
        }
        CommandKind::GetTargets => {
            answer(body::<ReplyOf<RawTargets>>(frame, message)?, shape, |raw| {
                Reply::Targets(raw.targets.into_iter().map(RawTarget::reduce).collect())
            })
        }
        CommandKind::AttachToTarget => answer(
            body::<ReplyOf<RawAttached>>(frame, message)?,
            shape,
            |raw| Reply::Attached {
                session: raw.session,
            },
        ),
        CommandKind::CreateTarget => answer(
            body::<ReplyOf<RawTargetRef>>(frame, message)?,
            shape,
            |raw| Reply::Created { target: raw.target },
        ),
        CommandKind::Navigate => answer(
            body::<ReplyOf<RawNavigated>>(frame, message)?,
            shape,
            |raw| Reply::Navigated {
                frame: raw.frame,
                start: match raw.error.as_deref() {
                    None => NavigationStart::Started,
                    Some(text) if text.contains("BLOCKED_BY_CLIENT") => NavigationStart::Blocked,
                    Some(text) if text.contains("ERR_ABORTED") => NavigationStart::Aborted,
                    Some(_) => NavigationStart::Failed,
                },
            },
        ),
        CommandKind::CreateIsolatedWorld => {
            answer(body::<ReplyOf<RawWorld>>(frame, message)?, shape, |raw| {
                Reply::World {
                    context: ContextId(raw.context),
                }
            })
        }
        CommandKind::Evaluate => answer(
            body::<ReplyOf<RawEvaluated>>(frame, message)?,
            shape,
            |raw| {
                Reply::Evaluated(match (raw.exception, raw.result.value) {
                    (None, Some(value)) => Evaluation::Value(PageValue(value)),
                    _ => Evaluation::Threw,
                })
            },
        ),
        CommandKind::SetDiscoverTargets
        | CommandKind::CloseTarget
        | CommandKind::EnablePage
        | CommandKind::SetLifecycleEvents
        | CommandKind::StopLoading
        | CommandKind::DismissDialog
        | CommandKind::EnableInterception
        | CommandKind::DisableInterception
        | CommandKind::ContinueRequest
        | CommandKind::FailRequest
        | CommandKind::DenyDownloads
        | CommandKind::BringToFront
        | CommandKind::Close => answer(body::<ReplyOf<IgnoredAny>>(frame, message)?, shape, |_| {
            Reply::Done
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::origin::StartingAddress;

    fn target(text: &str) -> TargetId {
        TargetId::new(text).unwrap()
    }

    fn session() -> SessionId {
        SessionId::new("S1").unwrap()
    }

    fn page(text: &str) -> PageAddress {
        PageAddress::from(&StartingAddress::parse(text).unwrap())
    }

    /// One command of every kind, with the frame it must encode to. Sent to
    /// the browser when `on_page` is false and to the tab's session when it
    /// is true.
    fn every_command() -> Vec<(Command, bool, &'static str)> {
        vec![
            (
                Command::GetVersion,
                false,
                r#"{"id":7,"method":"Browser.getVersion"}"#,
            ),
            (
                Command::SetDiscoverTargets { discover: true },
                false,
                r#"{"id":7,"method":"Target.setDiscoverTargets","params":{"discover":true}}"#,
            ),
            (
                Command::GetTargets,
                false,
                r#"{"id":7,"method":"Target.getTargets"}"#,
            ),
            (
                Command::AttachToTarget {
                    target: target("T1"),
                },
                false,
                r#"{"id":7,"method":"Target.attachToTarget","params":{"targetId":"T1","flatten":true}}"#,
            ),
            (
                Command::CreateTarget,
                false,
                r#"{"id":7,"method":"Target.createTarget","params":{"url":"about:blank"}}"#,
            ),
            (
                Command::CloseTarget {
                    target: target("T2"),
                },
                false,
                r#"{"id":7,"method":"Target.closeTarget","params":{"targetId":"T2"}}"#,
            ),
            (
                Command::EnablePage,
                true,
                r#"{"id":7,"method":"Page.enable","sessionId":"S1"}"#,
            ),
            (
                Command::SetLifecycleEvents { enabled: true },
                true,
                r#"{"id":7,"method":"Page.setLifecycleEventsEnabled","params":{"enabled":true},"sessionId":"S1"}"#,
            ),
            (
                Command::Navigate {
                    address: page("https://example.com/issues?state=open"),
                },
                true,
                r#"{"id":7,"method":"Page.navigate","params":{"url":"https://example.com/issues?state=open"},"sessionId":"S1"}"#,
            ),
            (
                Command::StopLoading,
                true,
                r#"{"id":7,"method":"Page.stopLoading","sessionId":"S1"}"#,
            ),
            (
                Command::DismissDialog,
                true,
                r#"{"id":7,"method":"Page.handleJavaScriptDialog","params":{"accept":false},"sessionId":"S1"}"#,
            ),
            (
                Command::EnableInterception,
                true,
                r#"{"id":7,"method":"Fetch.enable","params":{"patterns":[{"urlPattern":"*","resourceType":"Document","requestStage":"Request"}]},"sessionId":"S1"}"#,
            ),
            (
                Command::DisableInterception,
                true,
                r#"{"id":7,"method":"Fetch.disable","sessionId":"S1"}"#,
            ),
            (
                Command::ContinueRequest {
                    request: RequestId::new("interception-job-1.0").unwrap(),
                },
                true,
                r#"{"id":7,"method":"Fetch.continueRequest","params":{"requestId":"interception-job-1.0"},"sessionId":"S1"}"#,
            ),
            (
                Command::FailRequest {
                    request: RequestId::new("interception-job-2.0").unwrap(),
                },
                true,
                r#"{"id":7,"method":"Fetch.failRequest","params":{"requestId":"interception-job-2.0","errorReason":"BlockedByClient"},"sessionId":"S1"}"#,
            ),
            (
                Command::CreateIsolatedWorld {
                    frame: FrameId::new("F1").unwrap(),
                },
                true,
                r#"{"id":7,"method":"Page.createIsolatedWorld","params":{"frameId":"F1","worldName":"koru-read"},"sessionId":"S1"}"#,
            ),
            (
                Command::Evaluate {
                    script: ConstantScript::READ,
                    context: ContextId(3),
                },
                true,
                // Checked field by field below: the script is long.
                "",
            ),
            (
                Command::DenyDownloads,
                false,
                r#"{"id":7,"method":"Browser.setDownloadBehavior","params":{"behavior":"deny"}}"#,
            ),
            (
                Command::BringToFront,
                true,
                r#"{"id":7,"method":"Page.bringToFront","sessionId":"S1"}"#,
            ),
            (
                Command::Close,
                false,
                r#"{"id":7,"method":"Browser.close"}"#,
            ),
        ]
    }

    fn encoded(command: &Command, on_page: bool) -> String {
        let session = session();
        let frame = command.encode(CommandId(7), on_page.then_some(&session));
        String::from_utf8(frame).unwrap()
    }

    #[test]
    fn m7_protocol_closed_the_list_is_section_4_4() {
        let commands: Vec<&str> = CommandKind::ALL.iter().map(|kind| kind.name()).collect();
        assert_eq!(
            commands,
            [
                // Version and floor.
                "Browser.getVersion",
                // Find and follow the tab.
                "Target.setDiscoverTargets",
                "Target.getTargets",
                "Target.attachToTarget",
                "Target.createTarget",
                "Target.closeTarget",
                // Page lifecycle.
                "Page.enable",
                "Page.setLifecycleEventsEnabled",
                "Page.navigate",
                "Page.stopLoading",
                "Page.handleJavaScriptDialog",
                // Enforce the site.
                "Fetch.enable",
                "Fetch.disable",
                "Fetch.continueRequest",
                "Fetch.failRequest",
                // Read text.
                "Page.createIsolatedWorld",
                "Runtime.evaluate",
                // Refuse downloads.
                "Browser.setDownloadBehavior",
                // Bring forward.
                "Page.bringToFront",
                // End.
                "Browser.close",
            ]
        );
        let events: Vec<&str> = EventName::ALL.iter().map(|event| event.name()).collect();
        assert_eq!(
            events,
            [
                "Target.targetCreated",
                "Target.targetDestroyed",
                "Target.targetCrashed",
                "Page.frameNavigated",
                "Page.navigatedWithinDocument",
                "Page.lifecycleEvent",
                "Page.javascriptDialogOpening",
                "Fetch.requestPaused",
                "Inspector.targetCrashed",
            ]
        );
        // No name twice, and a command is never an event.
        let mut all: Vec<&str> = commands.iter().chain(&events).copied().collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 29);
    }

    #[test]
    fn m7_protocol_closed_only_listed_messages_can_be_encoded() {
        let listed: Vec<&str> = CommandKind::ALL.iter().map(|kind| kind.name()).collect();
        let commands = every_command();
        // One command of every kind: a new variant must be listed to exist.
        let kinds: Vec<CommandKind> = commands.iter().map(|(c, _, _)| c.kind()).collect();
        assert_eq!(kinds, CommandKind::ALL);
        for (command, on_page, _) in &commands {
            let frame: serde_json::Value =
                serde_json::from_str(&encoded(command, *on_page)).unwrap();
            let method = frame["method"].as_str().unwrap();
            assert!(listed.contains(&method), "{method}");
            assert_eq!(method, command.kind().name());
            assert_eq!(frame["id"], 7);
            // Exactly these keys, and no others.
            let keys: Vec<&str> = frame
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            for key in keys {
                assert!(
                    matches!(key, "id" | "method" | "params" | "sessionId"),
                    "{key}"
                );
            }
        }
    }

    #[test]
    fn m7_protocol_closed_no_cookie_input_capture_upload_injection_or_binding() {
        // Whole domains Koru never speaks.
        let domains = [
            "Network.",
            "Storage.",
            "Input.",
            "DOM.",
            "DOMStorage.",
            "IndexedDB.",
            "CacheStorage.",
            "Emulation.",
            "Security.",
            "IO.",
            "Debugger.",
            "Overlay.",
            "Autofill.",
            "WebAuthn.",
            "Extensions.",
            "Tracing.",
        ];
        // And, in the domains it does speak, what it never says.
        let names = [
            "Cookie",
            "captureScreenshot",
            "captureSnapshot",
            "printToPDF",
            "Screencast",
            "FileChooser",
            "setFileInputFiles",
            "addScriptToEvaluateOnNewDocument",
            "addBinding",
            "callFunctionOn",
            "compileScript",
            "runScript",
            "setBypassCSP",
            "setDocumentContent",
            "grantPermissions",
            "setPermission",
            "dispatch",
            "insertText",
            "getResponseBody",
            "fulfillRequest",
            "continueWithAuth",
            "continueResponse",
            "setAutoAttach",
        ];
        let every = every_command();
        for (command, on_page, _) in &every {
            let frame = encoded(command, *on_page);
            for domain in domains {
                assert!(
                    !frame.contains(&format!("\"{domain}")),
                    "{domain} in {frame}"
                );
            }
            for name in names {
                assert!(
                    !command.kind().name().contains(name),
                    "{name} in {}",
                    command.kind().name()
                );
            }
        }
        // A continued request is continued unchanged: nothing can rewrite
        // its address, its method, its headers or its body.
        let continued = encoded(&every[13].0, true);
        for field in [
            "url",
            "method\":\"P",
            "headers",
            "postData",
            "interceptResponse",
        ] {
            assert!(!continued.contains(field), "{field}");
        }
        // Interception is for documents only, before they are sent.
        let enabled: serde_json::Value =
            serde_json::from_str(&encoded(&every[11].0, true)).unwrap();
        assert_eq!(
            enabled["params"]["patterns"],
            serde_json::json!([{"urlPattern": "*", "resourceType": "Document", "requestStage": "Request"}])
        );
        // A dialog is only ever dismissed, and a new tab only ever blank.
        assert!(encoded(&Command::DismissDialog, true).contains("\"accept\":false"));
        assert!(!encoded(&Command::DismissDialog, true).contains("promptText"));
        assert!(encoded(&Command::CreateTarget, false).contains("\"url\":\"about:blank\""));
        assert!(encoded(&Command::DenyDownloads, false).contains("\"behavior\":\"deny\""));
    }

    #[test]
    fn m7_protocol_closed_only_a_constant_script_is_evaluated() {
        let command = Command::Evaluate {
            script: ConstantScript::READ,
            context: ContextId(3),
        };
        let frame: serde_json::Value = serde_json::from_str(&encoded(&command, true)).unwrap();
        assert_eq!(frame["method"], "Runtime.evaluate");
        assert_eq!(frame["sessionId"], "S1");
        let parameters = frame["params"].as_object().unwrap();
        assert_eq!(parameters.len(), 3);
        assert_eq!(parameters["expression"], ConstantScript::READ.text());
        assert_eq!(parameters["contextId"], 3);
        assert_eq!(parameters["returnByValue"], true);
        // The script reads what is drawn and the anchors, and none of what
        // the contract forbids.
        let script = ConstantScript::READ.text();
        for forbidden in [
            "cookie",
            "Storage",
            "indexedDB",
            ".value",
            "password",
            "frames",
            "contentDocument",
            "contentWindow",
            "fetch(",
            "XMLHttpRequest",
            "sendBeacon",
            "eval(",
            "Function(",
            "import(",
            "postMessage",
            "location.",
            "open(",
            "click(",
            "submit(",
        ] {
            assert!(!script.contains(forbidden), "{forbidden}");
        }
        assert!(script.contains("innerText") && script.contains("a[href]"));
        // The script is shown by name, never by text.
        assert_eq!(
            format!("{command:?}"),
            "Evaluate { script: ConstantScript(read), context: ContextId(3) }"
        );
    }

    #[test]
    fn m7_protocol_codec_every_command_encodes_to_its_frame() {
        for (command, on_page, frame) in every_command() {
            if frame.is_empty() {
                continue;
            }
            assert_eq!(encoded(&command, on_page), frame, "{:?}", command.kind());
        }
        // A session is added to any command, and text is escaped as JSON.
        let odd = Command::Navigate {
            address: page("https://example.com/a\"b?q=<\u{e9}>"),
        };
        let frame: serde_json::Value = serde_json::from_str(&encoded(&odd, true)).unwrap();
        assert_eq!(
            frame["params"]["url"],
            "https://example.com/a\"b?q=<\u{e9}>"
        );
        // No frame holds the separator the transport frames with.
        for (command, on_page, _) in every_command() {
            assert!(!encoded(&command, on_page).contains('\0'));
        }
    }

    #[test]
    fn m7_protocol_codec_heads() {
        assert_eq!(
            classify(br#"{"id":7,"result":{}}"#),
            Ok(FrameHead::Reply {
                id: CommandId(7),
                session: None
            })
        );
        assert_eq!(
            classify(br#"{"id":8,"result":{"frameId":"F1"},"sessionId":"S1"}"#),
            Ok(FrameHead::Reply {
                id: CommandId(8),
                session: Some(session())
            })
        );
        assert_eq!(
            classify(br#"{"id":9,"error":{"code":-32000,"message":"No target"}}"#),
            Ok(FrameHead::Reply {
                id: CommandId(9),
                session: None
            })
        );
        for name in EventName::ALL {
            let frame = format!(
                r#"{{"method":"{}","params":{{}},"sessionId":"S1"}}"#,
                name.name()
            );
            assert_eq!(
                classify(frame.as_bytes()),
                Ok(FrameHead::Event {
                    name,
                    session: Some(session())
                })
            );
        }
        // Any other event: its name if it is a plain one, never its body.
        assert_eq!(
            classify(br#"{"method":"Target.attachedToTarget","params":{"sessionId":"S1"}}"#),
            Ok(FrameHead::Unlisted {
                name: Some("Target.attachedToTarget".into())
            })
        );
        assert_eq!(
            classify(br#"{"method":"https://secret.example/?t=MARK","params":{}}"#),
            Ok(FrameHead::Unlisted { name: None })
        );
        // Not a frame.
        let failed = |frame: &[u8], reason| {
            assert_eq!(
                classify(frame),
                Err(DecodeFailure {
                    message: "frame",
                    length: frame.len(),
                    reason
                })
            );
        };
        failed(b"", DecodeReason::NotJson);
        failed(b"not json", DecodeReason::NotJson);
        failed(b"[1,2]", DecodeReason::NotJson);
        failed(br#"{"id":"7"}"#, DecodeReason::NotJson);
        failed(br#"{"id":7"#, DecodeReason::NotJson);
        failed(br#"{}"#, DecodeReason::NoHead);
        failed(br#"{"id":7,"method":"Page.enable"}"#, DecodeReason::NoHead);
        failed(br#"{"id":7,"id":8}"#, DecodeReason::NotJson);
    }

    fn reply(frame: &str, kind: CommandKind) -> Reply {
        decode_reply(frame.as_bytes(), kind).unwrap().unwrap()
    }

    #[test]
    fn m7_protocol_codec_every_reply_decodes_from_its_frame() {
        assert_eq!(
            reply(
                r#"{"id":1,"result":{"protocolVersion":"1.3","product":"Browser/131.0.2903.86","revision":"@abc","userAgent":"Mozilla/5.0 (Windows NT 10.0)","jsVersion":"13.1"}}"#,
                CommandKind::GetVersion
            ),
            Reply::Version(VersionInfo {
                protocol: "1.3".into(),
                product_version: Some(BrowserVersion::new(131, 0, 2903, 86)),
                product: "Browser/131.0.2903.86".into(),
            })
        );
        assert_eq!(
            reply(
                r#"{"id":2,"result":{"targetInfos":[{"targetId":"T1","type":"page","title":"Issues","url":"about:blank","attached":false,"canAccessOpener":false,"browserContextId":"B1"},{"targetId":"T2","type":"service_worker","title":"x","url":"https://example.com/sw.js?v=MARK","attached":true,"openerId":"T1"}]}}"#,
                CommandKind::GetTargets
            ),
            Reply::Targets(vec![
                TargetInfo {
                    target: target("T1"),
                    kind: TargetKind::Page,
                    site: AddressSite::of("about:blank"),
                    attached: false,
                    opened_by_page: false,
                },
                TargetInfo {
                    target: target("T2"),
                    kind: TargetKind::Other,
                    site: AddressSite::of("https://example.com"),
                    attached: true,
                    opened_by_page: true,
                },
            ])
        );
        assert_eq!(
            reply(
                r#"{"id":3,"result":{"sessionId":"S1"}}"#,
                CommandKind::AttachToTarget
            ),
            Reply::Attached { session: session() }
        );
        assert_eq!(
            reply(
                r#"{"id":4,"result":{"targetId":"T9"}}"#,
                CommandKind::CreateTarget
            ),
            Reply::Created {
                target: target("T9")
            }
        );
        let frame = FrameId::new("F1").unwrap();
        for (text, start) in [
            (
                r#"{"frameId":"F1","loaderId":"L1"}"#,
                NavigationStart::Started,
            ),
            (
                r#"{"frameId":"F1","errorText":"net::ERR_BLOCKED_BY_CLIENT"}"#,
                NavigationStart::Blocked,
            ),
            (
                r#"{"frameId":"F1","errorText":"net::ERR_ABORTED"}"#,
                NavigationStart::Aborted,
            ),
            (
                r#"{"frameId":"F1","errorText":"net::ERR_NAME_NOT_RESOLVED"}"#,
                NavigationStart::Failed,
            ),
        ] {
            assert_eq!(
                reply(
                    &format!(r#"{{"id":5,"result":{text},"sessionId":"S1"}}"#),
                    CommandKind::Navigate
                ),
                Reply::Navigated {
                    frame: frame.clone(),
                    start
                }
            );
        }
        assert_eq!(
            reply(
                r#"{"id":6,"result":{"executionContextId":12},"sessionId":"S1"}"#,
                CommandKind::CreateIsolatedWorld
            ),
            Reply::World {
                context: ContextId(12)
            }
        );
        let Reply::Evaluated(Evaluation::Value(value)) = reply(
            r#"{"id":7,"result":{"result":{"type":"object","value":{"title":"Issues","text":"One\nTwo","total":7,"links":[{"text":"One","address":"https://example.com/issues/1"}],"more_links":false}}},"sessionId":"S1"}"#,
            CommandKind::Evaluate,
        ) else {
            panic!("a value")
        };
        assert_eq!(value.json()["title"], "Issues");
        assert_eq!(
            value.json()["links"][0]["address"],
            "https://example.com/issues/1"
        );
        for threw in [
            r#"{"id":7,"result":{"result":{"type":"object","subtype":"error"},"exceptionDetails":{"text":"Uncaught","exception":{"description":"MARK"}}}}"#,
            r#"{"id":7,"result":{"result":{"type":"undefined"}}}"#,
            r#"{"id":7,"result":{"result":{"type":"object","value":{"a":1}},"exceptionDetails":{}}}"#,
        ] {
            assert_eq!(
                reply(threw, CommandKind::Evaluate),
                Reply::Evaluated(Evaluation::Threw)
            );
        }
        // Every command with nothing to read back.
        for kind in CommandKind::ALL {
            let empty = matches!(
                kind,
                CommandKind::SetDiscoverTargets
                    | CommandKind::CloseTarget
                    | CommandKind::EnablePage
                    | CommandKind::SetLifecycleEvents
                    | CommandKind::StopLoading
                    | CommandKind::DismissDialog
                    | CommandKind::EnableInterception
                    | CommandKind::DisableInterception
                    | CommandKind::ContinueRequest
                    | CommandKind::FailRequest
                    | CommandKind::DenyDownloads
                    | CommandKind::BringToFront
                    | CommandKind::Close
            );
            let decoded = decode_reply(br#"{"id":8,"result":{}}"#, kind);
            if empty {
                assert_eq!(decoded, Ok(Ok(Reply::Done)), "{kind:?}");
                assert_eq!(
                    decode_reply(br#"{"id":8,"result":{"success":true}}"#, kind),
                    Ok(Ok(Reply::Done))
                );
            } else {
                assert_eq!(
                    decoded,
                    Err(DecodeFailure {
                        message: kind.name(),
                        length: 20,
                        reason: DecodeReason::Shape
                    }),
                    "{kind:?}"
                );
            }
            // A refusal is the browser's answer, whatever the command.
            assert_eq!(
                decode_reply(
                    br#"{"id":8,"error":{"code":-32601,"message":"'X.y' wasn't found"}}"#,
                    kind
                ),
                Ok(Err(Refusal { code: -32601 })),
                "{kind:?}"
            );
            // Neither a result nor an error is not a reply.
            assert!(decode_reply(br#"{"id":8}"#, kind).is_err(), "{kind:?}");
        }
    }

    fn event(frame: &str) -> Event {
        let FrameHead::Event { name, .. } = classify(frame.as_bytes()).unwrap() else {
            panic!("a listed event")
        };
        decode_event(frame.as_bytes(), name, DecodeScope::default()).unwrap()
    }

    #[test]
    fn m7_protocol_codec_every_event_decodes_from_its_frame() {
        assert_eq!(
            event(
                r#"{"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"T3","type":"page","title":"","url":"http://127.0.0.1:41001/","attached":false,"openerId":"T1","canAccessOpener":true}}}"#
            ),
            Event::TargetCreated(TargetInfo {
                target: target("T3"),
                kind: TargetKind::Page,
                site: AddressSite::of("http://127.0.0.1:41001"),
                attached: false,
                opened_by_page: true,
            })
        );
        assert_eq!(
            event(r#"{"method":"Target.targetDestroyed","params":{"targetId":"T3"}}"#),
            Event::TargetDestroyed {
                target: target("T3")
            }
        );
        assert_eq!(
            event(
                r#"{"method":"Target.targetCrashed","params":{"targetId":"T3","status":"crashed","errorCode":133}}"#
            ),
            Event::TargetCrashed {
                target: target("T3")
            }
        );
        let frame = FrameId::new("F1").unwrap();
        let inner = FrameId::new("F2").unwrap();
        // The top frame keeps its address: it is the current page.
        let Event::FrameNavigated {
            frame: navigated,
            top,
            site,
            address,
        } = event(
            r#"{"method":"Page.frameNavigated","params":{"frame":{"id":"F1","loaderId":"L1","url":"https://example.com/issues?state=open","securityOrigin":"https://example.com","mimeType":"text/html"},"type":"Navigation"},"sessionId":"S1"}"#,
        )
        else {
            panic!("a navigation")
        };
        assert_eq!((navigated, top), (frame.clone(), true));
        assert_eq!(site.record(), "https://example.com");
        assert_eq!(
            address.unwrap().as_str(),
            "https://example.com/issues?state=open"
        );
        // A frame inside it does not.
        assert_eq!(
            event(
                r#"{"method":"Page.frameNavigated","params":{"frame":{"id":"F2","parentId":"F1","loaderId":"L2","url":"https://other.example/embed?t=MARK"}},"sessionId":"S1"}"#
            ),
            Event::FrameNavigated {
                frame: inner.clone(),
                top: false,
                site: AddressSite::of("https://other.example"),
                address: None,
            }
        );
        // A change within the document keeps its address only for the top
        // frame the scope names.
        let within = r#"{"method":"Page.navigatedWithinDocument","params":{"frameId":"F1","url":"https://example.com/issues#top"},"sessionId":"S1"}"#;
        for (top, kept) in [(None, false), (Some(&inner), false), (Some(&frame), true)] {
            let Event::NavigatedWithinDocument { address, site, .. } = decode_event(
                within.as_bytes(),
                EventName::NavigatedWithinDocument,
                DecodeScope { top_frame: top },
            )
            .unwrap() else {
                panic!("a change within the document")
            };
            assert_eq!(address.is_some(), kept);
            assert_eq!(site.record(), "https://example.com");
        }
        for (name, stage) in [
            ("init", LifecycleStage::Init),
            ("DOMContentLoaded", LifecycleStage::DomContentLoaded),
            ("load", LifecycleStage::Load),
            ("networkAlmostIdle", LifecycleStage::NetworkAlmostIdle),
            ("networkIdle", LifecycleStage::NetworkIdle),
            ("firstPaint", LifecycleStage::Other),
        ] {
            assert_eq!(
                event(&format!(
                    r#"{{"method":"Page.lifecycleEvent","params":{{"frameId":"F1","loaderId":"L1","name":"{name}","timestamp":1.5}}}}"#
                )),
                Event::Lifecycle {
                    frame: frame.clone(),
                    stage
                }
            );
        }
        for (name, kind) in [
            ("alert", DialogKind::Alert),
            ("confirm", DialogKind::Confirm),
            ("prompt", DialogKind::Prompt),
            ("beforeunload", DialogKind::BeforeUnload),
            ("other", DialogKind::Other),
        ] {
            assert_eq!(
                event(&format!(
                    r#"{{"method":"Page.javascriptDialogOpening","params":{{"url":"https://example.com/dialog","message":"MARK","type":"{name}","hasBrowserHandler":false,"defaultPrompt":"MARK"}}}}"#
                )),
                Event::DialogOpening { kind }
            );
        }
        assert_eq!(
            event(
                r#"{"method":"Fetch.requestPaused","params":{"requestId":"interception-job-3.0","request":{"url":"http://127.0.0.1:41001/?from=out","method":"GET","headers":{"Accept":"text/html"},"initialPriority":"VeryHigh","referrerPolicy":"strict-origin-when-cross-origin"},"frameId":"F1","resourceType":"Document","networkId":"N1"},"sessionId":"S1"}"#
            ),
            Event::RequestPaused {
                request: RequestId::new("interception-job-3.0").unwrap(),
                frame: frame.clone(),
                site: AddressSite::of("http://127.0.0.1:41001"),
                document: true,
                method: RequestMethod::Get,
            }
        );
        assert_eq!(
            event(r#"{"method":"Inspector.targetCrashed","params":{},"sessionId":"S1"}"#),
            Event::InspectorCrashed
        );
        // Every listed event was decoded above.
        assert_eq!(EventName::ALL.len(), 9);
    }

    #[test]
    fn m7_frame_reduction_headers_and_bodies_are_not_deserialized() {
        // Headers and a body full of marks, and of shapes no typed reader
        // would accept: the event decodes the same, because neither is read.
        let body = "MARK-BODY ".repeat(10_000);
        let frames = [
            format!(
                r#"{{"method":"Fetch.requestPaused","params":{{"requestId":"R1","request":{{"url":"https://example.com/login/code?code=MARK-QUERY","method":"POST","headers":{{"Cookie":"session=MARK-COOKIE","Authorization":"Bearer MARK-TOKEN"}},"postData":"{body}","postDataEntries":[{{"bytes":"TUFSSw=="}}],"hasPostData":true}},"frameId":"F1","resourceType":"Document"}}}}"#
            ),
            r#"{"method":"Fetch.requestPaused","params":{"requestId":"R1","request":{"url":"https://example.com/login/code?code=MARK-QUERY","method":"POST","headers":7,"postData":{"not":["a","string"]}},"frameId":"F1","resourceType":"Document","responseHeaders":"MARK-COOKIE"}}"#.to_owned(),
        ];
        for frame in &frames {
            let decoded = event(frame);
            assert_eq!(
                decoded,
                Event::RequestPaused {
                    request: RequestId::new("R1").unwrap(),
                    frame: FrameId::new("F1").unwrap(),
                    site: AddressSite::of("https://example.com"),
                    document: true,
                    method: RequestMethod::Post,
                }
            );
            for shown in [format!("{decoded:?}"), decoded.record()] {
                assert!(!shown.contains("MARK"), "{shown}");
            }
        }
        // The decoder has no field to put them in.
        let source = include_str!("protocol.rs");
        let (code, _) = source.split_once("\n#[cfg(test)]\nmod tests {").unwrap();
        for field in [
            "headers",
            "postData",
            "responseHeaders",
            "defaultPrompt",
            "title",
        ] {
            assert!(!code.contains(&format!("rename = \"{field}\"")), "{field}");
            assert!(!code.contains(&format!("    {field}:")), "{field}");
        }
    }

    #[test]
    fn m7_frame_reduction_an_address_is_reduced_to_its_site_at_decode() {
        const SECRET: &str =
            "https://signin.example/callback?code=MARK-ONE-TIME&state=MARK-STATE#MARK-FRAGMENT";
        let reduced = [
            event(&format!(
                r#"{{"method":"Target.targetCreated","params":{{"targetInfo":{{"targetId":"T1","type":"page","title":"MARK-TITLE","url":"{SECRET}","attached":false}}}}}}"#
            )),
            event(&format!(
                r#"{{"method":"Page.frameNavigated","params":{{"frame":{{"id":"F2","parentId":"F1","url":"{SECRET}","name":"MARK-NAME"}}}}}}"#
            )),
            event(&format!(
                r#"{{"method":"Page.navigatedWithinDocument","params":{{"frameId":"F1","url":"{SECRET}"}}}}"#
            )),
            event(&format!(
                r#"{{"method":"Fetch.requestPaused","params":{{"requestId":"R1","request":{{"url":"{SECRET}","method":"GET","headers":{{}}}},"frameId":"F1","resourceType":"Document"}}}}"#
            )),
            event(&format!(
                r#"{{"method":"Page.javascriptDialogOpening","params":{{"url":"{SECRET}","message":"MARK-MESSAGE","type":"alert"}}}}"#
            )),
        ];
        for decoded in &reduced {
            // Nothing of the address but its site is held at all.
            match decoded {
                Event::TargetCreated(info) => {
                    assert_eq!(info.site.record(), "https://signin.example")
                }
                Event::FrameNavigated { site, address, .. }
                | Event::NavigatedWithinDocument { site, address, .. } => {
                    assert_eq!(site.record(), "https://signin.example");
                    assert_eq!(*address, None);
                }
                Event::RequestPaused { site, .. } => {
                    assert_eq!(site.record(), "https://signin.example");
                }
                Event::DialogOpening { kind } => assert_eq!(*kind, DialogKind::Alert),
                other => panic!("{other:?}"),
            }
            // `Debug` shows not even the site; the transcript shows only it.
            let debug = format!("{decoded:?}");
            assert!(
                !debug.contains("MARK") && !debug.contains("signin"),
                "{debug}"
            );
            let record = decoded.record();
            assert!(
                !record.contains("MARK") && !record.contains("callback"),
                "{record}"
            );
        }
        // The one address kept is the top frame's, in memory, and neither
        // `Debug` nor the transcript shows more than its site.
        let top = event(&format!(
            r#"{{"method":"Page.frameNavigated","params":{{"frame":{{"id":"F1","url":"{SECRET}"}}}}}}"#
        ));
        let Event::FrameNavigated { address, .. } = &top else {
            panic!("a navigation")
        };
        assert_eq!(address.as_ref().unwrap().as_str(), SECRET);
        assert!(!format!("{top:?}").contains("signin"));
        assert_eq!(
            top.record(),
            "! Page.frameNavigated frame=F1 top=true site=https://signin.example"
        );
        // A reply's addresses are reduced the same way, and a command shows
        // the site it navigates to and nothing after it.
        let targets = reply(
            &format!(
                r#"{{"id":1,"result":{{"targetInfos":[{{"targetId":"T1","type":"page","title":"MARK-TITLE","url":"{SECRET}","attached":true}}]}}}}"#
            ),
            CommandKind::GetTargets,
        );
        assert!(!format!("{targets:?}").contains("MARK"));
        assert!(!targets.record(CommandId(1)).contains("MARK"));
        let navigate = Command::Navigate {
            address: PageAddress::reported(SECRET).unwrap(),
        };
        assert!(!format!("{navigate:?}").contains("signin"));
        assert_eq!(
            navigate.record(CommandId(4), Some(&session())),
            "> 4 Page.navigate site=https://signin.example session=S1"
        );
        // Page text a script returned is never shown either.
        let value = reply(
            r#"{"id":7,"result":{"result":{"type":"object","value":{"text":"MARK-TEXT"}}}}"#,
            CommandKind::Evaluate,
        );
        assert!(!format!("{value:?}").contains("MARK"));
        assert_eq!(value.record(CommandId(7)), "< 7 evaluated value");
    }

    #[test]
    fn m7_frame_reduction_a_decode_error_carries_a_name_and_a_length_only() {
        let frame = r#"{"method":"Fetch.requestPaused","params":{"requestId":"R1","request":{"url":"https://example.com/?t=MARK","method":"GET"},"resourceType":"Document"}}"#;
        let failure = decode_event(
            frame.as_bytes(),
            EventName::RequestPaused,
            DecodeScope::default(),
        )
        .unwrap_err();
        assert_eq!(
            failure,
            DecodeFailure {
                message: "Fetch.requestPaused",
                length: frame.len(),
                reason: DecodeReason::Shape
            }
        );
        assert_eq!(
            format!("{failure:?}"),
            format!(
                "DecodeFailure {{ message: \"Fetch.requestPaused\", length: {}, reason: Shape }}",
                frame.len()
            )
        );
        // An identifier that is not a plain token is a shape error, and is
        // not echoed.
        for bad in [
            r#"{"id":3,"result":{"sessionId":"MARK with a space"}}"#.to_owned(),
            r#"{"id":3,"result":{"sessionId":""}}"#.to_owned(),
            format!(
                r#"{{"id":3,"result":{{"sessionId":"{}"}}}}"#,
                "a".repeat(129)
            ),
            r#"{"id":3,"result":{"sessionId":7}}"#.to_owned(),
        ] {
            let failure = decode_reply(bad.as_bytes(), CommandKind::AttachToTarget).unwrap_err();
            assert_eq!(failure.message, "Target.attachToTarget");
            assert_eq!(failure.length, bad.len());
            assert!(!format!("{failure:?}").contains("MARK"));
        }
        // A refusal keeps its code and drops its message.
        let refused = decode_reply(
            br#"{"id":3,"error":{"code":-32000,"message":"Cannot navigate to https://example.com/?t=MARK"}}"#,
            CommandKind::Navigate,
        );
        assert_eq!(refused, Ok(Err(Refusal { code: -32000 })));
        assert!(!format!("{refused:?}").contains("MARK"));
        // The version's free text is bounded and plain.
        let Reply::Version(info) = reply(
            &format!(
                r#"{{"id":1,"result":{{"protocolVersion":"1.3<script>","product":"Name\n/1.2.3.4{}"}}}}"#,
                "x".repeat(500)
            ),
            CommandKind::GetVersion,
        ) else {
            panic!("a version")
        };
        assert_eq!(info.protocol, "1.3");
        assert_eq!(info.product.len(), 64);
        assert!(!info.product.contains('\n'));
        assert_eq!(info.product_version, None);
    }
}
