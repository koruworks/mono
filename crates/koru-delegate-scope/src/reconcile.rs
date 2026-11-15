// SPDX-License-Identifier: MPL-2.0

//! Rebuilding the registry after a worker restart (§7.6, §7.8).
//!
//! The registry is memory, so a restart starts empty — and an attempt that
//! was running when the worker died may still be running, or may be the
//! *prior attempt unknown* §7.6 refuses to guess about. Both keep their
//! claim: a folder is free only when its holder is known to have ended.
//! Every kept claim is resolved again against the filesystem, because an
//! alias can change while the worker is down and a stored identity would
//! be a stored guess.

use koru_delegate_core::{RunAttemptId, RunId, WorkspaceScope};

use crate::{ScopeClaim, ScopeClaims, ScopeIdentity, ScopeRefusal, ScopeRoots};

/// A claim as `runs.db` recorded it: the scope as requested, never a
/// resolved path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DurableClaim {
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub scope: WorkspaceScope,
}

/// The port `runs.db` (P1.3) implements: which claims were live when the
/// worker last stopped.
pub trait DurableClaims {
    type Error;

    /// Every claim recorded as held and not yet released. Returning more
    /// than was live is safe — reconciliation drops the ended ones;
    /// returning fewer is not.
    fn claims_live_at_shutdown(&self) -> Result<Vec<DurableClaim>, Self::Error>;
}

/// What the orchestrator's non-creating lookup (§7.6) says about an
/// attempt. `Unknown` is possibly running, never free.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Liveness {
    Live,
    Ended,
    Unknown,
}

/// What reconciliation did, in ids and codes, for the orchestrator to log.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ReconcileReport {
    /// Kept: still live or unknown.
    pub kept: Vec<(RunId, RunAttemptId)>,
    /// Dropped: known to have ended.
    pub ended: Vec<(RunId, RunAttemptId)>,
    /// Dropped: the scope no longer resolves to a directory, so no start
    /// can collide with it either. Reported, because a live attempt whose
    /// folder vanished is news.
    pub unresolvable: Vec<(RunId, RunAttemptId, ScopeRefusal)>,
}

impl ScopeClaims {
    /// Reconcile known scopes while preserving only already-held exact claims
    /// for Runs whose retained scope could not be read. This creates no claim
    /// and grants no admission; a healthy newly resolved claim takes precedence.
    pub fn reconcile_preserving<D: DurableClaims + ?Sized>(
        durable: &D,
        roots: &dyn ScopeRoots,
        liveness: impl FnMut(RunAttemptId) -> Liveness,
        previous: &Self,
        unresolved_scopes: &[RunId],
    ) -> Result<(Self, ReconcileReport), D::Error> {
        let (mut claims, mut report) = Self::reconcile(durable, roots, liveness)?;
        for old in previous
            .claims()
            .filter(|old| unresolved_scopes.contains(&old.run))
        {
            if !claims
                .claims()
                .any(|current| current.attempt == old.attempt)
            {
                claims.insert(old.clone());
                report.kept.push((old.run, old.attempt));
            }
        }
        Ok((claims, report))
    }

    /// Rebuild the registry from what `runs.db` recorded, keeping every
    /// claim whose attempt `liveness` calls live or unknown and dropping
    /// the ones it calls ended.
    pub fn reconcile<D: DurableClaims + ?Sized>(
        durable: &D,
        roots: &dyn ScopeRoots,
        mut liveness: impl FnMut(RunAttemptId) -> Liveness,
    ) -> Result<(Self, ReconcileReport), D::Error> {
        let mut claims = Self::new();
        let mut report = ReconcileReport::default();
        for recorded in durable.claims_live_at_shutdown()? {
            let key = (recorded.run, recorded.attempt);
            match liveness(recorded.attempt) {
                Liveness::Ended => report.ended.push(key),
                Liveness::Live | Liveness::Unknown => {
                    match ScopeIdentity::resolve(&recorded.scope, recorded.run, roots) {
                        Ok(scope) => {
                            claims.insert(ScopeClaim {
                                run: recorded.run,
                                attempt: recorded.attempt,
                                scope,
                            });
                            report.kept.push(key);
                        }
                        Err(refusal) => report.unresolvable.push((key.0, key.1, refusal)),
                    }
                }
            }
        }
        Ok((claims, report))
    }
}
