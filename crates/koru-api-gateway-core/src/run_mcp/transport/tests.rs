// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn ordinary_run_catalog_does_not_advertise_bot_operations() {
    let instructions = initialize(&ProtocolVersion::LATEST).instructions.unwrap();
    assert!(instructions.contains("artifact endpoint"));
    assert!(!instructions.contains("memory"));
    assert!(!instructions.contains("routine"));
    assert!(
        tools::definitions()
            .iter()
            .all(|tool| !tool.name.contains("bot"))
    );
}

#[tokio::test]
async fn missing_execution_authority_is_never_ordinary_permission() {
    // No valid artifact bearer may bypass the separate exact-generation proof.
    let authority = record();
    assert_eq!(
        classify(None, &authority).await.unwrap_err().code,
        "run_authority_unavailable"
    );
}

fn record() -> RunAuthorityRecord {
    use koru_artifact_core::{AttemptGeneration, AuthorityState, RunWorkspace, WorkspaceId};
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::new_v4());
    RunAuthorityRecord {
        run: RunId::mint(),
        workspace,
        generation: AttemptGeneration::FIRST,
        grants: std::collections::BTreeSet::from([workspace]),
        run_workspace: RunWorkspace::new(std::path::Path::new(if cfg!(windows) {
            "C:/ordinary-run"
        } else {
            "/ordinary-run"
        }))
        .unwrap(),
        state: AuthorityState::Active,
        created_at_ms: 1,
        revoked_at_ms: None,
    }
}

#[tokio::test]
async fn execution_owner_must_confirm_the_exact_run_generation() {
    struct Owner(RunId, koru_artifact_core::AttemptGeneration);
    impl koru_delegate_api::authority::RunAuthorityPort for Owner {
        fn resolve(
            &self,
            run: RunId,
            generation: koru_artifact_core::AttemptGeneration,
        ) -> Result<(), koru_delegate_api::Failure> {
            if (run, generation) == (self.0, self.1) {
                Ok(())
            } else {
                Err(koru_delegate_api::Failure::new(
                    "run_authority_unavailable",
                    "Unavailable",
                    false,
                ))
            }
        }
    }
    let mut authority = record();
    let port: Arc<dyn koru_delegate_api::authority::RunAuthorityPort> =
        Arc::new(Owner(authority.run, authority.generation));
    assert!(classify(Some(port.clone()), &authority).await.is_ok());
    authority.generation = authority.generation.next();
    assert!(classify(Some(port), &authority).await.is_err());
}
