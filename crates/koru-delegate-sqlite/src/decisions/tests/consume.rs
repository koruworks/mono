// SPDX-License-Identifier: MPL-2.0
use super::*;
use lifecycle::expected;

pub(super) fn ready()->(Harness,DecisionRecord,PrepareNativeResult,BotExecutionFence,Vec<u8>){
    let (mut h,record,_clock)=lifecycle::koru_enrollment_ready();
    let record=lifecycle::enrolled(&mut h,&record);
    let release=lifecycle::origin_release(&mut h,&record,1000);
    let record=h.db.record_decision_origin_stop(&expected(&record),DecisionTransitionId::mint(),&release).unwrap();
    let input=h.answer_input(&record,true);
    let record=h.db.answer_bot_decision(&input,DecisionTransitionId::mint(),&h.proof,h.boot).unwrap();
    let host=HostInstanceId::mint();
    let record=h.db.reserve_decision_successor(&expected(&record),DecisionTransitionId::mint(),host,[42;32],&h.proof,h.boot).unwrap();
    let resume=record.resume.as_ref().unwrap();
    let (attempt,generation,_,creation)=resume.consumer_identity();
    let session=SessionId::mint();
    let record=h.db.bind_decision_successor(&expected(&record),DecisionTransitionId::mint(),creation,host,session).unwrap();
    h.db.advance(h.origin().run,generation,crate::AttemptPhase::Running).unwrap();
    let resume=record.resume.as_ref().unwrap();
    let turn=AgentTurnId::mint();
    let record=h.db.record_decision_resume_input(&expected(&record),DecisionTransitionId::mint(),resume.input_key,resume.input_fingerprint,Some(&DecisionInputAcknowledgment{turn,receipt_digest:DecisionDigest::from_bytes([43;32])})).unwrap();
    let DecisionBindingV1::KoruEdit(edit)=&record.receipt.binding else {panic!("edit")};
    let request=PrepareNativeResult {
        key:NativeResultKey{run:edit.run,attempt,generation,turn,operation_id:edit.operation_id},kind:NativeResultOperation::EditPage,
        destination:edit.workspace,target:Some(edit.target),request_fingerprint:edit.edit_fingerprint,
        // Different map order, same semantic revision as the retained binding.
        expected_revision:Some(vec![2,2,4,1,2]),config_revision:edit.global_policy.config_revision.get(),policy_revision:edit.global_policy.policy_revision.get(),title:"Original edit".into(),
    };
    let fence=BotExecutionFence{owner:edit.owner,bot:edit.bot,run:edit.run,attempt,generation,session,turn:Some(turn)};
    let payload=edit.payload.as_slice().to_vec();
    (h,record,request,fence,payload)
}

#[test]
fn decision_consume_is_atomic_across_native_lease_companion_receipt_and_control(){
    for moment in crate::Moment::ALL {
        let (mut h,record,request,fence,payload)=ready();
        let transition=DecisionTransitionId::mint();
        h.db.arm_crash(crate::CrashPoint{op:crate::WriteOp::RecordDecisionProjection,at:moment});
        assert!(h.db.consume_decision_edit(&expected(&record),transition,&request,&payload,&fence,&h.proof,h.boot).is_err());
        let committed=moment==crate::Moment::AfterCommit;
        assert_eq!(h.read(record.receipt.decision).unwrap().receipt.state.authorization==DecisionAuthorization::Consumed,committed);
        assert_eq!(h.db.native_result(&request.key).unwrap().is_some(),committed);
        let companions:i64=h.db.conn().query_row("SELECT count(*) FROM native_decision_leases",[],|r|r.get(0)).unwrap();
        assert_eq!(companions,i64::from(committed));
        assert_eq!(h.db.decision_projection(fence.owner,fence.run,transition).unwrap().is_some(),committed);
        h.db.restart_in_memory();
        let retried=h.db.consume_decision_edit(&expected(&record),transition,&request,&payload,&fence,&h.proof,h.boot).unwrap();
        assert_eq!(retried.execute.is_some(),!committed);
        assert!(h.db.consume_decision_edit(&expected(&record),transition,&request,&payload,&fence,&h.proof,h.boot).unwrap().execute.is_none());
    }
}

#[test]
fn decision_consume_replays_first_key_after_revocation_and_rejects_changed_body(){
    let (mut h,record,request,fence,payload)=ready();
    let transition=DecisionTransitionId::mint();
    let first=h.db.consume_decision_edit(&expected(&record),transition,&request,&payload,&fence,&h.proof,h.boot).unwrap();
    assert!(first.execute.is_some());
    h.revoke();
    let mut later=request.clone();later.key.attempt=RunAttemptId::mint();later.key.generation=AttemptGeneration::from_raw(99).unwrap();later.key.turn=AgentTurnId::mint();
    let replay=h.db.consume_decision_edit(&expected(&record),transition,&later,&payload,&fence,&h.proof,DelegateBootId::mint()).unwrap();
    assert_eq!(replay.intent,first.intent);assert!(replay.execute.is_none());
    assert!(h.db.consume_decision_edit(&expected(&record),transition,&later,b"changed",&fence,&h.proof,h.boot).is_err());
    later.title="changed title".into();
    assert!(h.db.consume_decision_edit(&expected(&record),transition,&later,&payload,&fence,&h.proof,h.boot).is_err());
    let mut foreign=fence.clone();foreign.bot=BotId::mint();
    assert!(h.db.consume_decision_edit(&expected(&record),transition,&request,&payload,&foreign,&h.proof,h.boot).is_err());
}

