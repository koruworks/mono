// SPDX-License-Identifier: MPL-2.0

//! Coherent projection of the existing owner account reads. A matching row in
//! the owner-only Workspace usage response proves ownership; an arbitrary
//! caller's account policy is never substituted for a Workspace owner's policy.

use crate::{
    HttpControlPlane,
    ports::{CloudAccessFacts, TransferError},
};
use koru_account_core::{
    cloud_admission::{Binding, Observation},
    cloud_plan::{CloudAccessState, CloudAllowance},
};
use koru_account_wire::cloud_plan::{
    AllowanceV1, CloudPlanResponseV1, CloudUsageWorkspacesResponseV1, UsageState,
};

impl HttpControlPlane {
    /// One owner read, shared by every held Workspace bound to that account.
    /// The host owns cancellation, current authority, coalescing and scheduling.
    pub async fn admission_observations(
        &self,
        access: &CloudAccessFacts,
        bindings: &[Binding],
    ) -> Result<Vec<Observation>, TransferError> {
        let Some(first) = bindings.first() else {
            return Ok(Vec::new());
        };
        if bindings.iter().any(|b| b.owner != first.owner) {
            return Err(TransferError::NoAccess);
        }
        let plan: CloudPlanResponseV1 = self.account_read(access, "/v1/account/cloud-plan").await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        let observed = plan.evaluated_at_millis.get();
        if observed > now
            || now.saturating_sub(observed) >= 60_000
            || plan
                .valid_until_millis
                .is_some_and(|until| until.get() <= now)
        {
            return Err(TransferError::Unreachable);
        }
        let mut path = "/v1/account/cloud-usage/workspaces?limit=100".to_owned();
        let mut result = Vec::new();
        let mut cursors = std::collections::HashSet::new();
        let mut digest = None;
        for _ in 0..64 {
            let page: CloudUsageWorkspacesResponseV1 = self.account_read(access, &path).await?;
            if page.revision != plan.revision
                || page.generation != plan.usage.generation
                || page.items.len() > 100
                || digest
                    .as_ref()
                    .is_some_and(|d| d != &page.catalog_revision_digest)
            {
                return Err(TransferError::Unreachable);
            }
            digest = Some(page.catalog_revision_digest.clone());
            for binding in bindings {
                if let Some(observation) = project(&plan, &page, *binding) {
                    if result
                        .iter()
                        .any(|o: &Observation| o.binding.workspace == binding.workspace)
                    {
                        return Err(TransferError::Unreachable);
                    }
                    result.push(observation);
                }
            }
            match page.next_cursor {
                None => return Ok(result),
                Some(cursor) => {
                    if cursor.is_empty()
                        || cursor.len() > 2048
                        || !cursor
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                        || !cursors.insert(cursor.clone())
                    {
                        return Err(TransferError::Unreachable);
                    }
                    path = format!("/v1/account/cloud-usage/workspaces?limit=100&cursor={cursor}");
                }
            }
        }
        Err(TransferError::Unreachable)
    }
}

