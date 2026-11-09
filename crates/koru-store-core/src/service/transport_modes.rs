// SPDX-License-Identifier: MPL-2.0

//! The Store's half of A1: the typed operations, and the immediate stop.
//!
//! The catalog holds the rows and [`crate::transport::plan`] decides what a
//! group of requests does to them. What is here is the part that needs a
//! *process*: closing admission in memory before the durable write, keeping it
//! closed when that write fails, and answering "may this move data right now"
//! in a way a transport can branch on.
//!
//! ## The order of operations, and why it is that order
//!
//! ```text
//! set_transport_modes(restrictive)
//!   1. read the current group          what is the fence being compared to
//!   2. close the fence, in memory      nothing new is admitted from here
//!   3. write, durably                  the row becomes the authority
//!   4a. committed -> reopen            the row is at least as restrictive
//!   4b. failed    -> stay closed       and say so; claim no durable choice
//! ```
//!
//! Steps 2 and 3 are in that order because a person who has just turned Cloud
//! off is entitled to have it off *now*, not after an fsync — and because a
//! failed write that had already reopened the guard would be a transfer
//! continuing under a mode nobody holds. Restart is the other half: the fence
//! is not persisted, so a restart reloads the last **actually committed** mode
//! and a failed write cannot promise anything across it.

use super::*;
use crate::root_catalog::CatalogUnitRef;
use crate::transport::{
    EffectiveTransportMode, InitializeTransportMode, SetTransportMode, TransportMode,
    TransportModeCommit, TransportModeGroup, TransportModeRequest, TransportModeState,
    TransportPath, TransportSubject,
};

impl<C: Catalog, B: Backend> Store<C, B> {
    // ------------------------------------------------------------ the view

    /// One subject's row: its mode, whether it is a choice or a default, and
    /// the revision a compare-and-set must carry.
    ///
    /// A missing or undecodable row is a storage error. It is not read as
    /// Local-only — see [`Catalog::transport_mode`].
    pub fn transport_mode(&self, subject: TransportSubject) -> Result<TransportModeState> {
        self.catalog.transport_mode(subject)
    }

    /// One Profile's row and every Workspace row beneath it, as the local
    /// catalog view a settings surface reads.
    pub fn transport_mode_group(&self, owner: ProfileId) -> Result<TransportModeGroup> {
        self.catalog.transport_mode_group(owner)
    }

    /// The live policy generation. A callback holding an older one has been
    /// invalidated and must ask again; it is a session fence, never the
    /// persisted revision.
    pub fn policy_generation(&self) -> u64 {
        self.transport.generation()
    }

    /// **The one question a transport check asks**, for both the peer and the
    /// Cloud path and for both a Profile and a Workspace.
    ///
    /// The answer is the subject's own mode capped at its Profile's, then
    /// capped again by the in-memory fence. A stored child above its Profile
    /// is reported through `ceiling_mismatch` and never repaired: contract §3
    /// is explicit that a mismatch stops the affected transport and is said out
    /// loud, because the alternative is rewriting somebody's choice to make an
    /// inconsistency go away.
    pub fn transport_admission(&self, subject: TransportSubject) -> Result<EffectiveTransportMode> {
        let state = self.catalog.transport_mode(subject)?;
        self.admission_of(state, state.owner)
    }

    /// [`Self::transport_admission`] as it will read once `subject` is filed
    /// under `owner` — what a move asks about its **destination** before it
    /// writes anything (root-catalog TECHNICAL.final §4.4), so a placement
    /// bound for a Local-only or fenced Profile goes to the private overlay
    /// rather than the durable unit. The row's stored choice is read as it
    /// is; only the ceiling and the fence come from `owner`.
    pub(super) fn transport_admission_under(
        &self,
        subject: TransportSubject,
        owner: ProfileId,
    ) -> Result<EffectiveTransportMode> {
        let state = self.catalog.transport_mode(subject)?;
        self.admission_of(state, owner)
    }

