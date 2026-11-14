// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) struct Fresh {
    pub bot: Bot,
    pub creation: BotRoutineCreationIdentity,
    pub canonical: BotRoutineCanonical,
    pub profile: koru_ai_core::ProfileId,
    pub library: library::LibrarySources,
}
pub(super) enum Phase {
    PreclaimRead(Box<preclaim::Snapshot>),
    PreclaimInstall(Box<preclaim::Install>),
    Capture(Box<Fresh>),
    Observe {
        home: WorkspaceId,
        claim: bool,
    },
    Acknowledge {
        home: WorkspaceId,
        acknowledgement: RoutineLinkAcknowledgement,
    },
}
pub(in crate::worker) struct OccurrenceJob {
    pub(super) operation: Operation,
    pub(super) phase: Phase,
    pub(super) authority: Arc<dyn BotAuthority>,
    pub(super) delegate: Arc<dyn BotDelegatePort>,
}
pub(super) enum Evidence {
    PreclaimRead(Box<preclaim::Hint>),
    PreclaimInstalled(Box<preclaim::Install>),
    Captured(Box<BotRoutineCapturedSources>),
    Observed {
        authority: BotRoutineFinalizeAuthority,
        observation: Option<RoutineClaimObservation>,
    },
    Acknowledged {
        authority: BotRoutineFinalizeAuthority,
        record: RoutineClaimRecord,
        admission: Option<koru_delegate_core::BotAdmissionView>,
    },
}
pub(in crate::worker) struct OccurrenceResult {
    pub(super) operation: Operation,
    pub(super) evidence: Result<Evidence, RoutineBridgeRefusal>,
}

