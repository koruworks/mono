// SPDX-License-Identifier: MPL-2.0

//! The crash-injection seam: a named point at which a write dies.
//!
//! Every consequential write goes through one transaction helper
//! ([`crate::RunsDb`]'s `write`), and that helper asks this seam three
//! times: before the statements run, after they ran but before the commit,
//! and after the commit but before the caller is answered. A test arms one
//! point; when the helper reaches it the transaction is dropped uncommitted
//! (or, after the commit, the result is withheld) and the ledger goes dead —
//! every later call answers with the same point, the way a crashed process
//! answers nothing. The test then reopens the file, which is the restart,
//! and asserts the state §7.1 promises.
//!
//! Compiled only under `cfg(test)` and the `crash-points` feature, which
//! nothing shipped enables. [`WriteOp`] is unconditional because the helper
//! names its write either way.

#[cfg(any(test, feature = "crash-points"))]
use crate::LedgerError;

/// A consequential write. One per repository method that changes a row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WriteOp {
    RecordIntent,
    SatisfyIntent,
    AbandonIntent,
    OpenAttempt,
    AdvanceAttempt,
    BindSession,
    RetireAttempt,
    Enqueue,
    Acknowledge,
    BumpAttempts,
    PrepareSchedulePublication,
    AbandonSchedulePublication,
    MarkSchedulePublicationReady,
    AcknowledgeSchedulePublication,
    BumpSchedulePublicationAttempts,
    AdvanceCursor,
    ClaimSlot,
    ClaimScheduledRun,
    ClaimSkip,
    RecordOutcome,
    /// A person put a run in the trash and said what to do with what it
    /// produced.
    RecordDeletion,
    /// …and took it back out.
    ClearDeletion,
    /// The trash was emptied: every row of the run leaves the ledger.
    ForgetRun,
}

impl WriteOp {
    pub const ALL: [Self; 23] = [
        Self::RecordIntent,
        Self::SatisfyIntent,
        Self::AbandonIntent,
        Self::OpenAttempt,
        Self::AdvanceAttempt,
        Self::BindSession,
        Self::RetireAttempt,
        Self::Enqueue,
        Self::Acknowledge,
        Self::BumpAttempts,
        Self::PrepareSchedulePublication,
        Self::AbandonSchedulePublication,
        Self::MarkSchedulePublicationReady,
        Self::AcknowledgeSchedulePublication,
        Self::BumpSchedulePublicationAttempts,
        Self::AdvanceCursor,
        Self::ClaimSlot,
        Self::ClaimScheduledRun,
        Self::ClaimSkip,
        Self::RecordOutcome,
        Self::RecordDeletion,
        Self::ClearDeletion,
        Self::ForgetRun,
    ];
}

/// When, relative to the write, the process dies.
#[cfg(any(test, feature = "crash-points"))]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Moment {
    /// Nothing reached the file.
    BeforeWrite,
    /// The statements ran inside the transaction; it is never committed. The
    /// file must show nothing of them.
    AfterWriteBeforeCommit,
    /// The commit returned; the caller never heard. The file holds the fact
    /// and the caller's retry must find it rather than make it twice.
    AfterCommit,
}

#[cfg(any(test, feature = "crash-points"))]
impl Moment {
    pub const ALL: [Self; 3] = [
        Self::BeforeWrite,
        Self::AfterWriteBeforeCommit,
        Self::AfterCommit,
    ];
}

#[cfg(any(test, feature = "crash-points"))]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CrashPoint {
    pub op: WriteOp,
    pub at: Moment,
}

/// The armed point, and the one that fired. Once fired, every check fails.
#[cfg(any(test, feature = "crash-points"))]
#[derive(Default)]
pub(crate) struct CrashArm {
    armed: Option<CrashPoint>,
    fired: Option<CrashPoint>,
}

#[cfg(any(test, feature = "crash-points"))]
impl CrashArm {
    pub(crate) fn arm(&mut self, point: CrashPoint) {
        self.armed = Some(point);
    }

    pub(crate) fn check(&mut self, op: WriteOp, at: Moment) -> Result<(), LedgerError> {
        if let Some(fired) = self.fired {
            return Err(LedgerError::Crashed(fired));
        }
        let point = CrashPoint { op, at };
        if self.armed == Some(point) {
            self.fired = Some(point);
            self.armed = None;
            return Err(LedgerError::Crashed(point));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