    fn admission_of(
        &self,
        state: TransportModeState,
        owner: ProfileId,
    ) -> Result<EffectiveTransportMode> {
        let subject = state.subject;
        let profile_mode = match subject {
            TransportSubject::Profile(_) => state.mode,
            TransportSubject::Workspace(_) => {
                self.catalog
                    .transport_mode(TransportSubject::Profile(owner))?
                    .mode
            }
        };
        let ceiling_mismatch = state.mode > profile_mode;
        let fenced = self.transport.is_closed(subject, owner);
        let mode = if fenced {
            TransportMode::LocalOnly
        } else {
            state.mode.min(profile_mode)
        };
        Ok(EffectiveTransportMode {
            subject,
            mode,
            profile_mode,
            stored_mode: state.mode,
            explicit_choice: state.explicit_choice,
            revision: state.revision,
            ceiling_mismatch,
            fenced,
            generation: self.transport.generation(),
        })
    }

    /// The refusal every check point shares, so the sentence is written once.
    ///
    /// `Ok(())` means the bytes may move now, under the generation the answer
    /// was taken at. An old callback cannot bypass a newer fence, because this
    /// is asked again rather than remembered.
    pub fn admit_transport(
        &self,
        subject: TransportSubject,
        path: TransportPath,
    ) -> Result<EffectiveTransportMode> {
        let admission = self.transport_admission(subject)?;
        let allowed = match path {
            TransportPath::Peer => admission.allows_peer(),
            TransportPath::Cloud => admission.allows_cloud(),
        };
        if allowed {
            return Ok(admission);
        }
        if admission.ceiling_mismatch {
            // Reported rather than repaired, and reported as corruption
            // because that is what it is: a stored child above its Profile can
            // only come from damage or a move, and neither is a mode decision.
            return Err(Error::new(
                Code::CorruptState,
                "this Workspace's stored transport mode is above its Profile's; \
                 the transport is stopped until the inconsistency is resolved",
            ));
        }
        Err(Error::new(
            Code::Forbidden,
            match path {
                TransportPath::Peer => "this device's transport mode is Local-only here",
                TransportPath::Cloud => "this device's transport mode does not allow Cloud here",
            },
        ))
    }

    // ------------------------------------------------------- the operations

    /// A deliberate local selection.
    ///
    /// Sets `explicit_choice`, so choosing the already-effective default is
    /// still a choice and still pins the row against a later verified
    /// promotion. Returns after the Store's durable commit.
    pub fn set_transport_mode(&mut self, request: SetTransportMode) -> Result<TransportModeCommit> {
        one(self.commit_transport_modes(&[TransportModeRequest::Set(request)])?)
    }

    /// A verified default from the trusted host-evidence path.
    ///
    /// Capped at the Profile ceiling, never able to modify an explicit row, and
    /// never able to raise the Profile to fit a child.
    pub fn initialize_transport_mode(
        &mut self,
        request: InitializeTransportMode,
    ) -> Result<TransportModeCommit> {
        one(self.commit_transport_modes(&[TransportModeRequest::Initialize(request)])?)
    }

    /// Several verified defaults under one Profile, as **one** decision.
    ///
    /// **A Workspace initializer alone is not enough**, and that is the trap
    /// this exists to close. Migration 12 seeded the Profile row `local_only`
    /// too, and `plan` caps a child's default at its Profile while never
    /// raising the Profile to fit a child — so initializing a Cloud-enabled
    /// Workspace on its own commits `min(Cloud, LocalOnly)` and *reports
    /// success*. Submitted together, `plan` decides the Profile first and the
    /// child is capped at what the Profile has just become.
    pub fn initialize_transport_modes(
        &mut self,
        requests: &[InitializeTransportMode],
    ) -> Result<Vec<TransportModeCommit>> {
        let requests: Vec<_> = requests
            .iter()
            .copied()
            .map(TransportModeRequest::Initialize)
            .collect();
        self.commit_transport_modes(&requests)
    }

