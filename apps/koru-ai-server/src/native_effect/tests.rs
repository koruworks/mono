// SPDX-License-Identifier: MPL-2.0

//! Cohort `m6_save_keeps_version_id`, the confirmation half: the Store's
//! version id travels with the evidence to the execution owner, the owner's
//! exact echo is still required, and the saved receipt carries the id. The
//! owner here is a scripted port; the Store is real and in memory.
use super::*;
use crate::store_tools::native_store_backend;
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, DocumentVersionId, NativeResultStage, RunAttemptId, RunId,
};
use koru_store_core::model::Timestamp;
use koru_store_server::memory::MemoryStore;
use std::sync::Mutex;

/// How the scripted owner answers a confirmation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// As the ledger does since runs.db 12: store the evidence, return it.
    Echoes,
    /// As a ledger before the version column would: drop the id.
    DropsTheId,
    /// A different version for the same operation.
    ChangesTheId,
}

struct Port {
    owner: Owner,
    intent: NativeResultIntentView,
    sent: Mutex<Vec<NativeResultReference>>,
}
impl BotDelegatePort for Port {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let BotCommand::ConfirmNativeResult { request } = command else {
            return Err(koru_delegate_api::Failure::unavailable());
        };
        let NativeResultObservation::Durable { evidence } = request.observation else {
            return Err(koru_delegate_api::Failure::unavailable());
        };
        self.sent.lock().unwrap().push(evidence.clone());
        let mut verified = evidence;
        match self.owner {
            Owner::Echoes => {}
            Owner::DropsTheId => verified.version_id = None,
            Owner::ChangesTheId => verified.version_id = Some(DocumentVersionId::new()),
        }
        let mut intent = self.intent.clone();
        intent.stage = NativeResultStage::Registered;
        intent.verified = Some(verified);
        Ok(BotReply::NativeResult { intent })
    }
}

struct Host;
impl NativeEffectHost for Host {
    type Failure = &'static str;
    fn failure(&self, code: &'static str) -> &'static str {
        code
    }
    fn settled(&self) {}
    fn uncertain(&self, _: BotOwner, _: BotId, _: NativeResultKey) -> Result<(), &'static str> {
        Ok(())
    }
}

struct Fixture {
    memory: MemoryStore,
    authority: NativeStoreAuthority,
    evidence: NativeWriteEvidence,
    intent: NativeResultIntentView,
}

/// A page really created through the native effect path, and the prepared
/// intent of the operation that created it.
fn fixture() -> Fixture {
    let memory = MemoryStore::open("native-effect-version-id").unwrap();
    let authority = memory
        .with_store(|store| {
            let at = Timestamp::now();
            let profile = store
                .create_profile("First", None, None, koru_store_core::Locale::default(), at)
                .unwrap()
                .id;
            NativeStoreAuthority {
                owner_scope: store.local_user_scope().unwrap(),
                expected_profile: profile,
                workspace: store.create_workspace(profile, "Work", at).unwrap().id,
            }
        })
        .unwrap();
    let backend = native_store_backend(memory.client(), authority.clone());
    let prepared = backend
        .prepare_native_write(
            "create_document",
            json!({"workspace_id": authority.workspace, "title": "Result"}),
        )
        .unwrap();
    let identity = NativeStoreIdentity {
        candidate: koru_store_core::ids::DocumentId::new(),
        scope: "native-effect-version-id".into(),
        request_key: uuid::Uuid::new_v4().to_string(),
    };
    let evidence = backend
        .execute_native_write(&prepared, identity)
        .unwrap_or_else(|error| panic!("create refused: {}", error.error().code));
    let intent = NativeResultIntentView {
        key: NativeResultKey {
            run: RunId::mint(),
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            turn: AgentTurnId::mint(),
            operation_id: uuid::Uuid::new_v4(),
        },
        purpose: NativeEffectPurpose::Result {},
        kind: NativeResultOperation::CreatePage,
        destination: evidence.workspace,
        request_fingerprint: [3; 32],
        target: evidence.document,
        store_request: uuid::Uuid::new_v4(),
        stage: NativeResultStage::Prepared,
        verified: None,
        disposition: NativeResultDisposition::Execute,
    };
    Fixture {
        memory,
        authority,
        evidence,
        intent,
    }
}

