// SPDX-License-Identifier: MPL-2.0

//! Who holds a scope, and D34's two answers when someone else wants it.
//!
//! A claim is `(scope, run, attempt)`: the attempt is the unit that holds a
//! folder, because a resume is a fresh attempt (§7.6) and the old one's
//! claim is what reconciliation drops or keeps. The registry is in memory;
//! `runs.db` is where a claim outlives the worker, and it hands them back
//! through [`crate::DurableClaims`].

use std::fmt;

use koru_delegate_core::{RunAttemptId, RunId, WorkspaceScope};
use uuid::Uuid;

use crate::{Overlap, ScopeIdentity, ScopeRefusal, ScopeRoots};

/// The schedule a firing came from, so a skipped one is recorded against
/// it. An opaque UUID; M2 mints its `ScheduleId` and this becomes that.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScheduleRef(Uuid);

impl ScheduleRef {
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Debug for ScheduleRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let full = self.0.simple().to_string();
        write!(f, "schedule({})", &full[..8])
    }
}

/// Why a start is being asked for — the one fact that changes the answer
/// to a busy scope (§7.8).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StartOrigin {
    /// A person is present and decides.
    Manual,
    /// Nobody is present; the system does not take the risk on their behalf.
    Scheduled { schedule: ScheduleRef },
    /// The schedule already atomically claimed this launch after checking
    /// the scope. Later edits, retirement, or another holder appearing do
    /// not revoke the durable claim.
    ClaimedSchedule { schedule: ScheduleRef },
    /// Constructed only after the execution owner validates durable Bot
    /// admission. The discriminator itself is not an authority credential.
    Bot { bot: koru_delegate_core::BotId },
}

/// One held scope.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeClaim {
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub scope: ScopeIdentity,
}

/// A run that holds a scope overlapping the one asked for. Ids and the
/// overlap kind only; the surface renders the run by id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Holder {
    pub run: RunId,
    pub attempt: RunAttemptId,
    /// How the candidate relates to this holder's scope.
    pub overlap: Overlap,
}

/// The scope is busy. Names the holder — the earliest claim that overlaps —
/// and every other one, because D34's manual override means several runs
/// can hold one folder at once.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BusyWarning {
    pub holder: Holder,
    pub others: Vec<Holder>,
}

impl BusyWarning {
    fn from_holders(mut holders: Vec<Holder>) -> Option<Self> {
        if holders.is_empty() {
            return None;
        }
        let holder = holders.remove(0);
        Some(Self {
            holder,
            others: holders,
        })
    }

    /// Every holder, the named one first.
    pub fn holders(&self) -> impl Iterator<Item = &Holder> {
        std::iter::once(&self.holder).chain(self.others.iter())
    }
}

/// The answer to a start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Admission {
    /// The claim is registered; the caller starts the run. With a warning,
    /// a person was told who holds the folder and proceeded anyway (D34).
    Admitted { warning: Option<BusyWarning> },
    /// Nothing was claimed and nothing starts. The caller records the
    /// skipped firing, and its holder, in the schedule's history.
    Skipped {
        schedule: ScheduleRef,
        busy: BusyWarning,
    },
    /// The scope resolves to no directory; nothing was claimed.
    Refused(ScopeRefusal),
}

/// The in-memory registry of held scopes.
#[derive(Default, Debug)]
pub struct ScopeClaims {
    /// In claim order, so the earliest holder is the one a warning names.
    claims: Vec<ScopeClaim>,
}

impl ScopeClaims {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `scope` for `run` and admit it per D34. The one entry P1.6's
    /// start path calls; it touches the disk through `roots` and
    /// `canonicalize`, so it runs on the orchestrator's thread.
    pub fn admit(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        scope: &WorkspaceScope,
        roots: &dyn ScopeRoots,
        origin: StartOrigin,
    ) -> Admission {
        if matches!(scope, WorkspaceScope::BotFolder { .. })
            != matches!(origin, StartOrigin::Bot { .. })
        {
            return Admission::Refused(ScopeRefusal::NotAdmissible);
        }
        match ScopeIdentity::resolve(scope, run, roots) {
            Ok(identity) => self.admit_resolved(run, attempt, identity, origin),
            Err(refusal) => Admission::Refused(refusal),
        }
    }

    /// Admit an already-resolved identity. Pure.
    ///
    /// A **manual** start is always admitted; if the scope overlaps a held
    /// one the admission carries the warning to show. A **scheduled** start
    /// onto a held scope is skipped with the holder recorded and claims
    /// nothing. An attempt re-admitting its own scope replaces its claim and
    /// is not its own holder.
    pub fn admit_resolved(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        scope: ScopeIdentity,
        origin: StartOrigin,
    ) -> Admission {
        let holders = self
            .holders_of(&scope)
            .into_iter()
            .filter(|holder| holder.attempt != attempt)
            .collect();
        let busy = BusyWarning::from_holders(holders);
        match (origin, busy) {
            (StartOrigin::Bot { .. }, Some(_)) => Admission::Refused(ScopeRefusal::Busy),
            (StartOrigin::Scheduled { schedule }, Some(busy)) => {
                Admission::Skipped { schedule, busy }
            }
            (StartOrigin::ClaimedSchedule { .. }, _) => {
                self.insert(ScopeClaim {
                    run,
                    attempt,
                    scope,
                });
                Admission::Admitted { warning: None }
            }
            (_, warning) => {
                self.insert(ScopeClaim {
                    run,
                    attempt,
                    scope,
                });
                Admission::Admitted { warning }
            }
        }
    }

    /// Every claim whose scope overlaps `scope`, earliest first.
    pub fn holders_of(&self, scope: &ScopeIdentity) -> Vec<Holder> {
        self.claims
            .iter()
            .filter_map(|claim| {
                let overlap = scope.overlap(&claim.scope);
                overlap.is_overlap().then_some(Holder {
                    run: claim.run,
                    attempt: claim.attempt,
                    overlap,
                })
            })
            .collect()
    }

    /// Release the claim `attempt` holds — on the run's end, or when the
    /// start it was admitted for never happened. `None` if it held nothing.
    pub fn release(&mut self, attempt: RunAttemptId) -> Option<ScopeClaim> {
        let index = self
            .claims
            .iter()
            .position(|claim| claim.attempt == attempt)?;
        Some(self.claims.remove(index))
    }

    pub fn claims(&self) -> impl Iterator<Item = &ScopeClaim> {
        self.claims.iter()
    }

    pub fn len(&self) -> usize {
        self.claims.len()
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    pub(crate) fn insert(&mut self, claim: ScopeClaim) {
        match self
            .claims
            .iter_mut()
            .find(|held| held.attempt == claim.attempt)
        {
            Some(held) => *held = claim,
            None => self.claims.push(claim),
        }
    }
}
