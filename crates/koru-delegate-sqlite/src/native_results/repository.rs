// SPDX-License-Identifier: MPL-2.0

use super::{rows::*, *};
use crate::{AttemptPhase, BotAdmissionRecord, BotLedger, RunsDb, WriteOp, attempts};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) const VERIFIED_RUN_QUERY: &str = "SELECT run_id,operation_id,attempt_id,generation,turn_id FROM native_result_intents WHERE run_id=?1 AND stage='verified' ORDER BY operation_id LIMIT 1";

pub(crate) fn policy_digest(barrier: &BotPolicyBarrier) -> Result<[u8; 32], BotLedgerError> {
    if !barrier.policy.valid()
        || barrier.config_revision == 0
        || barrier.config_revision > i64::MAX as u64
        || barrier.policy_revision == 0
        || barrier.policy_revision > i64::MAX as u64
    {
        return Err(BotLedgerError::Invalid);
    }
    let bytes = serde_json::to_vec(barrier).map_err(|_| BotLedgerError::Invalid)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"koru-native-policy-v1");
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}
pub(super) fn admission(db: &RunsDb, run: RunId) -> Result<BotAdmissionRecord, BotLedgerError> {
    routine::admission(db.conn(), run)
}
pub(super) fn current_attempt(
    conn: &Connection,
    key: &NativeResultKey,
    admission: &BotAdmissionRecord,
) -> Result<crate::AttemptRow, BotLedgerError> {
    routine::linked(conn, admission)?;
    let latest = attempts::latest(conn, key.run)?.ok_or(BotLedgerError::Fenced)?;
    let executing: bool = conn
        .query_row(
            "SELECT state='executing' FROM bot_admissions WHERE run_id=?1",
            [key.run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if !executing
        || latest.attempt != key.attempt
        || latest.generation != key.generation
        || latest.retired
        || latest.phase != AttemptPhase::Running
        || admission.initial_send.as_ref().and_then(|s| s.turn) != Some(key.turn)
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(latest)
}
pub(super) fn same_request(record: &NativeIntentRecord, request: &PrepareNativeResult) -> bool {
    record.key == request.key
        && record.kind == request.kind
        && record.destination == request.destination
        && record.fingerprint == request.request_fingerprint
        && record.expected_revision == request.expected_revision
        && record.requested_title == request.title
        && match request.kind {
            NativeResultOperation::CreatePage => request.target.is_none(),
            NativeResultOperation::EditPage => request.target == Some(record.target),
        }
}
pub(super) fn fresh_policy(
    conn: &Connection,
    key: &NativeResultKey,
    admitted: &BotAdmissionRecord,
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
) -> Result<(NativePolicyRecord, Option<crate::RoutinePolicyRecord>), BotLedgerError> {
    let fresh = &proof.global;
    let digest = policy_digest(fresh)?;
    let provenance = &admitted.submission.provenance;
    if fence.owner != provenance.owner
        || fence.bot != provenance.bot
        || fence.run != key.run
        || fence.attempt != key.attempt
        || fence.generation != key.generation
        || fence.turn != Some(key.turn)
        || fresh.key.owner != provenance.owner
        || fresh.key.bot != provenance.bot
    {
        return Err(BotLedgerError::Fenced);
    }
    let attempt = current_attempt(conn, key, admitted)?;
    if attempt.provider_session.and_then(|p| p.session) != Some(fence.session) {
        return Err(BotLedgerError::Fenced);
    }
    if fresh.config_revision < provenance.config_revision
        || fresh.policy_revision < provenance.policy_revision
    {
        return Err(BotLedgerError::Stale);
    }
    let current =
        current_policy(conn, provenance.owner, provenance.bot)?.ok_or(BotLedgerError::Fenced)?;
    if current.key != fresh.key
        || current.payload_digest != digest
        || current.config_revision != fresh.config_revision
        || current.policy_revision != fresh.policy_revision
        || current.state != BotPolicyBarrierState::Effective
        || !current.effects_allowed
    {
        return Err(BotLedgerError::Fenced);
    }
    let routine = routine::current(conn, admitted, proof)?;
    Ok((current, routine))
}
fn permits(policy: &CapturedBotPolicy, record: &NativeIntentRecord, created: bool) -> bool {
    policy.valid()
        && policy.workspace == record.destination
        && policy.destination == record.destination
        && (record.kind == NativeResultOperation::CreatePage
            || policy.access == BotAccess::Workspace
            || policy.selected_pages.contains(&record.target)
            || created)
}
fn evidence_matches(record: &NativeIntentRecord, evidence: &NativeResultReference) -> bool {
    evidence.valid()
        && evidence.operation_id == record.key.operation_id
        && evidence.workspace == record.destination
        && evidence.document == record.target
        && evidence.kind
            == match record.kind {
                NativeResultOperation::CreatePage => NativeResultKind::CreatedPage,
                NativeResultOperation::EditPage => NativeResultKind::EditedPage,
            }
}

impl NativeResultLedger for RunsDb {
    fn pending_routine_native_effects(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError> {
        super::drain::pending_routine(self.conn(), owner, bot, schedule, after, limit)
    }
    fn current_routine_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<Option<crate::RoutinePolicyRecord>, BotLedgerError> {
        self.current_routine_policy(owner, bot, schedule)
    }
    fn memory_edit_allocation(
        &self,
        run: RunId,
        operation: Uuid,
        caller_fingerprint: [u8; 32],
    ) -> Result<Option<MemoryEditAllocation>, BotLedgerError> {
        memory::lookup(self.conn(), run, operation, caller_fingerprint)
    }
    fn allocate_memory_edit(
        &mut self,
        request: &AllocateMemoryEditRequest,
        source_run_document: DocumentId,
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<MemoryEditAllocation, BotLedgerError> {
        memory::allocate(self, request, source_run_document, fence, fresh)
    }
    fn resolve_memory_edit_base(
        &mut self,
        key: &NativeResultKey,
        caller_fingerprint: [u8; 32],
        resolution: &MemoryBaseResolution,
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<MemoryEditAllocation, BotLedgerError> {
        memory::resolve(self, key, caller_fingerprint, resolution, fence, fresh)
    }
    fn prepare_memory_edit(
        &mut self,
        request: &PrepareNativeResult,
        caller_fingerprint: [u8; 32],
        authority: &MemoryEditAuthorityV1,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        memory::prepare(self, request, caller_fingerprint, authority)
    }
    fn pending_native_effects(
        &self,
        owner: BotOwner,
        bot: Option<BotId>,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError> {
        super::drain::pending(self, owner, bot, after, limit)
    }
    fn native_outcomes_unknown(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.conn().query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND stage='settled_unknown')", [run.to_storage()], |r|r.get(0)).map_err(sql)
    }
    fn native_effects_pending(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.conn().query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND (lease_state='held' OR stage='verified'))", [run.to_storage()], |row| row.get(0)).map_err(sql)
    }
    fn install_native_policy(
        &mut self,
        barrier: &BotPolicyBarrier,
    ) -> Result<NativePolicyRecord, BotLedgerError> {
        let digest = policy_digest(barrier)?;
        self.write_with_error(WriteOp::InstallNativePolicy,|tx,now| {
            if let Some(old)=read_policy(tx,&barrier.key)? {
                return if old.payload_digest==digest {Ok(old)} else {Err(BotLedgerError::Conflict)};
            }
            if let Some(current)=current_policy(tx,barrier.key.owner,barrier.key.bot)? {
                if barrier.config_revision==current.config_revision {return Err(BotLedgerError::Conflict);}
                if barrier.config_revision<current.config_revision || barrier.policy_revision<current.policy_revision {return Err(BotLedgerError::Stale);}
            }
            let state=if held(tx,barrier.key.owner,barrier.key.bot)? {"pending"} else {"effective"};
            tx.execute("INSERT INTO bot_policy_barriers(user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_digest,effects_allowed,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",params![barrier.key.owner.user_scope.to_storage(),barrier.key.owner.device.to_storage(),barrier.key.bot.to_storage(),barrier.key.request.to_storage(),barrier.config_revision,barrier.policy_revision,digest.as_slice(),barrier.effects_allowed,state,now]).map_err(sql)?;
            read_policy(tx,&barrier.key)?.ok_or(BotLedgerError::Invalid)
        })
    }
    fn native_policy(
        &self,
        key: &BotPolicyBarrierKey,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        read_policy(self.conn(), key)
    }
    fn current_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        current_policy(self.conn(), owner, bot)
    }
    fn settle_native_policy(
        &mut self,
        key: &BotPolicyBarrierKey,
    ) -> Result<NativePolicyRecord, BotLedgerError> {
        self.write_with_error(WriteOp::SettleNativePolicy,|tx,now|{
            let old=read_policy(tx,key)?.ok_or(BotLedgerError::Invalid)?;
            if old.state==BotPolicyBarrierState::Effective || held(tx,key.owner,key.bot)? {return Ok(old);}
            tx.execute("UPDATE bot_policy_barriers SET state='effective',updated_at_ms=?5 WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND request_id=?4",params![key.owner.user_scope.to_storage(),key.owner.device.to_storage(),key.bot.to_storage(),key.request.to_storage(),now]).map_err(sql)?;
            read_policy(tx,key)?.ok_or(BotLedgerError::Invalid)
        })
    }
    fn prepare_native_result(
        &mut self,
        request: &PrepareNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        if !request.valid() {
            return Err(BotLedgerError::Invalid);
        }
        // Immutable capture, decoded through its existing owner; no callback or
        // scheduling point intervenes before this exclusive writer transaction.
        let admitted = admission(self, request.key.run)?;
        self.write_with_error(WriteOp::PrepareNativeResult,|tx,now|{
            if memory::exists(tx,request.key.run,request.key.operation_id)? {return Err(BotLedgerError::Conflict);}
            if let Some(old)=read(tx,&request.key)? {return if matches!(old.purpose,NativeEffectPurpose::Result {}) && same_request(&old,request){Ok(old)}else{Err(BotLedgerError::Conflict)};}
            current_attempt(tx,&request.key,&admitted)?;
            if request.destination!=admitted.submission.policy.destination {return Err(BotLedgerError::Fenced);}
            let target=request.target.unwrap_or_else(||DocumentId::from_uuid(Uuid::new_v4()));
            let kind=match request.kind {NativeResultOperation::CreatePage=>"create_page",NativeResultOperation::EditPage=>"edit_page"};
            tx.execute("INSERT INTO native_result_intents(run_id,operation_id,attempt_id,generation,turn_id,kind,request_fingerprint,destination_workspace_id,target_document_id,store_request_id,expected_revision,requested_title,stage,lease_state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'prepared','none',?13,?13)",params![request.key.run.to_storage(),request.key.operation_id.to_string(),request.key.attempt.to_storage(),request.key.generation.get(),request.key.turn.to_string(),kind,request.request_fingerprint.as_slice(),request.destination.to_storage(),target.to_storage(),Uuid::new_v4().to_string(),request.expected_revision,request.title,now]).map_err(sql)?;
            required(tx,&request.key)
        })
    }
    fn lease_native_result(
        &mut self,
        key: &NativeResultKey,
        fingerprint: [u8; 32],
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<NativeLeaseOutcome, BotLedgerError> {
        let admitted = admission(self, key.run)?;
        self.write_with_error(WriteOp::LeaseNativeResult,|tx,now|{
            let record=read_admitted(tx,key,Some(&admitted))?.ok_or(BotLedgerError::Invalid)?;
            if record.fingerprint!=fingerprint {return Err(BotLedgerError::Conflict);}
            let provenance=&admitted.submission.provenance;
            if fence.owner!=provenance.owner || fence.bot!=provenance.bot || fence.run!=key.run || fence.attempt!=key.attempt || fence.generation!=key.generation || fence.turn!=Some(key.turn) {return Err(BotLedgerError::Fenced);}
            if record.stage!=NativeResultStage::Prepared || record.lease_state!=NativeLeaseState::None {return Ok(NativeLeaseOutcome {intent:record,execute:None});}
            let (current,companion)=fresh_policy(tx,key,&admitted,fence,fresh)?;
            let permissions=routine::current_permissions(fresh);
            match &record.purpose {
                NativeEffectPurpose::Result {} => {
                    let owned=if record.kind==NativeResultOperation::EditPage {created(tx,&admitted,record.destination,record.target)?}else{false};
                    if !permits(&admitted.submission.policy,&record,owned) || !permits(permissions,&record,owned) {return Err(BotLedgerError::Fenced);}
                }
                NativeEffectPurpose::Memory { authority } => {
                    if !authority.subject().matches(provenance) || authority.workspace!=record.destination
                        || authority.memory_document!=record.target
                        || !admitted.submission.policy.valid() || !permissions.valid()
                        || admitted.submission.policy.workspace!=authority.workspace
                        || permissions.workspace!=authority.workspace
                    {return Err(BotLedgerError::Fenced);}
                }
            }
            let id=Uuid::new_v4();
            if let Some(companion)=&companion { routine::insert(tx,key,companion)?; }
            tx.execute("UPDATE native_result_intents SET lease_id=?3,lease_user_scope=?4,lease_device_id=?5,lease_bot_id=?6,lease_barrier_request_id=?7,lease_config_revision=?8,lease_policy_revision=?9,lease_payload_digest=?10,lease_state='held',updated_at_ms=?11 WHERE run_id=?1 AND operation_id=?2",params![key.run.to_storage(),key.operation_id.to_string(),id.to_string(),current.key.owner.user_scope.to_storage(),current.key.owner.device.to_storage(),current.key.bot.to_storage(),current.key.request.to_storage(),current.config_revision,current.policy_revision,current.payload_digest.as_slice(),now]).map_err(sql)?;
            let intent=required(tx,key)?;
            Ok(NativeLeaseOutcome {intent,execute:Some(NativeEffectLease{id,policy:current,routine:companion})})
        })
    }
    fn native_result(
        &self,
        key: &NativeResultKey,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
        read(self.conn(), key)
    }
    fn pending_native_results(
        &self,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<Vec<PendingNativeResult>, BotLedgerError> {
        let (time, run, operation) = after
            .map(|c| {
                (
                    c.created_at_ms,
                    c.run.to_storage(),
                    c.operation_id.to_string(),
                )
            })
            .unwrap_or((i64::MIN, String::new(), String::new()));
        let mut statement=self.conn().prepare("SELECT run_id,operation_id,attempt_id,generation,turn_id FROM native_result_intents WHERE stage IN ('prepared','unconfirmed','verified') AND (created_at_ms,run_id,operation_id)>(?1,?2,?3) ORDER BY created_at_ms,run_id,operation_id LIMIT ?4").map_err(sql)?;
        let rows = statement
            .query_map(params![time, run, operation, limit.clamp(1, 256)], |row| {
                Ok(key(row))
            })
            .map_err(sql)?;
        rows.map(|r| {
            let key = r.map_err(sql)??;
            let record = required(self.conn(), &key)?;
            Ok(PendingNativeResult {
                purpose: record.purpose,
                cursor: NativeRecoveryCursor {
                    created_at_ms: record.created_at_ms,
                    run: key.run,
                    operation_id: key.operation_id,
                },
                key,
                stage: record.stage,
                lease_state: record.lease_state,
            })
        })
        .collect()
    }
    fn verified_native_result(
        &self,
        run: RunId,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
        let key = self
            .conn()
            .query_row(VERIFIED_RUN_QUERY, [run.to_storage()], |row| Ok(key(row)))
            .optional()
            .map_err(sql)?
            .transpose()?;
        key.map(|key| required(self.conn(), &key)).transpose()
    }
    fn refuse_unleased_native_result(
        &mut self,
        key: &NativeResultKey,
        refusal: NativeResultRefusal,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.write_with_error(WriteOp::RefuseUnleasedNativeResult,|tx,now|{
            let record=required(tx,key)?;
            if record.lease_state!=NativeLeaseState::None {return Err(BotLedgerError::Fenced);}
            if record.stage==NativeResultStage::Refused {return if record.refusal==Some(refusal){Ok(record)}else{Err(BotLedgerError::Conflict)};}
            if record.stage!=NativeResultStage::Prepared {return Err(BotLedgerError::Fenced);}
            tx.execute("UPDATE native_result_intents SET stage='refused',refusal=?3,updated_at_ms=?4 WHERE run_id=?1 AND operation_id=?2",params![key.run.to_storage(),key.operation_id.to_string(),refusal_name(refusal),now]).map_err(sql)?;
            required(tx,key)
        })
    }
    fn confirm_native_result(
        &mut self,
        request: &ConfirmNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.write_with_error(WriteOp::ConfirmNativeResult,|tx,now|{
            let record=required(tx,&request.key)?;
            if record.fingerprint!=request.request_fingerprint {return Err(BotLedgerError::Conflict);}
            if record.lease.is_none(){return Err(BotLedgerError::Fenced);}
            match &request.observation {
                NativeResultObservation::DrainedUnknown { receipt } => super::drain::confirm(tx,&record,receipt,now)?,
                NativeResultObservation::Unconfirmed => {
                    if record.lease_state==NativeLeaseState::Held {tx.execute("UPDATE native_result_intents SET stage='unconfirmed',updated_at_ms=?3 WHERE run_id=?1 AND operation_id=?2",params![request.key.run.to_storage(),request.key.operation_id.to_string(),now]).map_err(sql)?;}
                }
                NativeResultObservation::NotApplied{code} => {
                    if record.stage==NativeResultStage::SettledUnknown {return Ok(record);}
                    if record.stage==NativeResultStage::Refused {return if record.refusal==Some(*code){Ok(record)}else{Err(BotLedgerError::Conflict)};}
                    if record.verified.is_some(){return Err(BotLedgerError::Conflict);}
                    tx.execute("UPDATE native_result_intents SET stage='refused',lease_state='settled',refusal=?3,updated_at_ms=?4 WHERE run_id=?1 AND operation_id=?2",params![request.key.run.to_storage(),request.key.operation_id.to_string(),refusal_name(*code),now]).map_err(sql)?;
                }
                NativeResultObservation::Durable{evidence} => {
                    if !evidence_matches(&record,evidence) {return Err(BotLedgerError::Conflict);}
                    if let Some(old)=&record.verified {return if old==evidence {Ok(record)}else{Err(BotLedgerError::Conflict)};}
                    if record.stage==NativeResultStage::Refused {return Err(BotLedgerError::Conflict);}
                    tx.execute("UPDATE native_result_intents SET stage='verified',lease_state='settled',verified_version=?3,verified_title=?4,confirmed_at_ms=?5,updated_at_ms=?6 WHERE run_id=?1 AND operation_id=?2",params![request.key.run.to_storage(),request.key.operation_id.to_string(),evidence.version,evidence.title,evidence.confirmed_at_ms,now]).map_err(sql)?;
                }
            }
            required(tx,&request.key)
        })
    }
    fn register_native_result(
        &mut self,
        key: &NativeResultKey,
        evidence: &NativeResultReference,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.write_with_error(WriteOp::RegisterNativeResult,|tx,now|{
            let record=required(tx,key)?;
            if record.verified.as_ref()!=Some(evidence) {return Err(BotLedgerError::Conflict);}
            if record.stage==NativeResultStage::Registered {return Ok(record);}
            if record.stage!=NativeResultStage::Verified {return Err(BotLedgerError::Fenced);}
            tx.execute("UPDATE native_result_intents SET stage='registered',registered_at_ms=?3,updated_at_ms=?3 WHERE run_id=?1 AND operation_id=?2",params![key.run.to_storage(),key.operation_id.to_string(),now]).map_err(sql)?;
            required(tx,key)
        })
    }
    fn created_native_page(
        &self,
        execution: &BotRunKey,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError> {
        let admitted = admission(self, execution.run)?;
        let p = &admitted.submission.provenance;
        if p.owner != execution.owner
            || p.bot != execution.bot
            || admitted.submission.workspace != workspace
        {
            return Err(BotLedgerError::Fenced);
        }
        created(self.conn(), &admitted, workspace, document)
    }
}
