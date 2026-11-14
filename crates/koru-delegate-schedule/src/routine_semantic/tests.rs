// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;

const DEFINITION: &str = r#"{"revision":1,"effective_at_ms":1000,"template":{"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000001"},"objective":"task","agent":{"kind":"built_in","value":"codex"},"requested_model":null,"scope":{"kind":"temporary"},"grants":["00000000-0000-4000-8000-000000000002"],"context":[]},"recurrence":{"type":"floating","cron":"*/1 * * * *"},"lifecycle":"active","routine":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000004","device":"00000000-0000-4000-8000-000000000005"},"bot":"00000000-0000-4000-8000-000000000006","home":"00000000-0000-4000-8000-000000000002","creation_request":"00000000-0000-4000-8000-000000000007"}}"#;

#[test]
fn semantic_hash_is_shared_replay_identity_before_current_policy_or_ids() {
    let mut definition:ScheduleDefinition=serde_json::from_str(DEFINITION).unwrap();
    let association=definition.routine.clone().unwrap();
    let slot=RoutineSlot{owner:association.owner,bot:association.bot,schedule:"00000000-0000-4000-8000-000000000008".parse().unwrap(),occurrence_ms:60_000};
    let old=routine_semantic_digest(&slot,&definition,association.home,None).unwrap();
    assert_eq!(old,routine_semantic_digest(&slot,&definition,association.home,None).unwrap());
    let mut changed=slot;
    changed.occurrence_ms+=60_000;
    assert_ne!(old,routine_semantic_digest(&changed,&definition,association.home,None).unwrap());
    definition.template.requested_model=Some(ModelName::new("inert changed model").unwrap());
    assert_ne!(old,routine_semantic_digest(&slot,&definition,association.home,None).unwrap());
    definition.routine.as_mut().unwrap().bot=BotId::mint();
    assert!(routine_semantic_digest(&slot,&definition,association.home,None).is_err());
}
