// SPDX-License-Identifier: MPL-2.0

//! Fixed local-owner facade. Narrow public permissions never flow into the
//! generic Bots or Chat routes; only these explicit calls use owner context.
use super::*;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use koru_ai_core::bots::{Bot, BotId};
use koru_ai_core::{AiSurface, ContextRef, WorkspaceId};
use koru_ai_wire::*;
use serde::{Deserialize, Serialize};

const PAGE_BYTES: usize = 128 * 1024;

/// Only the fixed calls the facade can make. This seam also keeps gateway
/// contract tests in memory, without starting a Store or an AI process.
trait TaskPort {
    fn bots(&self, context: RequestContext, request: BotCommand) -> Result<BotResponse, Failure>;
    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<koru_ai_api::AiResponseStream, Failure>;
    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse, Failure>;
}
impl TaskPort for koru_ai_client::Client {
    fn bots(&self, context: RequestContext, request: BotCommand) -> Result<BotResponse, Failure> {
        self.bots(context, request)
    }
    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<koru_ai_api::AiResponseStream, Failure> {
        self.send_turn(context, request)
    }
    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse, Failure> {
        self.cancel_attempt(context, attempt)
    }
}

fn invalid() -> Failure {
    Failure {
        code: "invalid_request".into(),
        message: "Invalid Bot task request or cursor.".into(),
        retryable: false,
        details: Default::default(),
    }
}
fn missing() -> Failure {
    Failure {
        code: "not_found".into(),
        message: "The Bot task was not found in this workspace.".into(),
        retryable: false,
        details: Default::default(),
    }
}

/// What an external client is told when the person has paused every Bot
/// (M8). The engine's refusal keeps its code, `team_paused`; the text and
/// `details.reason` say why in the caller's terms. Pausing and resuming the
/// team are the person's controls: this facade forwards neither, and its
/// scopes (`bots.read`, `bots.run`) do not reach the Bots route that does.
const TEAM_PAUSED_MESSAGE: &str = "The person has paused all Bots.";

fn team_paused(mut failure: Failure) -> Failure {
    if failure.code == koru_ai_wire::bot_team_codes::TEAM_PAUSED {
        failure.message = TEAM_PAUSED_MESSAGE.into();
        failure.retryable = false;
        failure.details.insert(
            "reason".into(),
            koru_ai_wire::bot_team_codes::TEAM_PAUSED.into(),
        );
    }
    failure
}

/// Authentication is the local OS user, not this caller-chosen audit label.
/// Never return this context or accept an arbitrary BotCommand alongside it.
fn owner_context(mut context: RequestContext) -> RequestContext {
    context.principal.scopes = vec![AiScope::ManageBots, AiScope::Chat];
    context
}

fn owned_bot(
    client: &dyn TaskPort,
    context: &RequestContext,
    workspace: WorkspaceId,
    bot_id: BotId,
) -> Result<Bot, Failure> {
    match client.bots(context.clone(), BotCommand::Read { bot_id })? {
        BotResponse::Bot { bot } if bot.home_workspace == workspace => Ok(bot),
        _ => Err(missing()),
    }
}

/// Same-owner clients share a durable namespace. A changed label neither
/// creates a new identity nor grants access. Include a versioned hash domain.
fn operation_id(bot: &Bot, request: uuid::Uuid) -> Result<uuid::Uuid, Failure> {
    if request.get_version_num() != 4 || request.get_variant() != uuid::Variant::RFC4122 {
        return Err(invalid());
    }
    let bytes =
        serde_json::to_vec(&(bot.user_scope, bot.owner_device, request)).map_err(|_| invalid())?;
    let hash = blake3::derive_key("koru.bot.external-task.owner-request.v1", &bytes);
    let mut id: [u8; 16] = hash[..16].try_into().expect("fixed hash width");
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    Ok(uuid::Uuid::from_bytes(id))
}

fn encode<T: Serialize>(cursor: &T) -> Result<String, Failure> {
    serde_json::to_vec(cursor)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| invalid())
}
fn decode<T: serde::de::DeserializeOwned>(cursor: &str) -> Result<T, Failure> {
    if cursor.len() > 1024 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

pub(super) async fn list_bots(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<ListBotsRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ReadBots) {
        Ok(r) => r,
        Err(r) => return r,
    };
    answer(call(move || list(&client, owner_context(context), request)).await)
}