    /// **The initialization half of B8** (D254): promote one Profile's
    /// still-default rows from evidence the Agent has already verified.
    ///
    /// **Nothing in production calls this.** It was driven by the Agent's
    /// account, subscription, pairing and selection effects until D258
    /// withdrew evidence-driven promotion on 2026-09-20 and made a person's
    /// answer to the Profile prompt the only path; B10 deleted the desktop
    /// driver and this entry point kept its tests. It is retained rather than
    /// deleted because D258 has not yet been answered by a person on hardware —
    /// see `docs/contributing/techdebt/README.md` for the two honest exits. What follows
    /// describes what it does if something calls it again.
    ///
    /// It reads the group, quotes **each row's own
    /// revision** as the compare-and-set token, and submits only the rows a
    /// default could still move:
    ///
    /// - an explicit row is left alone, whatever the evidence says;
    /// - a row whose verified default is no higher than it already reads is
    ///   left alone, because the initializer promotes and never demotes;
    /// - the Profile is submitted with its children, so a Cloud-enabled
    ///   Workspace is not capped at the Profile's seeded Local-only.
    ///
    /// Returns the outcomes for the rows it submitted, in Profile-then-
    /// Workspace order. An empty result means nothing needed promoting, which
    /// is the ordinary answer on every effect after the first.
    pub fn initialize_transport_modes_from_evidence(
        &mut self,
        owner: ProfileId,
        profile: crate::transport::SharingEvidence,
        workspaces: &std::collections::BTreeMap<WorkspaceId, crate::transport::SharingEvidence>,
    ) -> Result<Vec<TransportModeCommit>> {
        let group = self.catalog.transport_mode_group(owner)?;
        let mut requests: Vec<InitializeTransportMode> = Vec::new();
        if profile.promotes(&group.profile) {
            requests.push(InitializeTransportMode {
                subject: group.profile.subject,
                expected_revision: group.profile.revision,
                mode: profile.default_mode(),
            });
        }
        for child in &group.children {
            let Some(workspace) = child.subject.workspace() else {
                continue;
            };
            let Some(evidence) = workspaces.get(&workspace).copied() else {
                continue;
            };
            if evidence.promotes(child) {
                requests.push(InitializeTransportMode {
                    subject: child.subject,
                    expected_revision: child.revision,
                    mode: evidence.default_mode(),
                });
            }
        }
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        self.initialize_transport_modes(&requests)
    }

    /// **What a person answered when they were asked** (D258).
    ///
    /// Turning a Profile's Cloud sync on asks whether its Workspaces should
    /// move too. This is both answers, because they are one operation with one
    /// flag: the Profile moves either way, and `include_workspaces` says
    /// whether its children come with it — in the **same group**, so a device
    /// is never left with a Profile at Cloud and half a ladder beneath it.
    ///
    /// Four rules, and each of them is a rule about not overwriting somebody:
    ///
    /// - **Raising never overwrites an explicit child.** A Workspace this
    ///   person already chose Local-only for is not in the group at all when
    ///   the answer is *move everything*. "Move all the Workspaces" is about
    ///   the ones nobody has spoken about; the one they *did* speak about is
    ///   the one an answer to a Profile-level question must not quietly
    ///   reverse.
    /// - **Lowering carries every child it would cap, explicit or not.** That
    ///   is not the same act wearing a different sign: a Profile below a child
    ///   caps it anyway, and A1 refuses the Profile outright unless each
    ///   higher child is *resolved in the same group*. The alternative to
    ///   carrying them is refusing the restriction, which would leave somebody
    ///   unable to turn their own device off.
    /// - **Each row quotes its own revision**, so a choice made on this device
    ///   between the read and the write supersedes rather than being clobbered.
    /// - **Nothing here is hosted.** D230 stands: the mode is device-local, and
    ///   no hosted row, policy CRDT, peer message or remote stop notice follows
    ///   from it. A second device is told nothing and decides for itself.
    ///
    /// This is deliberately **not** the evidence path. Nothing promotes a row
    /// because an account exists; a person does, or nothing does.
    pub fn choose_transport_mode(
        &mut self,
        owner: ProfileId,
        mode: TransportMode,
        include_workspaces: bool,
    ) -> Result<TransportModeChoice> {
        let group = self.catalog.transport_mode_group(owner)?;
        let mut requests = vec![SetTransportMode {
            subject: group.profile.subject,
            expected_revision: group.profile.revision,
            mode,
        }];
        let mut left_alone = 0u32;
        for child in &group.children {
            // A child this Profile is about to cap has to be in the group
            // whatever the answer was, or A1 refuses the whole decision.
            let capped = child.mode > mode;
            if !include_workspaces && !capped {
                continue;
            }
            if child.explicit_choice && !capped {
                left_alone = left_alone.saturating_add(1);
                continue;
            }
            requests.push(SetTransportMode {
                subject: child.subject,
                expected_revision: child.revision,
                mode,
            });
        }
        let outcomes = self.set_transport_modes(&requests)?;
        let profile = outcomes
            .first()
            .copied()
            .ok_or_else(|| Error::new(Code::Internal, "a mode group answered no outcome"))?;
        let moved = outcomes
            .iter()
            .skip(1)
            .filter(|outcome| outcome.wrote())
            .count() as u32;
        Ok(TransportModeChoice {
            profile,
            workspaces_submitted: requests.len().saturating_sub(1) as u32,
            workspaces_moved: moved,
            workspaces_left_alone: left_alone,
        })
    }

