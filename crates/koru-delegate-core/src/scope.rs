// SPDX-License-Identifier: MPL-2.0

//! Where a run works, and what executes it.
//!
//! **Scope** is the working folder an agent is started in — a session
//! project's root (the durable, workspace-scoped folder the terminal domain
//! already has), a folder inside it, or a directory Koru creates for a run
//! that has none. **Environment** is the machine running it: a local device
//! now, and the seam D12's cloud worker fills later.

use serde::{Deserialize, Serialize};

use crate::{DeviceId, ProjectId, RelativeFolder};

/// The working folder a run is admitted to (P1.5) and started in.
///
/// Identity is a [`ProjectId`] rather than a path: the project already
/// carries a canonical root the host validates at launch, and an id is what a
/// second device can *name* without being able to resolve it (D15, D16). The
/// folder inside it is text a person chose, and is redacted like every path.
///
/// **`None` is unreachable at admission.** It exists so a request can be
/// built before a scope is chosen and so the modal has a state to refuse;
/// P1.5 makes it impossible to start a run with it. M4 adds git shapes —
/// a worktree per run — as new variants, which is why this is an enum.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceScope {
    /// Fixed engine-owned Bot binding. Only authenticated Bot admission may
    /// resolve it; deleting a Run never deletes this persistent folder.
    BotFolder { folder: crate::BotWorkingFolder },
    /// No scope chosen. Refused at admission (P1.5), never started.
    None,
    /// The root of a session project.
    ProjectRoot { project: ProjectId },
    /// A folder inside a session project's root.
    ProjectFolder {
        project: ProjectId,
        folder: RelativeFolder,
    },
    /// A directory Koru creates for this run alone and owns (D37 names it:
    /// an unregistered result there survives as long as the run does).
    Temporary,
}

impl WorkspaceScope {
    /// The project a scope resolves through, if it has one.
    pub fn project(&self) -> Option<ProjectId> {
        match self {
            Self::ProjectRoot { project } | Self::ProjectFolder { project, .. } => Some(*project),
            Self::None | Self::Temporary | Self::BotFolder { .. } => None,
        }
    }

    /// Whether admission may accept this scope at all. `None` is the one
    /// shape that never starts; everything else is P1.5's to check against
    /// the real filesystem.
    pub fn is_admissible(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// **Whether Koru made the folder this scope names, and may therefore
    /// remove it.**
    ///
    /// One variant answers yes. `Temporary` is a directory Koru created for
    /// this run alone and owns for as long as the run lives (D37) — deleting
    /// the run is what ends that. `ProjectRoot` and `ProjectFolder` point at
    /// a person's **own project**: a run scoped to a project root runs
    /// *inside their repository*, and a delete that removed "the directory
    /// the run was run in" would remove the repository. It was theirs before
    /// the run and it is theirs after.
    ///
    /// `None` never started, so there is no folder to have an opinion about.
    ///
    /// It lives on the scope rather than beside the code that empties the
    /// trash because it is a fact about what a scope *is*, and because a
    /// second place to decide it is a second place to get it wrong. M4's git
    /// shapes arrive as new variants here and each one has to answer this
    /// question deliberately — a worktree Koru cut is Koru's; a checkout
    /// somebody handed it is not.
    pub fn is_koru_owned(&self) -> bool {
        match self {
            Self::Temporary => true,
            Self::None
            | Self::ProjectRoot { .. }
            | Self::ProjectFolder { .. }
            | Self::BotFolder { .. } => false,
        }
    }
}

/// How one working scope relates to another that already holds a folder,
/// **from the candidate's side** — the only point of view a run's own
/// history needs (D34).
///
/// It lives here rather than beside the overlap arithmetic because two
/// crates that never see each other need the same three words: the scope
/// crate answers the question, a run fact carries the answer across a
/// restart, and the event row spells it. A disjoint pair is not busy, so
/// it has no variant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ScopeOverlap {
    /// One directory, however it was spelled.
    Same,
    /// The candidate's folder lies inside the holder's.
    Inside,
    /// The candidate's folder contains the holder's.
    Around,
}

impl ScopeOverlap {
    pub const ALL: &'static [Self] = &[Self::Same, Self::Inside, Self::Around];
}

/// The machine a run executes on (D3: execution machinery is device-local).
///
/// One variant today. D12 makes the orchestrator cloud-deployable later, and
/// that arrives as a second variant here rather than as a flag on the device
/// — which is why this is `#[non_exhaustive]`: a dependant that matches it
/// must already say what it does with an environment it does not know.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionEnvironment {
    /// A device of the person's own, named so every other device can say
    /// which one is running the work (D16).
    LocalDevice {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        device: DeviceId,
    },
}

impl ExecutionEnvironment {
    pub fn local(device: DeviceId) -> Self {
        Self::LocalDevice { device }
    }

    /// The device executing, when the environment is one. A cloud worker
    /// will answer `None` here and be named some other way.
    pub fn device(&self) -> Option<DeviceId> {
        match self {
            Self::LocalDevice { device } => Some(*device),
        }
    }
}