#[test]
fn decision_consume_refuses_legacy_prepared_and_wrong_current_consumer(){
    let (mut h,record,request,fence,payload)=ready();
    let transition=DecisionTransitionId::mint();
    let mut wrong=request.clone();wrong.key.turn=AgentTurnId::mint();
    assert!(h.db.consume_decision_edit(&expected(&record),transition,&wrong,&payload,&fence,&h.proof,h.boot).is_err());
    wrong=request.clone();wrong.expected_revision=Some(vec![0xff]);
    assert!(h.db.consume_decision_edit(&expected(&record),transition,&wrong,&payload,&fence,&h.proof,h.boot).is_err());
    // An intentional collision fixture models a legacy intent; production
    // prepare refuses this overlap in either direction.
    crate::native_results::insert_prepared_on(h.db.conn(),&request,1000).unwrap();
    let store:String=h.db.conn().query_row("SELECT store_request_id FROM native_result_intents WHERE run_id=?1",[fence.run.to_storage()],|r|r.get(0)).unwrap();
    assert!(h.db.consume_decision_edit(&expected(&record),transition,&request,&payload,&fence,&h.proof,h.boot).is_err());
    assert_eq!(h.db.conn().query_row("SELECT store_request_id FROM native_result_intents WHERE run_id=?1",[fence.run.to_storage()],|r|r.get::<_,String>(0)).unwrap(),store);
    assert_eq!(h.read(record.receipt.decision).unwrap().receipt.state.authorization,DecisionAuthorization::Granted);
}

#[test]
fn decision_resolved_effect_requires_publication_then_releases_only_wait_for_continuation(){
    let (mut h,record,request,fence,payload)=ready();
    let initial=h.db.bot_admission(fence.run).unwrap().unwrap().initial_send;
    h.db.set_bot_active(fence.run,fence.attempt,fence.generation,true,Some(&h.proof)).unwrap();
    h.db.consume_decision_edit(&expected(&record),DecisionTransitionId::mint(),&request,&payload,&fence,&h.proof,h.boot).unwrap();
    let mut unrelated=request.clone();unrelated.key.operation_id=uuid::Uuid::new_v4();
    assert!(h.db.prepare_native_result(&unrelated).is_err());
    h.db.confirm_native_result(&ConfirmNativeResult{key:request.key.clone(),request_fingerprint:request.request_fingerprint,observation:NativeResultObservation::NotApplied{code:NativeResultRefusal::Conflict}}).unwrap();
    let consumed=h.read(record.receipt.decision).unwrap();
    let outcome=DecisionRetainedEffect::NotApplied{key:request.key.clone(),code:NativeResultRefusal::Conflict};
    let transition=DecisionTransitionId::mint();
    let settled=h.db.settle_decision_effect(&expected(&consumed),transition,&outcome).unwrap();
    assert!(h.db.prepare_native_result(&unrelated).is_err());
    let pending=h.db.decision_projection(fence.owner,fence.run,transition).unwrap().unwrap();
    let expected_wait=h.db.decision_wait_execution(fence.owner,fence.run).unwrap().unwrap().expected;
    let prepared=projection::prepared(&pending);
    let ack=DecisionProjectionAcknowledgment{identity:pending.identity.clone(),delta_digest:prepared.delta_digest,observed_version:DecisionVersionVector::new(vec![3]).unwrap()};
    assert!(h.db.settle_decision_wait(&expected_wait,&ack).is_err());
    projection::apply(&mut h,&pending);
    h.db.settle_decision_wait(&expected_wait,&ack).unwrap();
    assert!(h.db.decision_wait_execution(fence.owner,fence.run).unwrap().is_none());
    assert_eq!(h.db.bot_admission(fence.run).unwrap().unwrap().state,BotAdmissionState::Executing);
    assert_eq!(h.db.bot_admission(fence.run).unwrap().unwrap().initial_send,initial);
    assert!(!h.db.current_attempt(fence.run).unwrap().unwrap().retired);
    assert_eq!(h.db.bot_execution_inventory(fence.owner.device).unwrap().occupied,1);
    h.db.prepare_native_result(&unrelated).unwrap();
    let mut wrong=unrelated;wrong.key.operation_id=uuid::Uuid::new_v4();wrong.key.turn=AgentTurnId::mint();
    assert!(h.db.prepare_native_result(&wrong).is_err());
    assert_eq!(h.db.settle_decision_effect(&expected(&consumed),transition,&outcome).unwrap().receipt,settled.receipt);
}