fn confirm_with(f: &Fixture, owner: Owner) -> (Result<NativeResultIntentView, &'static str>, Port) {
    let port = Port {
        owner,
        intent: f.intent.clone(),
        sent: Mutex::new(Vec::new()),
    };
    let result = confirm_record(&port, &f.intent, f.evidence.clone(), &|code| code);
    (result, port)
}

#[test]
fn m6_save_keeps_version_id_confirm_sends_the_id_and_accepts_the_owners_echo() {
    let f = fixture();
    let version = f.evidence.version_id.expect("the Store returned its id");
    let (result, port) = confirm_with(&f, Owner::Echoes);
    let observed = result.expect("the owner's exact echo is accepted");
    // What was sent names the Store's version, and what came back is it.
    let sent = port.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].version_id, Some(version));
    assert_eq!(sent[0].document, f.evidence.document);
    assert_eq!(sent[0].version, f.evidence.version);
    assert_eq!(sent[0].kind, NativeResultKind::CreatedPage);
    assert_eq!(observed.stage, NativeResultStage::Registered);
    assert_eq!(observed.verified.as_ref(), Some(&sent[0]));
    assert_eq!(observed.verified.unwrap().version_id, Some(version));
}

#[test]
fn m6_save_keeps_version_id_an_owner_that_does_not_return_the_id_is_not_a_save() {
    // The ledger stores and returns the id (runs.db 12) before this sender
    // carries one: an owner that drops or changes it leaves the save unknown,
    // never silently registered without its pin.
    let f = fixture();
    for owner in [Owner::DropsTheId, Owner::ChangesTheId] {
        let (result, port) = confirm_with(&f, owner);
        assert_eq!(result, Err("save_status_unknown"));
        assert_eq!(port.sent.lock().unwrap().len(), 1);
    }
}

#[test]
fn m6_save_keeps_version_id_the_saved_receipt_carries_it() {
    let f = fixture();
    let version = f.evidence.version_id.unwrap();
    let (result, _) = confirm_with(&f, Owner::Echoes);
    let registered = result.unwrap();
    let receipt = saved(&Host, &f.memory.client(), &f.authority, &registered).unwrap();
    assert_eq!(receipt["status"], "saved");
    assert_eq!(receipt["version_id"], json!(version));
    assert_eq!(receipt["document_id"], json!(f.evidence.document));
    assert_eq!(receipt["kind"], "created_page");
    // The id is the one the Store holds for exactly this state.
    let workspace = f.authority.workspace;
    let stored = f
        .memory
        .with_store(move |store| store.bounded_document_version(workspace, version).unwrap())
        .unwrap()
        .expect("the version row exists");
    assert_eq!(stored.document, f.evidence.document);
    assert_eq!(stored.version_vector, f.evidence.version);
    // A result registered before the id was kept has no key, not a null.
    let mut earlier = registered.clone();
    earlier.verified.as_mut().unwrap().version_id = None;
    let receipt = saved(&Host, &f.memory.client(), &f.authority, &earlier).unwrap();
    assert_eq!(receipt["status"], "saved");
    assert!(receipt.get("version_id").is_none());
    // And such a reference, confirmed again after a restart, is sent exactly
    // as it was first sent: without an id, which the owner echoes.
    let mut retained = f.evidence.clone();
    retained.version_id = None;
    let port = Port {
        owner: Owner::Echoes,
        intent: f.intent.clone(),
        sent: Mutex::new(Vec::new()),
    };
    let observed = confirm_record(&port, &f.intent, retained, &|code: &'static str| code).unwrap();
    assert_eq!(observed.verified.unwrap().version_id, None);
    assert_eq!(port.sent.lock().unwrap()[0].version_id, None);
}
