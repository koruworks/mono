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
    Available {
        version: Option<String>,
    },
    Missing,
    SignedOut {
        code: ErrorCode,
    },
    /// The managed route refused the agent. When the refusal was its
    /// version, `found` is the version the agent printed (absent when none
    /// could be read) and `needed` what the route accepts, so a person is
    /// told both rather than "not supported". Both are optional and skipped
    /// when absent, so a peer that predates them reads and writes the same
    /// `{"kind":"incompatible","code":…}` it always did.
    Incompatible {
        code: ErrorCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        found: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        needed: Option<VersionNeed>,
    },
    Failed {
        code: ErrorCode,
    },
}

impl ExecutableAvailability {
    /// A refusal that says nothing about versions: the shape every
    /// `Incompatible` had before `found` and `needed`.
    pub fn incompatible(code: ErrorCode) -> Self {
        Self::Incompatible {
            code,
            found: None,
            needed: None,
        }
    }
}

/// **What a managed route accepts of an agent's version**, said when it
/// refused one. The two policies read differently to a person: under a
/// floor, updating fixes it; under a reviewed list, a release newer than
/// the newest reviewed is refused until it has been reviewed too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "version", rename_all = "snake_case")]
pub enum VersionNeed {
    /// This release or any newer one.
    AtLeast(String),
    /// Only the releases on a reviewed list, of which this is the newest.
    Reviewed(String),
}

impl VersionNeed {
    /// A reviewed list's need: its newest entry, whatever order the list is
    /// in. `None` for an empty list.
    pub fn reviewed<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        versions
            .into_iter()
            .reduce(|newest, version| {
                if version_at_least(version, newest) {
                    version
                } else {
                    newest
                }
            })
            .map(|newest| Self::Reviewed(newest.to_owned()))
    }

    /// Whether `found` is newer than everything a reviewed list holds —
    /// the case where updating cannot help and only a review can. Never
    /// for a floor.
    pub fn outgrown_by(&self, found: &str) -> bool {
        match self {
            Self::AtLeast(_) => false,
            Self::Reviewed(newest) => {
                version_at_least(found, newest) && !version_at_least(newest, found)
            }
        }
    }
}

/// `candidate >= floor` as dotted numbers: each component's leading digits,
/// missing components as zero, so `1.0.34 >= 1.0.25`, `2.0 >= 2.0.0` and
/// `0.76.1-beta >= 0.76.0`. A candidate with no number in front is not at
/// least anything. Here rather than in the session runtime because both the
/// runtime's gates and a window wording their refusal compare versions, and
/// they must compare them the same way.
pub fn version_at_least(candidate: &str, floor: &str) -> bool {
    fn component(part: &str) -> Option<u64> {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }
    let Some(Some(_)) = candidate.split('.').map(component).next() else {
        return false;
    };
    let mut a = candidate.split('.').map(component);
    let mut b = floor.split('.').map(component);
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (x, y) => {
                let x = x.flatten().unwrap_or(0);
                let y = y.flatten().unwrap_or(0);
                if x != y {
                    return x > y;
                }
            }
        }
    }
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
