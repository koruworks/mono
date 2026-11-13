// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{
    BotAccess, BotId, BotOwner, BotProvenance, BotRequestId, CapturedBotPolicy, DeviceId, RunId,
    WorkspaceId,
};
use koru_store_core::UserScopeId;

struct Port {
    reply: BotReply,
    run: RunId,
    generation: koru_delegate_core::AttemptGeneration,
}
impl BotDelegatePort for Port {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(
            matches!(command,BotCommand::ResolveRunAuthority {run,generation} if run==self.run && generation==self.generation)
        );
        Ok(self.reply.clone())
    }
}
fn record() -> RunAuthorityRecord {
    let workspace = WorkspaceId::new();
    RunAuthorityRecord {
        run: RunId::mint(),
        workspace,
        generation: koru_delegate_core::AttemptGeneration::FIRST,
        grants: [workspace].into(),
        run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
            if cfg!(windows) {
                "D:/isolated"
            } else {
                "/isolated"
            },
        ))
        .unwrap(),
        state: koru_artifact_core::AuthorityState::Active,
        created_at_ms: 0,
        revoked_at_ms: None,
    }
}
fn port(record: &RunAuthorityRecord, reply: BotReply) -> Option<Arc<dyn BotDelegatePort>> {
    Some(Arc::new(Port {
        reply,
        run: record.run,
        generation: record.generation,
    }))
}
fn bot(record: &RunAuthorityRecord) -> RunToolAuthority {
    RunToolAuthority::Bot {
        provenance: BotProvenance {
            routine: None,
            owner: BotOwner {
                user_scope: UserScopeId::new(),
                device: DeviceId::new(),
            },
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: uuid::Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace: record.workspace,
            destination: record.workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
        },
    }
}

#[tokio::test]
async fn only_positive_owner_classification_selects_a_catalog() {
    let record = record();
    assert!(classify(None, &record).await.is_err());
    assert!(
        classify(port(&record, BotReply::Missing), &record)
            .await
            .is_err()
    );
    assert!(matches!(
        classify(
            port(
                &record,
                BotReply::RunAuthority {
                    authority: RunToolAuthority::Ordinary {}
                }
            ),
            &record
        )
        .await
        .unwrap(),
        RunToolAuthority::Ordinary {}
    ));
    let authority = bot(&record);
    assert!(matches!(
        classify(
            port(
                &record,
                BotReply::RunAuthority {
                    authority: authority.clone()
                }
            ),
            &record
        )
        .await
        .unwrap(),
        RunToolAuthority::Bot { .. }
    ));
    let mut changed = record.clone();
    changed.workspace = WorkspaceId::new();
    assert!(
        classify(
            port(&changed, BotReply::RunAuthority { authority }),
            &changed
        )
        .await
        .is_err()
    );
}

#[test]
fn four_shared_native_tools_publish_only_owner_supported_identity_and_creation_fields() {
    let tools = definitions();
    let names: std::collections::BTreeSet<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "search_documents",
            "read_document",
            "create_document",
            "edit_document"
        ]
        .into()
    );
    for tool in &tools {
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        if matches!(tool.name.as_ref(), "create_document" | "edit_document") {
            assert!(schema["properties"]["operation_id"].is_object());
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("operation_id"))
            );
        }
        if tool.name == "create_document" {
            assert!(schema["properties"].get("idempotency_key").is_none());
            assert!(schema["properties"].get("parent_document_id").is_none());
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    schema["properties"]
                        .get(required.as_str().unwrap())
                        .is_some()
                );
            }
        }
        if tool.name == "edit_document" {
            let encoded = serde_json::to_value(tool).unwrap();
            assert_eq!(encoded["annotations"]["idempotentHint"], false);
        }
    }
}