impl OccurrenceJob {
    pub(in crate::worker) fn run(self, agents: Option<&AgentSessions>) -> OccurrenceResult {
        let evidence = catch_unwind(AssertUnwindSafe(|| self.execute(agents)))
            .unwrap_or(Err(RoutineBridgeRefusal::Unavailable));
        OccurrenceResult {
            operation: self.operation,
            evidence,
        }
    }
    pub(super) fn live(&self) -> Result<(), RoutineBridgeRefusal> {
        self.operation
            .context
            .live()
            .then_some(())
            .ok_or(RoutineBridgeRefusal::Unavailable)
    }
    pub(super) fn current(
        &self,
        home: WorkspaceId,
    ) -> Result<BotRoutineFinalizeAuthority, RoutineBridgeRefusal> {
        self.live()?;
        let proof = self.authority.inspect(&[home], &[]).map_err(refusal)?;
        if proof.owner != ai_owner(self.operation.key.slot) {
            return Err(RoutineBridgeRefusal::Forbidden);
        }
        let profile = proof.workspace(home).map_err(refusal)?;
        self.live()?;
        Ok(BotRoutineFinalizeAuthority {
            owner: self.operation.key.slot.owner,
            home,
            profile,
        })
    }
    fn unchanged(&self, before: &BotRoutineFinalizeAuthority) -> Result<(), RoutineBridgeRefusal> {
        let after = self.current(before.home)?;
        if after.owner != before.owner || after.profile != before.profile {
            return Err(RoutineBridgeRefusal::Forbidden);
        }
        Ok(())
    }
    pub(super) fn call(&self, command: DelegateCommand) -> Result<BotReply, RoutineBridgeRefusal> {
        self.live()?;
        let remaining = self
            .operation
            .context
            .deadline
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RoutineBridgeRefusal::Unavailable);
        }
        self.delegate
            .execute_within(command, remaining)
            .map_err(|_| RoutineBridgeRefusal::Unavailable)
    }
    fn observe(&self) -> Result<RoutineClaimObservation, RoutineBridgeRefusal> {
        let slot = self.operation.key.slot;
        match self.call(DelegateCommand::ReadRoutineClaim { slot })? {
            BotReply::RoutineClaim { observation }
                if observation.valid() && *observation.slot() == slot =>
            {
                Ok(observation)
            }
            _ => Err(RoutineBridgeRefusal::Conflict),
        }
    }
    fn execute(&self, agents: Option<&AgentSessions>) -> Result<Evidence, RoutineBridgeRefusal> {
        match &self.phase {
            Phase::PreclaimRead(snapshot) => self.preclaim_read(snapshot),
            Phase::PreclaimInstall(install) => self.preclaim_install(install),
            Phase::Capture(fresh) => self.capture(fresh, agents),
            Phase::Observe { home, claim } => {
                let authority = self.current(*home)?;
                let observation = if *claim { Some(self.observe()?) } else { None };
                self.unchanged(&authority)?;
                Ok(Evidence::Observed {
                    authority,
                    observation,
                })
            }
            Phase::Acknowledge {
                home,
                acknowledgement,
            } => {
                let authority = self.current(*home)?;
                // A failed finite IPC has unknown remote completion. Re-read the
                // same identity; never resend a mutation in this job.
                let record = match self.call(DelegateCommand::AcknowledgeRoutineLink {
                    acknowledgement: acknowledgement.clone(),
                }) {
                    Ok(BotReply::RoutineLinkAcknowledged { record }) => record,
                    Ok(_) => return Err(RoutineBridgeRefusal::Conflict),
                    Err(_) => match self.observe()? {
                        RoutineClaimObservation::Claimed { record } => record,
                        _ => return Err(RoutineBridgeRefusal::Unavailable),
                    },
                };
                if !record.valid() || record.claim != acknowledgement.claim {
                    return Err(RoutineBridgeRefusal::Conflict);
                }
                let admission = match record.link_state {
                    RoutineLinkState::Held {} => return Err(RoutineBridgeRefusal::Unavailable),
                    RoutineLinkState::Terminal { .. } => None,
                    RoutineLinkState::Linked {} => {
                        if acknowledgement.outcome != (RoutineLinkOutcome::Linked {}) {
                            return Err(RoutineBridgeRefusal::Conflict);
                        }
                        let key = koru_delegate_core::BotSubmissionKey {
                            owner: record.claim.slot.owner,
                            bot: record.claim.slot.bot,
                            request: record.claim.ids.submission,
                        };
                        match self.call(DelegateCommand::Reconcile { key: key.clone() })? {
                            BotReply::Admission { admission }
                                if admission.key == key
                                    && admission.run == record.claim.ids.run =>
                            {
                                Some(admission)
                            }
                            _ => return Err(RoutineBridgeRefusal::Unavailable),
                        }
                    }
                };
                self.unchanged(&authority)?;
                Ok(Evidence::Acknowledged {
                    authority,
                    record,
                    admission,
                })
            }
        }
    }
    fn capture(
        &self,
        fresh: &Fresh,
        agents: Option<&AgentSessions>,
    ) -> Result<Evidence, RoutineBridgeRefusal> {
        let Request::Prepare(request) = &self.operation.request else {
            return Err(RoutineBridgeRefusal::Invalid);
        };
        let proof = self.current(fresh.canonical.home)?;
        if proof.profile != fresh.profile || request.workspace != proof.home {
            return Err(RoutineBridgeRefusal::Forbidden);
        }
        validate_model_evidence(agents, &fresh.canonical.config, false)
            .map_err(|_| RoutineBridgeRefusal::Unavailable)?;
        self.live()?;
        self.authority
            .verify_routine_definition(
                ai_owner(request.slot),
                proof.profile,
                &fresh.creation,
                &request.schedule_definition,
                request.owner_handoff,
                &self.operation.context.cancellation,
            )
            .map_err(refusal)?;
        self.live()?;
        let pins = self
            .authority
            .capture(
                ai_owner(request.slot),
                proof.home,
                proof.profile,
                &fresh.library.pages(),
            )
            .map_err(refusal)?;
        self.live()?;
        let task = BotTask {
            text: request
                .schedule_definition
                .template
                .objective
                .as_str()
                .into(),
            documents: fresh.library.task_documents.clone(),
        };
        fresh
            .library
            .validate(&fresh.bot, &task, &pins)
            .map_err(|_| RoutineBridgeRefusal::Conflict)?;
        let pin = |page: &BotPage| -> Result<BotKnowledgeCapture, RoutineBridgeRefusal> {
            let version = pins
                .iter()
                .find_map(|item| match item {
                    koru_delegate_core::RunContextItem::Document {
                        workspace,
                        document,
                        version,
                    } if *workspace == page.workspace && *document == page.document => {
                        Some(*version)
                    }
                    _ => None,
                })
                .ok_or(RoutineBridgeRefusal::Conflict)?;
            Ok(BotKnowledgeCapture {
                page: page.clone(),
                version,
            })
        };
        let (memory, index_digest) = if let Some(binding) = &fresh.library.memory_binding {
            let captured = self
                .authority
                .capture_memory(
                    ai_owner(request.slot),
                    proof.profile,
                    binding,
                    &self.operation.context.cancellation,
                )
                .map_err(|_| RoutineBridgeRefusal::Unavailable)?;
            captured.validate_for_binding(binding).map_err(refusal)?;
            if captured.owner != ai_owner(request.slot) || captured.bot != request.slot.bot {
                return Err(RoutineBridgeRefusal::Forbidden);
            }
            (
                BotCapturedMemory::Captured {
                    page: captured.page,
                    version: captured.version,
                    creation_request: captured.creation_request,
                    nodes: captured.nodes,
                    omitted_count: captured.omitted_count,
                    invalid_count: captured.invalid_count,
                },
                captured.index_digest,
            )
        } else {
            (BotCapturedMemory::Absent {}, *blake3::hash(&[]).as_bytes())
        };
        let captured = BotRoutineCapturedSources {
            slot: request.slot,
            definition: request.schedule_definition.clone(),
            owner_handoff: request.owner_handoff,
            creation: fresh.creation.route(),
            canonical: fresh.canonical.clone(),
            profile: proof.profile,
            grant: fresh.creation.grant.clone(),
            role: pin(&fresh.library.role)?,
            knowledge: fresh
                .library
                .knowledge
                .iter()
                .map(pin)
                .collect::<Result<_, _>>()?,
            task: fresh
                .library
                .task_documents
                .iter()
                .map(pin)
                .collect::<Result<_, _>>()?,
            memory,
            index_digest,
        };
        self.unchanged(&proof)?;
        Ok(Evidence::Captured(Box::new(captured)))
    }
}