    /// A Profile and the children a downgrade needed resolved, as **one**
    /// decision.
    ///
    /// This is not a second settings owner: it is the existing serialized Store
    /// transaction carrying the exact affected operations, so an unrelated
    /// invalid mutation cannot leave half a ladder behind.
    pub fn set_transport_modes(
        &mut self,
        requests: &[SetTransportMode],
    ) -> Result<Vec<TransportModeCommit>> {
        let requests: Vec<_> = requests
            .iter()
            .copied()
            .map(TransportModeRequest::Set)
            .collect();
        self.commit_transport_modes(&requests)
    }

    fn commit_transport_modes(
        &mut self,
        requests: &[TransportModeRequest],
    ) -> Result<Vec<TransportModeCommit>> {
        let owner = self.transport_group_owner(requests)?;

        // Read first, only to decide what the fence has to close. This read is
        // **not** the authority — the catalog re-reads under its own serialized
        // transaction — so a racing change here can only make the fence wider,
        // which is the safe direction.
        let before = self.catalog.transport_mode_group(owner)?;
        // What may export *now*, remembered before the fence moves: a subject
        // this group turns from no-export to export has its pending root
        // overlay published once the rows have committed (root_move.rs).
        let exportable_before: Vec<(TransportSubject, bool)> = std::iter::once(&before.profile)
            .chain(before.children.iter())
            .map(|state| {
                let effective = state.mode.min(before.profile.mode);
                let fenced = self.transport.is_closed(state.subject, owner);
                (state.subject, !fenced && effective.allows_peer())
            })
            .collect();
        let restrictive = restrictive_subjects(&before, requests);
        // A row whose revision is exhausted can never be changed again, so this
        // device could never turn that transport off later either. Contract §2
        // holds its admission closed until the row is repaired — whichever
        // direction the request that met it was going, and whether or not the
        // operation itself succeeds. It is therefore closed separately from
        // `restrictive` and is not reopened below: nothing here repairs it.
        let exhausted = exhausted_subjects(&before, requests);
        if !restrictive.is_empty() || !exhausted.is_empty() {
            // Before the durable write, and before anything else can be
            // admitted under the mode that is being left behind.
            self.transport
                .close(restrictive.iter().chain(exhausted.iter()).copied());
        }

        match self.catalog.commit_transport_modes(owner, requests) {
            Ok(outcomes) => {
                // The rows are authority again. Reopening is safe precisely
                // because what they now say is at least as restrictive as what
                // the fence was standing in for.
                let reopen: Vec<_> = restrictive
                    .iter()
                    .copied()
                    .filter(|subject| !exhausted.contains(subject))
                    .collect();
                if !reopen.is_empty() {
                    self.transport.reopen(reopen);
                } else if outcomes.iter().any(|outcome| outcome.wrote()) {
                    // A permissive change still invalidates: an answer cached
                    // across it would deny something now allowed.
                    self.transport.invalidate();
                }
                if outcomes.iter().any(|outcome| outcome.wrote()) {
                    self.publish_newly_exportable_overlays(&exportable_before);
                }
                Ok(outcomes)
            }
            // **The guard stays closed and the failure is reported.** Saying
            // "saved" here, or reopening on the way out, would claim a durable
            // choice that is not on the disk — and the next restart would
            // silently go back to the old mode with transfers running.
            Err(error) => Err(error),
        }
    }

    /// Root-catalog TECHNICAL.final §4.3: when a committed group makes a
    /// Workspace exportable, the private overlay its placement waited in is
    /// published into the durable unit. A failure is logged, not returned —
    /// the mode rows have committed and the overlay is intact, so the next
    /// move or eligibility change retries, and the export seam answers
    /// `None` for a subject with a pending overlay in the meantime.
    fn publish_newly_exportable_overlays(
        &mut self,
        exportable_before: &[(TransportSubject, bool)],
    ) {
        for (subject, before) in exportable_before {
            let TransportSubject::Workspace(workspace) = *subject else {
                continue;
            };
            if *before {
                continue;
            }
            let now = self
                .transport_admission(*subject)
                .is_ok_and(|admission| admission.allows_peer());
            if !now {
                continue;
            }
            if let Err(error) =
                self.publish_pending_overlay(CatalogUnitRef::Workspace { workspace })
            {
                tracing::warn!(
                    workspace = %workspace,
                    error = %error,
                    "a pending root overlay could not be published after its subject became exportable"
                );
            }
        }
    }

