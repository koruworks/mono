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
mod recovery;
mod writes;

pub struct Engine {
    pub endpoint: koru_ipc::Endpoint,
    /// Private Agent bootstrap authority, never included in worker requests.
    pub authority: Secret,
}
trait Resolver {
    fn catalog(&self, secret: &Secret) -> Result<Value, Error>;
    fn resolve(&self, secret: &Secret, binding: Uuid, call: &Call) -> Result<Resolution, Error>;
}
impl Resolver for Engine {
    fn catalog(&self, secret: &Secret) -> Result<Value, Error> {
        match worker::request(
            &self.endpoint,
            self.authority.expose(),
            worker::Command::SessionCatalog {
                secret: secret.clone(),
            },
        )? {
            worker::Reply::Catalog(grant) => Ok(grant.0),
            worker::Reply::Refused(error) => Err(error),
            _ => Err(Error::EngineUnavailable),
        }
    }
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
    incarnation: Uuid,
}
impl GatewayState {
    pub fn bots_workspace(&self, engine: Engine) -> Boundary {
        Boundary {
            engine,
            store: self.store.client(),
            incarnation: Uuid::new_v4(),
        }
    }
}
impl Boundary {
    pub fn handle(&self, command: workspace::Command) -> workspace::Reply {
        match command {
            workspace::Command::Recover { authority, request } => {
                if !same_authority(&authority, &self.engine.authority) {
                    return workspace::Reply::Refused(Error::Unauthenticated);
                }
                self.recover(request)
                    .map(workspace::Reply::Recovery)
                    .unwrap_or_else(workspace::Reply::Refused)
            }
            workspace::Command::Call(request) => self.execute(request),
            workspace::Command::Catalog { secret } => catalog(&self.engine, &secret)
                .map(workspace::Reply::Result)
                .unwrap_or_else(workspace::Reply::Refused),
            workspace::Command::Context { authority, sources } => {
                if !same_authority(&authority, &self.engine.authority) {
                    return workspace::Reply::Refused(Error::Unauthenticated);
                }
                load_context(sources, |identity, page| {
                    let arguments = serde_json::json!({"workspace_id":identity.workspace.to_storage(),"document_id":page});
                    let call = Call::parse(Uuid::new_v4(), "read_document".into(), &serde_json::to_vec(&arguments).map_err(|_| Error::InvalidRequest)?)?;
                    self.read(identity, &call)
                }).map(workspace::Reply::Context).unwrap_or_else(workspace::Reply::Refused)
            }
        }
    }
    pub fn execute(&self, request: workspace::Request) -> workspace::Reply {
        if matches!(request.tool.as_str(), "create_document" | "edit_document") {
            return self
                .write(request)
                .map(workspace::Reply::Result)
                .unwrap_or_else(workspace::Reply::Refused);
        }
        execute(&self.engine, request, |identity, call| {
            self.read(identity, call)
        })
        .map(workspace::Reply::Result)
        .unwrap_or_else(workspace::Reply::Refused)
    }
    fn read(&self, identity: ReadAuthority, call: &Call) -> Result<Value, Error> {
        let workspace = identity.workspace;
        let backend = self.backend(identity)?;
        let context = koru_mcp_server::RequestContext::selected(
            "local",
            "koru-bots-engine",
            [Scope::ReadDocuments],
            [workspace],
        );
        koru_mcp_server::WorkspaceToolExecutor::new(backend)
            .invoke(&context, call.tool(), call.arguments().clone())
            .map_err(native_error)
    }
    fn store_authority(
        &self,
        identity: ReadAuthority,
    ) -> Result<koru_ai_server::NativeStoreAuthority, Error> {
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
        Ok(koru_ai_server::NativeStoreAuthority {
            owner_scope: owner,
            expected_profile: profile,
            workspace,
        })
    }
    fn backend(
        &self,
        identity: ReadAuthority,
    ) -> Result<std::sync::Arc<koru_mcp_server::LocalBackend>, Error> {
        Ok(koru_ai_server::native_store_backend(
            self.store.clone(),
            self.store_authority(identity)?,
        ))
    }
}
fn native_error(error: koru_mcp_server::ToolError) -> Error {
    match error.code.as_str() {
        "permission_denied" => Error::NotAllowed,
        "invalid_request" => Error::InvalidRequest,
        _ => Error::StorageUnavailable,
    }
}
#[derive(Clone, Copy)]
struct ReadAuthority {
    owner: UserScopeId,
    workspace: WorkspaceId,
}

fn same_authority(supplied: &Secret, expected: &Secret) -> bool {
    supplied.expose().len() == expected.expose().len()
        && supplied
            .expose()
            .bytes()
            .zip(expected.expose().bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn load_context(
    sources: workspace::Sources,
    mut read: impl FnMut(ReadAuthority, Uuid) -> Result<Value, Error>,
) -> Result<workspace::Context, Error> {
    sources.validate()?;
    let grant: CapturedBotPolicy =
        serde_json::from_value(sources.grant).map_err(|_| Error::NotAllowed)?;
    if !grant.valid() {
        return Err(Error::NotAllowed);
    }
    let owner = Uuid::parse_str(&sources.scope)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(Error::NotAllowed)?;
    let identity = ReadAuthority {
        owner: UserScopeId::from_uuid(owner),
        workspace: grant.workspace,
    };
    let mut bytes = 0usize;
    let mut page = |id| {
        let text = read(identity, id)?.to_string();
        bytes = bytes.saturating_add(text.len());
        if bytes > 1024 * 1024 {
            return Err(Error::InputTooSmall);
        }
        Ok(workspace::Page { id, text })
    };
    let context = workspace::Context {
        role: sources.role.map(&mut page).transpose()?,
        knowledge: sources
            .knowledge
            .into_iter()
            .map(&mut page)
            .collect::<Result<_, _>>()?,
        memory: sources.memory.map(page).transpose()?,
    };
    if serde_json::to_vec(&context)
        .map_err(|_| Error::InvalidRequest)?
        .len()
        > 1024 * 1024
    {
        return Err(Error::InputTooSmall);
    }
    Ok(context)
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
    let identity = read_authority(resolved)?;
    read(identity, &call)
}
fn read_authority(resolved: Resolution) -> Result<ReadAuthority, Error> {
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
    Ok(ReadAuthority {
        owner: UserScopeId::from_uuid(owner),
        workspace: current.workspace,
    })
}

/// Discovery has no resource content. The worker filters this static catalog
/// with its captured grant, and every execution is freshly resolved above.
fn read_catalog() -> Vec<Value> {
    koru_mcp_server::native::native_tool_catalog().into_iter()
        .map(|mut tool| {
            if matches!(tool.name.as_str(),"create_document"|"edit_document") {
                tool.input_schema["properties"]["operation_id"]=serde_json::json!({"type":"string","format":"uuid","description":"New UUID v4 for a new logical write. Reuse this exact ID only to query or retry the same intent; never change its arguments."});
                tool.input_schema["required"].as_array_mut().expect("native tool required fields").push(Value::String("operation_id".into()));
            }
            serde_json::json!({"name":tool.name,"description":tool.description,"inputSchema":tool.input_schema})
        })
        .collect()
}

fn catalog(resolver: &dyn Resolver, secret: &Secret) -> Result<Value, Error> {
    let grant: CapturedBotPolicy =
        serde_json::from_value(resolver.catalog(secret)?).map_err(|_| Error::NotAllowed)?;
    if !grant.valid() {
        return Err(Error::NotAllowed);
    }
    Ok(Value::Array(read_catalog()))
}

#[cfg(test)]
mod tests;