fn project(
    plan: &CloudPlanResponseV1,
    page: &CloudUsageWorkspacesResponseV1,
    binding: Binding,
) -> Option<Observation> {
    if page.revision != plan.revision || page.generation != plan.usage.generation {
        return None;
    }
    let row = page.items.iter().find(|row| {
        row.workspace_id.0 == binding.workspace && row.incarnation_id.0 == binding.incarnation
    })?;
    let allowance = match plan.policy.allowance {
        AllowanceV1::Finite {
            base_bytes,
            effective_bytes,
        } => CloudAllowance::Finite {
            base_bytes: base_bytes.get() as u64,
            effective_bytes: effective_bytes.get() as u64,
        },
        AllowanceV1::Unbounded => CloudAllowance::Unbounded,
        AllowanceV1::Unavailable { .. } => CloudAllowance::Unavailable,
    };
    let read = matches!(
        plan.policy.access.state,
        CloudAccessState::ReadWrite | CloudAccessState::ReadOnly
    );
    let observed = plan.evaluated_at_millis.get();
    Some(Observation {
        binding,
        usage_revision: plan.revision.get() as u64,
        policy_revision: plan.policy_revision.get() as u64,
        generation: plan.usage.generation.map(|g| g.0),
        observed_at_millis: observed,
        valid_until_millis: plan
            .valid_until_millis
            .map(|v| v.get())
            .unwrap_or(i64::MAX)
            .min(observed.saturating_add(60_000)),
        allowance,
        read,
        ordinary_write: plan.policy.access.state == CloudAccessState::ReadWrite,
        remove: read,
        complete_usage: plan.usage.state == UsageState::Known
            && row.usage.state == UsageState::Known,
        used_bytes: plan.usage.used_bytes.map(|v| v.get() as u64),
        reserved_bytes: plan.usage.reserved_bytes.map(|v| v.get() as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn fixtures() -> (Value, Value) {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../koru-account-wire/src/cloud_plan.fixtures.json"
        ))
        .unwrap();
        let get = |name: &str| {
            fixtures["fixtures"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .unwrap()["response"]
                .clone()
        };
        (get("finite_plus"), get("workspace_page"))
    }

    fn binding(page: &CloudUsageWorkspacesResponseV1, index: usize) -> Binding {
        Binding {
            owner: uuid::Uuid::from_u128(1),
            workspace: page.items[index].workspace_id.0,
            incarnation: page.items[index].incarnation_id.0,
            account_generation: 1,
            authority_generation: 2,
        }
    }

    #[test]
    fn absent_incarnation_or_incoherent_usage_cannot_supply_owner_facts() {
        let (plan, page) = fixtures();
        let plan: CloudPlanResponseV1 = serde_json::from_value(plan).unwrap();
        let mut page: CloudUsageWorkspacesResponseV1 = serde_json::from_value(page).unwrap();
        let binding = binding(&page, 0);
        assert!(project(&plan, &page, binding).unwrap().complete_usage);
        assert!(
            project(
                &plan,
                &page,
                Binding {
                    incarnation: uuid::Uuid::from_u128(9),
                    ..binding
                }
            )
            .is_none()
        );
        page.revision = koru_account_wire::cloud_plan::Decimal::new(43).unwrap();
        assert!(project(&plan, &page, binding).is_none());
        page.revision = plan.revision;
        page.generation = None;
        assert!(project(&plan, &page, binding).is_none());
    }

    #[test]
    fn unknown_workspace_never_inherits_known_owner_usage_proof() {
        let (plan, page) = fixtures();
        let plan: CloudPlanResponseV1 = serde_json::from_value(plan).unwrap();
        let page: CloudUsageWorkspacesResponseV1 = serde_json::from_value(page).unwrap();
        let observation = project(&plan, &page, binding(&page, 2)).unwrap();
        assert!(!observation.complete_usage);
        assert_eq!(
            observation.valid_until_millis,
            observation.observed_at_millis + 60_000
        );
    }

    #[tokio::test]
    async fn two_workspaces_share_one_owner_plan_and_usage_read() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let (mut plan, page) = fixtures();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        plan["evaluated_at_millis"] = Value::String(now.to_string());
        plan["valid_until_millis"] = Value::String((now + 60_000).to_string());
        let typed: CloudUsageWorkspacesResponseV1 = serde_json::from_value(page.clone()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let plan_calls = calls.clone();
        let usage_calls = calls.clone();
        let router = axum::Router::new()
            .route(
                "/v1/account/cloud-plan",
                axum::routing::get(move || {
                    plan_calls.fetch_add(1, Ordering::SeqCst);
                    let plan = plan.clone();
                    async move { axum::Json(plan) }
                }),
            )
            .route(
                "/v1/account/cloud-usage/workspaces",
                axum::routing::get(move || {
                    usage_calls.fetch_add(1, Ordering::SeqCst);
                    let page = page.clone();
                    async move { axum::Json(page) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let access = CloudAccessFacts {
            api_origin: format!("http://{address}").parse().unwrap(),
            bearer: "fixture-not-a-credential".into(),
            region: koru_account_core::workspace_authorization::Region::Us,
            byte_origins: crate::ByteOrigins::for_region(
                koru_account_core::workspace_authorization::Region::Us,
            ),
        };
        let observations = HttpControlPlane::new()
            .unwrap()
            .admission_observations(&access, &[binding(&typed, 0), binding(&typed, 1)])
            .await
            .unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_ne!(
            observations[0].binding.workspace,
            observations[1].binding.workspace
        );
        assert_eq!(observations[0].binding.owner, observations[1].binding.owner);
        server.abort();
    }
}
