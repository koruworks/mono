// SPDX-License-Identifier: MPL-2.0
//! The finite delegate read, run on an existing pool lane. No writer access.
use super::*;
use koru_delegate_api::routines::ROUTINE_BRIDGE_DEADLINE;
use koru_delegate_core::decisions::{BotDecisionWaitObservation, DecisionResume};

type WaitRead = Result<Option<BotDecisionWaitObservation>, DecisionWaitPortError>;
type ResumeRead = Result<Option<DecisionResume>, DecisionWaitPortError>;

/// Who is owed the completion.
pub(super) enum Origin {
    /// An authenticated HTTP operation, with its own reply and permit.
    Operation(Operation),
    /// A live Bot task enrolled in this wait (origin proof, held wait or
    /// consumer). `resume` also reads the retained resume row.
    Chat {
        attempt: InferenceAttemptId,
        key: BotWaitEnrollmentKey,
        resume: bool,
    },
}

pub(in crate::worker) struct DecisionWaitJob {
    pub(super) origin: Origin,
    subject: DecisionWaitSubject,
    deadline: Instant,
    cancellation: Cancellation,
    delegate: Arc<dyn DecisionWaitDelegate>,
}
pub(in crate::worker) struct DecisionWaitResult {
    pub(super) origin: Origin,
    pub(super) wait: WaitRead,
    pub(super) resume: Option<ResumeRead>,
}

impl DecisionWaitJob {
    pub(super) fn for_operation(
        operation: Operation,
        delegate: Arc<dyn DecisionWaitDelegate>,
    ) -> Self {
        Self {
            subject: DecisionWaitSubject::of(&operation.key.wait),
            deadline: operation.context.deadline,
            cancellation: operation.context.cancellation.clone(),
            origin: Origin::Operation(operation),
            delegate,
        }
    }
    /// A chat read has its own single five-second budget and no caller.
    pub(in crate::worker) fn for_chat(
        attempt: InferenceAttemptId,
        key: BotWaitEnrollmentKey,
        resume: bool,
        delegate: Arc<dyn DecisionWaitDelegate>,
    ) -> Self {
        Self {
            subject: DecisionWaitSubject::of(&key),
            deadline: Instant::now() + ROUTINE_BRIDGE_DEADLINE,
            cancellation: Cancellation::default(),
            origin: Origin::Chat {
                attempt,
                key,
                resume,
            },
            delegate,
        }
    }
    pub(super) fn into_origin(self) -> Origin {
        self.origin
    }
    fn remaining(&self) -> Result<Duration, DecisionWaitPortError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || self.cancellation.is_cancelled() {
            return Err(DecisionWaitPortError::Unavailable);
        }
        Ok(remaining)
    }
    /// Only an exactly matching, validated observation counts as an answer.
    fn read_wait(&self) -> WaitRead {
        let observed = self
            .delegate
            .read_decision_wait(&self.subject, self.remaining()?)?;
        match observed {
            None => Ok(None),
            Some(observed)
                if observed.validate().is_ok()
                    && DecisionWaitSubject::of(&observed.key) == self.subject =>
            {
                Ok(Some(observed))
            }
            Some(_) => Err(DecisionWaitPortError::Refused(BotWaitRefusal::Conflict)),
        }
    }
    fn read_resume(&self) -> ResumeRead {
        let resume = self
            .delegate
            .read_decision_resume(&self.subject, self.remaining()?)?;
        match resume {
            None => Ok(None),
            Some(resume)
                if resume.validate().is_ok()
                    && resume.decision == self.subject.decision_id
                    && resume.origin.owner == self.subject.owner
                    && resume.origin.bot == self.subject.bot
                    && resume.origin.run == self.subject.run =>
            {
                Ok(Some(resume))
            }
            Some(_) => Err(DecisionWaitPortError::Refused(BotWaitRefusal::Conflict)),
        }
    }
    pub(in crate::worker) fn run(self) -> DecisionWaitResult {
        let reads = catch_unwind(AssertUnwindSafe(|| {
            let wait = self.read_wait();
            let resume = matches!(self.origin, Origin::Chat { resume: true, .. })
                .then(|| self.read_resume());
            (wait, resume)
        }));
        let (wait, resume) = reads.unwrap_or((Err(DecisionWaitPortError::Unavailable), None));
        DecisionWaitResult {
            origin: self.origin,
            wait,
            resume,
        }
    }
}