    /// Which Profile's group a set of requests belongs to, refusing a mix.
    fn transport_group_owner(&self, requests: &[TransportModeRequest]) -> Result<ProfileId> {
        let mut owner: Option<ProfileId> = None;
        for request in requests {
            let found = self.catalog.transport_mode(request.subject())?.owner;
            match owner {
                None => owner = Some(found),
                Some(existing) if existing == found => {}
                Some(_) => {
                    return Err(Error::new(
                        Code::InvalidRequest,
                        "a transport mode group must name one Profile and its own Workspaces",
                    ));
                }
            }
        }
        owner.ok_or_else(|| {
            Error::new(
                Code::InvalidRequest,
                "a transport mode group names no subject",
            )
        })
    }
}

/// What one answer to the Profile prompt did (D258).
///
/// The counts are separate because they are three different facts and a
/// surface that added them up would be unable to tell a person the one that
/// matters: a Workspace **left alone** is not a Workspace that failed, it is
/// one they had already decided about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransportModeChoice {
    /// The Profile's own outcome. `AlreadyCurrent` is a success and means the
    /// person answered the same thing twice.
    pub profile: TransportModeCommit,
    /// How many Workspaces the group carried.
    pub workspaces_submitted: u32,
    /// How many of those actually wrote.
    pub workspaces_moved: u32,
    /// Workspaces held out of the group because somebody had already chosen
    /// for them. Their choice is untouched, and that is the point.
    pub workspaces_left_alone: u32,
}

/// Which subjects a group would *restrict*, and therefore which ones have to be
/// closed before the write is attempted.
///
/// Lowering the Profile restricts every child that was above the new value,
/// whether or not the caller named it — the fence is about what may move now,
/// not about which rows are being written.
/// The single outcome of a single-request group. `plan` answers one outcome per
/// request, so the vector is never empty here.
fn one(mut outcomes: Vec<TransportModeCommit>) -> Result<TransportModeCommit> {
    if outcomes.len() == 1 {
        return Ok(outcomes.remove(0));
    }
    Err(Error::new(
        Code::Internal,
        "a transport mode operation answered a different number of outcomes than requests",
    ))
}

fn restrictive_subjects(
    before: &TransportModeGroup,
    requests: &[TransportModeRequest],
) -> Vec<TransportSubject> {
    let mut closed = Vec::new();
    let mut profile_after = before.profile.mode;
    for request in requests {
        if could_change(request, &before.profile) && request.mode() < before.profile.mode {
            profile_after = request.mode();
            closed.push(before.profile.subject);
        }
    }
    for child in &before.children {
        let requested = requests
            .iter()
            .find(|request| could_change(request, child))
            .map(|request| request.mode());
        let effective_before = child.mode.min(before.profile.mode);
        let effective_after = requested.unwrap_or(child.mode).min(profile_after);
        if effective_after < effective_before {
            closed.push(child.subject);
        }
    }
    closed
}

/// Whether `request` names `state` **and** could change it.
///
/// An initializer meeting an explicit row can never write, so fencing on it
/// would interrupt a transfer for an operation that was always going to be a
/// no-op. That mattered most while the evidence path ran on every pairing and
/// subscription event; D258 retired that caller, and the rule stays because a
/// group commit still carries initializer requests beside explicit ones.
fn could_change(request: &TransportModeRequest, state: &TransportModeState) -> bool {
    request.subject() == state.subject && (request.is_explicit() || !state.explicit_choice)
}

/// Subjects named by this group whose revision can no longer advance.
fn exhausted_subjects(
    before: &TransportModeGroup,
    requests: &[TransportModeRequest],
) -> Vec<TransportSubject> {
    requests
        .iter()
        .map(|request| request.subject())
        .filter(|subject| {
            std::iter::once(&before.profile)
                .chain(before.children.iter())
                .any(|state| state.subject == *subject && state.revision == i64::MAX)
        })
        .collect()
}
