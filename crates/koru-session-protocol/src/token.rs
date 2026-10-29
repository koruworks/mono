// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenParseError(pub(crate) &'static str);

impl fmt::Display for TokenParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TokenParseError {}

fn valid_token(value: &str, separator: u8) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == separator)
}

macro_rules! token_type {
    ($(#[$doc:meta])* $name:ident, $separator:literal) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize, JsonSchema)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str { &self.0 }
        }

        impl FromStr for $name {
            type Err = TokenParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                valid_token(value, $separator)
                    .then(|| Self(value.to_owned()))
                    .ok_or(TokenParseError("invalid stable token"))
            }
        }

        impl TryFrom<String> for $name {
            type Error = TokenParseError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self { value.0 }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
        }
    };
}

token_type!(
    /// Additive negotiated capability name (`render-deltas`).
    CapabilityName,
    b'-'
);
token_type!(
    /// Stable built-in launch-profile key (`powershell`).
    BuiltInProfileKey,
    b'-'
);
token_type!(
    /// Provider category, never a provider-native identity.
    ProviderKind,
    b'-'
);
token_type!(
    /// Stable neutral icon token interpreted by a surface.
    IconName,
    b'-'
);

/// Sorted, duplicate-free additive capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CapabilitySet(BTreeSet<CapabilityName>);

impl CapabilitySet {
    pub fn new(names: impl IntoIterator<Item = CapabilityName>) -> Self {
        Self(names.into_iter().collect())
    }

    pub fn contains(&self, name: &CapabilityName) -> bool {
        self.0.contains(name)
    }

    pub fn supports(&self, name: &str) -> bool {
        self.0.iter().any(|capability| capability.as_str() == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &CapabilityName> {
        self.0.iter()
    }

    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).cloned().collect())
    }
}

/// Append-only protocol capability spellings. Types existing in this crate do
/// not imply that a runtime advertises the corresponding capability.
pub mod capability {
    pub const CONTROLLER_LEASES: &str = "controller-leases";
    pub const RENDER_DELTAS: &str = "render-deltas";
    pub const SCROLLBACK_PAGES: &str = "scrollback-pages";
    pub const RAW_WRITE: &str = "raw-write";
    pub const TERMINAL_KEYS: &str = "terminal-keys";
    pub const RESIZE: &str = "resize";
    pub const INTERRUPT: &str = "interrupt";
    pub const TERMINATE: &str = "terminate";
    pub const ATTACHMENTS: &str = "attachments";
    pub const QUOTED_PATH_DELIVERY: &str = "quoted-path-delivery";
    /// The host resolves an authored profile key into argv; callers never
    /// become a second process-launch authority.
    pub const PROFILE_LAUNCH: &str = "profile-launch";
    /// Bounded snapshot/scrollback reads that do not acquire a controller.
    pub const OBSERVER_OUTPUT: &str = "observer-output";
    /// One atomic bounded snapshot plus the independent event/output cursors
    /// needed to resume a public stream after a gap.
    pub const OBSERVATION_CURSORS: &str = "observation-cursors";
    /// Profile-authorized creation of a structured provider session with no
    /// PTY fallback hidden behind the requested mode.
    pub const MANAGED_SESSIONS: &str = "managed-sessions";
    pub const NATIVE_ATTACHMENT_DELIVERY: &str = "native-attachment-delivery";
    pub const MANAGED_EVENTS: &str = "managed-events";
    pub const MANAGED_CANCEL: &str = "managed-cancel";
    pub const MANAGED_APPROVAL: &str = "managed-approval";
    pub const DYNAMIC_APPROVALS: &str = "dynamic-approvals";
    /// A controller may clear managed attention only after presenting the
    /// corresponding managed-event sequence to the user.
    pub const PRESENTED_ATTENTION: &str = "presented-attention";
    pub const MANAGED_CHILD_SESSIONS: &str = "managed-child-sessions";
    pub const MANAGED_DELEGATION: &str = "managed-delegation";
    pub const PROVIDER_MODELS: &str = "provider-models";
    pub const PROVIDER_READINESS: &str = "provider-readiness";
    pub const MANAGED_USAGE: &str = "managed-usage";
    /// The project verbs, the named catalog subscription, and placing a
    /// session into a project at create. One capability rather than several:
    /// a peer that can list projects can place sessions in them, and a peer
    /// that cannot must not be able to do either.
    pub const PROJECTS: &str = "projects";
    /// The supervised workspace-lifecycle verb: closing every session a Koru
    /// workspace owns, on behalf of the workspace's removal.
    ///
    /// Separate from [`TERMINATE`], which is the lease-holder ending *its own*
    /// session. This one ends somebody else's, so it is not a terminal command
    /// at all — negotiating it says a peer can speak the frame, and the
    /// launch-scoped lifecycle authority in the frame says it may.
    pub const WORKSPACE_LIFECYCLE: &str = "workspace-lifecycle";
    /// The delegation family (protocol v14): the non-creating creation
    /// lookup and key retirement, the controller handover verbs, the cancel
    /// reason, and model identity on usage.
    ///
    /// One capability rather than several because it names one authority
    /// family — a peer that hands work to an agent and must find it again
    /// after a restart. A peer without it can still open and drive manual
    /// managed sessions exactly as before; it just cannot be delegated to,
    /// because without the lookup the recovery guarantee cannot hold.
    pub const DELEGATION: &str = "delegation";
    /// Session origin ownership (protocol v16): the `origin` a creation and
    /// a summary carry, the `lookup_origin` verb, the `ai_control` hello
    /// role and the creation generation it mints. A peer without it cannot
    /// own a chat's sessions, so the AI worker refuses to start an agent
    /// chat against it rather than creating sessions nobody can find again.
    pub const SESSION_ORIGIN: &str = "session-origin";
    /// Explicit launch selection (protocol v16): the defaulted `selection`
    /// on a creation. Gated because an older host would ignore the field and
    /// run the provider's default, which is the one outcome an explicit
    /// choice exists to prevent.
    pub const LAUNCH_SELECTION: &str = "launch-selection";
    /// Resume at launch: the defaulted `resume` flag on a creation, which asks
    /// the host to start a raw agent on its most recent conversation in the
    /// folder. Gated because an older host would ignore the flag and start a
    /// fresh conversation under a button that said *Resume*.
    pub const LAUNCH_RESUME: &str = "launch-resume";

    pub const ALL: &[&str] = &[
        CONTROLLER_LEASES,
        RENDER_DELTAS,
        SCROLLBACK_PAGES,
        RAW_WRITE,
        TERMINAL_KEYS,
        RESIZE,
        INTERRUPT,
        TERMINATE,
        ATTACHMENTS,
        QUOTED_PATH_DELIVERY,
        PROFILE_LAUNCH,
        OBSERVER_OUTPUT,
        OBSERVATION_CURSORS,
        MANAGED_SESSIONS,
        NATIVE_ATTACHMENT_DELIVERY,
        MANAGED_EVENTS,
        MANAGED_CANCEL,
        MANAGED_APPROVAL,
        DYNAMIC_APPROVALS,
        PRESENTED_ATTENTION,
        MANAGED_CHILD_SESSIONS,
        MANAGED_USAGE,
        PROVIDER_READINESS,
        PROVIDER_MODELS,
        MANAGED_DELEGATION,
        PROJECTS,
        WORKSPACE_LIFECYCLE,
        DELEGATION,
        SESSION_ORIGIN,
        LAUNCH_SELECTION,
        LAUNCH_RESUME,
    ];
}
