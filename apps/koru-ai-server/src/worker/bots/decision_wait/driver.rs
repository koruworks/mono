// SPDX-License-Identifier: MPL-2.0
//! Writer-thread phases: exact replay, the lane job, then the writer receipt.
use super::jobs::Origin;
use super::*;

impl WorkerState {
    pub(in crate::worker) fn begin_decision_wait(&mut self, incoming: Incoming) {
        let (owner, epoch) = self.routine_recovery.owner_epoch();
        self.decision_waits.synchronize(owner, epoch);
        let Incoming {
            request,
            reply,
            context,
            permit,
        } = incoming;
        if let Err(code) = context.validate() {
            reply.send(Ok(BotWaitEnrollmentReply::Refused {
                code: bridge_refusal(code),
            }));
            return;
        }
        let Some(owner) = owner else {
            reply.send(Ok(BotWaitEnrollmentReply::Refused {
                code: BotWaitRefusal::Unavailable,
            }));
            return;
        };
        if !same_owner(owner, request.key()) {
            reply.send(Ok(BotWaitEnrollmentReply::Refused {
                code: BotWaitRefusal::Forbidden,
            }));
            return;
        }
        if self
            .decision_waits
            .active
            .values()
            .any(|active| active.key.request_id == context.request_id)
        {
            reply.send(Ok(BotWaitEnrollmentReply::Refused {
                code: BotWaitRefusal::Conflict,
            }));
            return;
        }
        let Some(serial) = self.decision_waits.serial.checked_add(1) else {
            reply.send(Ok(BotWaitEnrollmentReply::Refused {
                code: BotWaitRefusal::Unavailable,
            }));
            return;
        };
        self.decision_waits.serial = serial;
        let key = Key {
            request_id: context.request_id,
            serial,
            owner_epoch: epoch,
            wait: request.key().clone(),
            read: request.read(),
            phase: 0,
        };
        self.decision_waits.active.insert(
            serial,
            Active {
                key: key.clone(),
                cancellation: context.cancellation.clone(),
            },
        );
        let operation = Operation {
            key,
            request,
            reply,
            context,
            _permit: permit,
        };
        // Exact retained replay precedes the delegate read and fresh policy:
        // a historical receipt answers even after policy or terminal change.
        let read = ReadBotWaitCleanup {
            key: operation.key.wait.clone(),
        };
        match self.repository.read_bot_wait_cleanup(owner, &read) {
            Ok(enrolled @ BotWaitEnrollmentReply::Enrolled { .. }) => {
                self.answer_decision_wait(operation, enrolled)
            }
            Ok(BotWaitEnrollmentReply::Missing { .. }) => self.queue_decision_wait(operation),
            Ok(BotWaitEnrollmentReply::Refused { code }) => {
                self.refuse_decision_wait(operation, code)
            }
            Err(error) => self.refuse_decision_wait(operation, refusal(error)),
        }
    }

    fn queue_decision_wait(&mut self, mut operation: Operation) {
        if !operation.context.live() {
            self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
            return;
        }
        let Some(next) = operation.key.phase.checked_add(1) else {
            self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
            return;
        };
        operation.key.phase = next;
        if let Some(active) = self.decision_waits.active.get_mut(&operation.key.serial) {
            active.key = operation.key.clone();
        }
        // No installed delegate read (production until IPC16): refuse.
        let Some(delegate) = self.decision_waits.delegate() else {
            self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
            return;
        };
        let Some(pool) = &self.provider_admin else {
            self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
            return;
        };
        let read = operation.key.read;
        let job = DecisionWaitJob::for_operation(operation, delegate);
        let job = if read {
            ProviderAdminJob::DecisionWaitRead(job)
        } else {
            ProviderAdminJob::DecisionWaitMutation(job)
        };
        if let Err(error) = pool.try_submit(job) {
            let job = match error.job {
                ProviderAdminJob::DecisionWaitRead(job)
                | ProviderAdminJob::DecisionWaitMutation(job) => job,
                _ => unreachable!("decision wait lane classification"),
            };
            if let Origin::Operation(operation) = job.into_origin() {
                self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
            }
        }
    }

    fn answer_decision_wait(&mut self, operation: Operation, reply: BotWaitEnrollmentReply) {
        self.decision_waits.finish(&operation.key);
        // The receipt answers first and alone; the card follows in its own
        // transaction (O129) and its outcome changes nothing in this reply.
        let enrolled = matches!(reply, BotWaitEnrollmentReply::Enrolled { .. })
            .then(|| operation.key.wait.clone());
        operation.reply.send(Ok(reply));
        if let Some(key) = enrolled {
            let _ = self.ensure_bot_decision_card(&key);
        }
    }

    fn refuse_decision_wait(&mut self, operation: Operation, code: BotWaitRefusal) {
        self.decision_waits.finish(&operation.key);
        operation
            .reply
            .send(Ok(BotWaitEnrollmentReply::Refused { code }));
    }

    pub(in crate::worker) fn complete_decision_wait(&mut self, result: DecisionWaitResult) {
        let DecisionWaitResult {
            origin,
            wait,
            resume,
        } = result;
        match origin {
            Origin::Operation(operation) => self.complete_decision_operation(operation, wait),
            Origin::Chat { attempt, key, .. } => {
                self.complete_chat_decision_read(attempt, key, wait, resume)
            }
        }
    }

    fn complete_decision_operation(
        &mut self,
        operation: Operation,
        wait: Result<
            Option<koru_delegate_core::decisions::BotDecisionWaitObservation>,
            DecisionWaitPortError,
        >,
    ) {
        let (owner, epoch) = self.routine_recovery.owner_epoch();
        self.decision_waits.synchronize(owner, epoch);
        let owner = match owner {
            Some(owner)
                if self.decision_waits.current(&operation.key)
                    && operation.context.live()
                    && operation.context.request_id == operation.key.request_id
                    && *operation.request.key() == operation.key.wait =>
            {
                owner
            }
            // Late owner, changed epoch, expired or cancelled caller.
            _ => {
                self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
                return;
            }
        };
        let observed = match wait {
            Ok(observed) => observed,
            Err(DecisionWaitPortError::Unavailable) => {
                self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable);
                return;
            }
            Err(DecisionWaitPortError::Refused(code)) => {
                self.refuse_decision_wait(operation, code);
                return;
            }
        };
        // The delegate must positively know this exact wait before either a
        // fresh receipt or a Missing answer is possible.
        let Some(observed) = observed else {
            self.refuse_decision_wait(operation, BotWaitRefusal::Stale);
            return;
        };
        if observed.key != operation.key.wait {
            self.refuse_decision_wait(operation, BotWaitRefusal::Conflict);
            return;
        }
        let answer = match &operation.request {
            Request::Enroll(request) => {
                self.repository
                    .enroll_bot_wait_cleanup(owner, request, &observed, Timestamp::now())
            }
            Request::Read(request) => self.repository.read_bot_wait_cleanup(owner, request),
        };
        match answer {
            Ok(reply) if reply.matches_request(&operation.key.wait, operation.key.read) => {
                self.answer_decision_wait(operation, reply)
            }
            // Enroll cannot turn absence into a reply; nothing else matches.
            Ok(_) => self.refuse_decision_wait(operation, BotWaitRefusal::Unavailable),
            Err(error) => self.refuse_decision_wait(operation, refusal(error)),
        }
    }
}