fn list(
    client: &dyn TaskPort,
    context: RequestContext,
    request: ListBotsRequest,
) -> Result<ListBotsResponse, Failure> {
    if !(1..=100).contains(&request.limit) {
        return Err(invalid());
    }
    let after = request
        .cursor
        .as_deref()
        .map(decode::<(WorkspaceId, BotId)>)
        .transpose()?;
    if after.is_some_and(|(workspace, _)| workspace != request.workspace) {
        return Err(invalid());
    }
    let BotResponse::TaskBots { page } = client.bots(
        context,
        BotCommand::ListTaskBots {
            workspace: request.workspace,
            after: after.map(|(_, id)| id),
            limit: request.limit,
        },
    )?
    else {
        return Err(unavailable_failure());
    };
    let next_cursor = page
        .next
        .map(|id| encode(&(request.workspace, id)))
        .transpose()?;
    Ok(ListBotsResponse {
        bots: page.bots,
        next_cursor,
    })
}

pub(super) async fn start_bot_task(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<StartBotTaskRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // Check the extra document authority before creating any private context.
    if let Err(error) = selected_documents_allowed(&principal, &request) {
        return ai_failure(StatusCode::FORBIDDEN, error);
    }
    let (client, context) = match admitted(&state, principal, AiScope::RunBots) {
        Ok(r) => r,
        Err(r) => return r,
    };
    answer(call(move || start(&client, owner_context(context), request)).await)
}

fn selected_documents_allowed(
    principal: &GatewayPrincipal,
    request: &StartBotTaskRequest,
) -> Result<(), Failure> {
    if !request.document_ids.is_empty()
        && !principal.scopes.iter().any(|s| s == "store.read_documents")
    {
        return Err(Failure {
            code: "forbidden".into(),
            message: "Selected documents require store.read_documents.".into(),
            retryable: false,
            details: Default::default(),
        });
    }
    Ok(())
}

