// SPDX-License-Identifier: MPL-2.0

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BuiltInProfileKey, CapabilitySet, ErrorCode, IconName, ProviderKind, SessionProfileId,
};

/// Stable identity for a launch profile. Built-ins remain readable across
/// machines; user-created profiles are opaque UUIDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SessionProfileKey {
    BuiltIn(BuiltInProfileKey),
    Custom(SessionProfileId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Shell,
    Agent,
    Custom,
}

/// The parser that will consume a quoted attachment path. This is profile
/// data, never inferred from an executable name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellQuoting {
    Posix,
    #[serde(rename = "powershell")]
    PowerShell,
}

/// One explicit attachment-delivery declaration for a launch profile.
/// Native delivery is only effective after the managed adapter separately
/// advertises support for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentDeliveryPolicy {
    NativeStructured,
    QuotedPath {
        shell: ShellQuoting,
    },
    #[default]
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileDisplay {
    pub name: String,
    pub icon: IconName,
}

/// An executable and argv elements. No command line is ever reparsed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutableSpec {
    pub program: String,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkingDirectoryPolicy {
    Inherit,
    Home,
    Explicit { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutableAvailability {
    Checking,
    Available { version: Option<String> },
    Missing,
    SignedOut { code: ErrorCode },
    Incompatible { code: ErrorCode },
    Failed { code: ErrorCode },
}

/// Declared input shapes. Native structured delivery still requires a live
/// provider handshake and a negotiated capability.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InputCapabilities {
    pub text: bool,
    pub raw_bytes: bool,
    pub terminal_keys: bool,
    pub paste: bool,
    pub files: bool,
    pub images: bool,
}

/// Complete launch definition, held by the session authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionProfile {
    pub id: SessionProfileKey,
    pub kind: ProfileKind,
    pub display: ProfileDisplay,
    pub executable: ExecutableSpec,
    pub working_directory: WorkingDirectoryPolicy,
    pub input: InputCapabilities,
    #[serde(default)]
    pub attachment_delivery: AttachmentDeliveryPolicy,
    #[serde(default)]
    pub declared_capabilities: CapabilitySet,
    pub provider: Option<ProviderKind>,
    pub availability: ExecutableAvailability,
}

/// Safe list-row projection. It cannot expose argv, environment, or paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionProfileSummary {
    pub id: SessionProfileKey,
    pub kind: ProfileKind,
    pub display: ProfileDisplay,
    pub input: InputCapabilities,
    #[serde(default)]
    pub attachment_delivery: AttachmentDeliveryPolicy,
    pub provider: Option<ProviderKind>,
    pub availability: ExecutableAvailability,
}

/// Append-only built-in profile keys. Availability remains platform-specific.
pub mod built_in_profile {
    pub const PLATFORM_SHELL: &str = "platform-shell";
    pub const POWERSHELL: &str = "powershell";
    pub const BASH: &str = "bash";
    pub const CLAUDE: &str = "claude";
    pub const OPENCODE: &str = "opencode";
    pub const CODEX: &str = "codex";
    pub const GROK: &str = "grok";
    pub const KIMI: &str = "kimi";
    pub const CODEX_CLI: &str = "codex-cli";
    pub const COPILOT: &str = "copilot";
    pub const GEMINI: &str = "gemini";
    pub const QWEN: &str = "qwen";
    /// Google's Antigravity CLI (`agy`) as a raw profile, and its official
    /// ACP server as the managed route (AGENTUX I9). Never the retired
    /// `gemini` key.
    pub const ANTIGRAVITY: &str = "antigravity";
    /// Meta's Muse Code CLI (`muse`) as a raw profile. No managed route yet.
    pub const MUSE: &str = "muse";
    /// Cognition's Devin CLI (`devin`) as a raw profile, and `devin acp` as
    /// the managed route.
    pub const DEVIN: &str = "devin";

    pub const ALL: &[&str] = &[
        PLATFORM_SHELL,
        POWERSHELL,
        BASH,
        CLAUDE,
        OPENCODE,
        CODEX,
        GROK,
        KIMI,
        CODEX_CLI,
        COPILOT,
        GEMINI,
        QWEN,
        ANTIGRAVITY,
        MUSE,
        DEVIN,
    ];
}
