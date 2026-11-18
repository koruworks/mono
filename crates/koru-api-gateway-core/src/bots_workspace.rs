// SPDX-License-Identifier: MPL-2.0

//! The Bots engine's workspace boundary. Shared native semantics and the
//! existing Store adapter remain the only document readers and writers.
use crate::GatewayState;
use koru_bots_engine::{
    Error,
    authority::{Call, Resolution, Secret},
    worker, workspace,
};
use koru_delegate_core::CapturedBotPolicy;
use koru_store_core::{UserScopeId, WorkspaceId, model::Scope};
use koru_store_server::StoreWorkerClient;
use serde_json::Value;
use uuid::Uuid;

pub struct Engine {
    pub endpoint: koru_ipc::Endpoint,
    /// Private Agent bootstrap authority, never included in worker requests.
    pub authority: Secret,
}
trait Resolver {
    fn resolve(&self, secret: &Secret, binding: Uuid, call: &Call) -> Result<Resolution, Error>;
}
impl Resolver for Engine {
    fn resolve(&self, secret: &Secret, binding: Uuid, call: &Call) -> Result<Resolution, Error> {
        match worker::request(
            &self.endpoint,
            self.authority.expose(),
            worker::Command::ResolveCall {
                secret: secret.clone(),
                binding,
                descriptor: call.descriptor().clone(),
            },
        )? {
            worker::Reply::Resolved(resolution) => Ok(resolution),
            worker::Reply::Refused(error) => Err(error),
            _ => Err(Error::EngineUnavailable),
        }
    }
}
pub struct Boundary {
    engine: Engine,
    store: StoreWorkerClient,
}
impl GatewayState {
    pub fn bots_workspace(&self, engine: Engine) -> Boundary {
        Boundary {
            engine,
            store: self.store.client(),
        }
    }
}
impl Boundary {
    pub fn execute(&self, request: workspace::Request) -> workspace::Reply {
        execute(&self.engine, request, |identity, call| {
            self.read(identity, call)
        })
        .map(workspace::Reply::Result)
        .unwrap_or_else(workspace::Reply::Refused)
    }
    fn read(&self, identity: ReadAuthority, call: &Call) -> Result<Value, Error> {
        let owner = identity.owner;
        let workspace = identity.workspace;
        // Pin the current placement, then the native adapter checks it again
        // inside the Store operation. Neither a stale profile nor another local
        // user's scope is inferred from arguments.
        let profile = self
            .store
            .with_store(move |store| {
                if store
                    .local_user_scope()
                    .map_err(|_| Error::StorageUnavailable)?
                    != owner
                {
                    return Err(Error::NotAllowed);
                }
                store
                    .current_placement_profile(workspace)
                    .map_err(|_| Error::StorageUnavailable)?
                    .ok_or(Error::NotAllowed)
            })
            .map_err(|_| Error::StorageUnavailable)??;
        let backend = koru_ai_server::native_store_backend(
            self.store.clone(),
            koru_ai_server::NativeStoreAuthority {
                owner_scope: owner,
                expected_profile: profile,
                workspace,
            },
        );
        let context = koru_mcp_server::RequestContext::selected(
            "local",
            "koru-bots-engine",
            [Scope::ReadDocuments],
            [workspace],
        );
        koru_mcp_server::WorkspaceToolExecutor::new(backend)
            .invoke(&context, call.tool(), call.arguments().clone())
            .map_err(|error| match error.code.as_str() {
                "permission_denied" => Error::NotAllowed,
                "invalid_request" => Error::InvalidRequest,
                _ => Error::StorageUnavailable,
            })
    }
}
struct ReadAuthority {
    owner: UserScopeId,
    workspace: WorkspaceId,
}
fn execute(
    resolver: &dyn Resolver,
    request: workspace::Request,
    read: impl FnOnce(ReadAuthority, &Call) -> Result<Value, Error>,
) -> Result<Value, Error> {
    let call = request.call()?;
    let resolved = resolver.resolve(&request.secret, request.binding, &call)?;
    if !matches!(call.tool(), "read_document" | "search_documents") {
        // Native mutations are installed with the lease/fence composition.
        return Err(Error::NotAllowed);
    }
    let captured: CapturedBotPolicy =
        serde_json::from_value(resolved.captured_grant).map_err(|_| Error::NotAllowed)?;
    let current: CapturedBotPolicy =
        serde_json::from_value(resolved.current_grant).map_err(|_| Error::NotAllowed)?;
    if !captured.valid() || !current.valid() || captured.workspace != current.workspace {
        return Err(Error::NotAllowed);
    }
    let owner = Uuid::parse_str(&resolved.scope)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(Error::NotAllowed)?;
    // Default access limits edits. Reads/search use the home workspace, whose
    // scope the shared executor intersects with the requested resource.
    read(
        ReadAuthority {
            owner: UserScopeId::from_uuid(owner),
            workspace: current.workspace,
        },
        &call,
    )
}

/// Discovery has no resource content. The worker filters this static catalog
/// with its captured grant, and every execution is freshly resolved above.
pub fn read_catalog() -> Vec<Value> {
    koru_mcp_server::native::native_tool_catalog().into_iter()
        .filter(|tool| matches!(tool.name.as_str(), "read_document" | "search_documents"))
        .map(|tool| serde_json::json!({"name":tool.name,"description":tool.description,"inputSchema":tool.input_schema}))
        .collect()
}

#[cfg(test)]
mod tests;