fn start(
    client: &dyn TaskPort,
    context: RequestContext,
    request: StartBotTaskRequest,
) -> Result<SendTurnAccepted, Failure> {
    let task = koru_ai_core::bots::BotTask {
        text: request.text.clone(),
        documents: request
            .document_ids
            .iter()
            .map(|id| koru_ai_core::bots::BotPage {
                workspace: request.workspace,
                document: *id,
            })
            .collect(),
    };
    task.validate(request.workspace).map_err(|_| invalid())?;
    let bot = owned_bot(client, &context, request.workspace, request.bot_id)?;
    let operation_id = operation_id(&bot, request.request_id)?;
    let stream = client
        .send_turn(
            context,
            SendTurnRequest {
                destination_workspace: Some(request.workspace),
                operation_id,
                chat_id: bot.main_chat,
                surface: AiSurface::Standalone,
                markdown: request.text,
                explicit_refs: request
                    .document_ids
                    .into_iter()
                    .map(|document_id| ContextRef::Document { document_id })
                    .collect(),
                reasoning_override: None,
                web_search: false,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .map_err(team_paused)?;
    // accepted is durable. Dropping the subscriber only detaches transport;
    // the AI writer and Run owner continue the shared conversation.
    Ok(stream.accepted)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCursor {
    attempt: InferenceAttemptId,
    operation: uuid::Uuid,
    answer: u64,
    results: usize,
}

fn bound_answer(
    client: &dyn TaskPort,
    context: &RequestContext,
    bot: &Bot,
    request_id: uuid::Uuid,
    attempt_id: InferenceAttemptId,
    offset: u64,
) -> Result<koru_ai_core::bots::BotTaskAnswer, Failure> {
    let operation_id = operation_id(bot, request_id)?;
    match client.bots(
        context.clone(),
        BotCommand::ReadTaskAnswer {
            bot_id: bot.id,
            chat_id: bot.main_chat,
            attempt_id,
            operation_id,
            offset,
        },
    )? {
        BotResponse::TaskAnswer { answer } => Ok(answer),
        _ => Err(unavailable_failure()),
    }
}

pub(super) async fn read_bot_task(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<ReadBotTaskRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ReadBots) {
        Ok(r) => r,
        Err(r) => return r,
    };
    answer(call(move || read(&client, owner_context(context), request)).await)
}

fn read(
    client: &dyn TaskPort,
    context: RequestContext,
    request: ReadBotTaskRequest,
) -> Result<ReadBotTaskResponse, Failure> {
    let bot = owned_bot(client, &context, request.workspace, request.bot_id)?;
    let operation = operation_id(&bot, request.request_id)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(decode::<TaskCursor>)
        .transpose()?
        .unwrap_or(TaskCursor {
            attempt: request.attempt_id,
            operation,
            answer: 0,
            results: 0,
        });
    if cursor.attempt != request.attempt_id || cursor.operation != operation {
        return Err(invalid());
    }
    let answer = bound_answer(
        client,
        &context,
        &bot,
        request.request_id,
        request.attempt_id,
        cursor.answer,
    )?;
    let mut state = if answer.status.is_terminal() {
        BotResultsState::Unavailable
    } else {
        BotResultsState::Preparing
    };
    let mut refs = Vec::new();
    if let Some(run_id) = answer.run_id {
        match client.bots(
            context,
            BotCommand::ReadRun {
                bot_id: bot.id,
                chat_id: bot.main_chat,
                message_id: Some(answer.message_id),
                run_id,
                from: 0,
                len: 1,
            },
        ) {
            Ok(BotResponse::Run { view }) => {
                refs = confirmed_results(request.workspace, view.header.native_results);
                state = BotResultsState::Available;
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "bot_unavailable" | "ai_unavailable" | "not_found" | "ai_worker_saturated"
                ) => {}
            Err(error) => return Err(error),
            _ => return Err(unavailable_failure()),
        }
    }
    page(request.attempt_id, operation, answer, state, refs, cursor)
}

fn confirmed_results(
    workspace: WorkspaceId,
    refs: Vec<koru_delegate_core::NativeResultReference>,
) -> Vec<BotTaskResult> {
    refs.into_iter()
        .filter(|r| r.workspace == workspace && r.valid())
        .map(|r| BotTaskResult {
            document_id: r.document,
            workspace: r.workspace,
            title: r.title,
            version: STANDARD.encode(r.version),
            confirmed_at_ms: r.confirmed_at_ms,
        })
        .collect()
}

fn page(
    attempt_id: InferenceAttemptId,
    operation: uuid::Uuid,
    answer: koru_ai_core::bots::BotTaskAnswer,
    state: BotResultsState,
    refs: Vec<BotTaskResult>,
    cursor: TaskCursor,
) -> Result<ReadBotTaskResponse, Failure> {
    if matches!(state, BotResultsState::Available) && cursor.results > refs.len() {
        return Err(invalid());
    }
    let answer_end = cursor.answer + answer.markdown.as_ref().map_or(0, |s| s.len() as u64);
    let mut response = ReadBotTaskResponse {
        attempt_id,
        status: answer.status,
        answer: answer.markdown,
        results_state: state,
        results: Vec::new(),
        next_cursor: None,
    };
    let mut result_end = cursor.results;
    for result in refs.iter().skip(cursor.results).take(20) {
        response.results.push(result.clone());
        // Reserve a complete cursor while measuring escaped JSON, not raw text.
        response.next_cursor = Some(encode(&TaskCursor {
            attempt: attempt_id,
            operation,
            answer: answer_end,
            results: result_end + 1,
        })?);
        if serde_json::to_vec(&response).map_err(|_| invalid())?.len() > PAGE_BYTES {
            response.results.pop();
            break;
        }
        result_end += 1;
    }
    response.next_cursor = if answer.next_offset.is_some() || result_end < refs.len() {
        Some(encode(&TaskCursor {
            attempt: attempt_id,
            operation,
            answer: answer_end,
            results: result_end,
        })?)
    } else {
        None
    };
    if serde_json::to_vec(&response).map_err(|_| invalid())?.len() > PAGE_BYTES {
        return Err(invalid());
    }
    Ok(response)
}

pub(super) async fn stop_bot_task(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<StopBotTaskRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let (client, context) = match admitted(&state, principal, AiScope::RunBots) {
        Ok(r) => r,
        Err(r) => return r,
    };
    answer(call(move || stop(&client, owner_context(context), request)).await)
}

fn stop(
    client: &dyn TaskPort,
    context: RequestContext,
    request: StopBotTaskRequest,
) -> Result<CancelAttemptResponse, Failure> {
    let bot = owned_bot(client, &context, request.workspace, request.bot_id)?;
    bound_answer(
        client,
        &context,
        &bot,
        request.request_id,
        request.attempt_id,
        0,
    )?;
    client.cancel_attempt(context, request.attempt_id)
}

#[cfg(test)]
mod tests;
