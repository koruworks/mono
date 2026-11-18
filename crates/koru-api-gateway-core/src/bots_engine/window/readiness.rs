// SPDX-License-Identifier: MPL-2.0
//! Native discovery is a gateway precondition, never engine actor work.
use super::*;
use koru_ai_wire::Failure;

pub(in crate::bots_engine) enum Refusal {
    Engine(Error),
    Runtime(Failure),
}
impl From<Error> for Refusal {
    fn from(error: Error) -> Self {
        Self::Engine(error)
    }
}
impl Refusal {
    pub(in crate::bots_engine) fn response(self, window: bool) -> Response {
        match self {
            Self::Engine(error) if window => window_failed(error),
            Self::Engine(error) => failed(error),
            Self::Runtime(failure) => {
                let status = crate::ai::status_for(&failure);
                (status, Json(failure)).into_response()
            }
        }
    }
}

fn unavailable() -> Failure {
    Failure {
        code: "runtime_unavailable".into(),
        message: "The selected runtime is unavailable.".into(),
        retryable: true,
        details: std::collections::BTreeMap::from([(
            "reason".into(),
            Value::String(koru_ai_wire::agent_readiness_reasons::SERVICE_UNAVAILABLE.into()),
        )]),
    }
}

pub(in crate::bots_engine) fn explain(
    mut failure: Failure,
    readiness: impl FnOnce() -> Result<koru_ai_wire::ProviderValidationResponse, Failure>,
) -> Failure {
    if failure.code == "agent_service_unavailable" {
        return unavailable();
    }
    if failure.code == "agent_models_failed"
        && let Ok(readiness) = readiness()
    {
        let reason = readiness
            .failure
            .and_then(|failure| failure.details.get("reason").cloned())
            .or_else(|| {
                (readiness.status == koru_ai_wire::ProviderValidationStatus::Ready).then(|| {
                    Value::String(koru_ai_wire::agent_readiness_reasons::LISTING_FAILED.into())
                })
            });
        if let Some(reason) = reason {
            failure.details.insert("reason".into(), reason);
        }
    }
    failure
}

pub(in crate::bots_engine) fn validate(
    client: Option<&koru_ai_client::Client>,
    configuration: &Configuration,
) -> Result<(), Failure> {
    let client = client.ok_or_else(unavailable)?;
    let selection = model_view(configuration).map_err(|_| Failure {
        code: "invalid_request".into(),
        message: "The selected runtime settings are invalid.".into(),
        retryable: false,
        details: Default::default(),
    })?;
    // Called only after first-party ManageBots admission. This internal
    // context permits catalog discovery, never provider settings mutation.
    let context = || koru_ai_api::RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "Bot runtime validation".into(),
            scopes: vec![koru_ai_api::AiScope::ManageProviders],
        },
        trace_id: Uuid::new_v4().to_string(),
        deadline_millis: None,
    };
    let catalog = client
        .discover_agent_models(
            context(),
            selection.provider.clone(),
            koru_ai_wire::DiscoverAgentModelsRequest { refresh: false },
        )
        .map_err(|failure| {
            explain(failure, || {
                client.validate_provider(context(), selection.provider.clone())
            })
        })?;
    koru_ai_server::validate_agent_model_catalog(
        &selection,
        configuration
            .selection
            .mode
            .as_ref()
            .map(|mode| mode.as_str()),
        configuration.selection.fast == Some(true),
        &catalog,
    )
    .map(|_| ())
}

impl Gateway {
    fn validate_change_runtime(
        &self,
        owner: BotOwner,
        command: &Command,
        validate: &impl Fn(&Configuration) -> Result<(), Failure>,
    ) -> Result<(), Refusal> {
        let Command::Change { id, action } = command else {
            return Ok(());
        };
        let configuration = match action {
            Change::Create { definition, .. } => definition.configuration.clone(),
            Change::Configure { configuration, .. } => configuration.clone(),
            Change::ConversationSettings {
                provider,
                selection,
                ..
            } => Configuration {
                provider: provider.clone(),
                selection: selection.clone(),
                working_folder: None,
            },
            _ => return Ok(()),
        };
        let receipt: Value = self.person_read(
            owner,
            Query::ChangeReceipt {
                id: *id,
                action: action.clone(),
            },
        )?;
        if !receipt.is_null() {
            return Ok(());
        }
        let workspace = match action {
            Change::Create { definition, .. } => definition.access.workspace,
            Change::Configure { bot, .. } | Change::ConversationSettings { bot, .. } => {
                let bot: person::Bot = self.person_read(owner, Query::Bot { bot: *bot })?;
                bot.definition.access.workspace
            }
            _ => unreachable!(),
        };
        self.window_workspace(owner, workspace)?;
        validate(&configuration).map_err(Refusal::Runtime)
    }

    pub(in crate::bots_engine) fn validated_execute(
        &self,
        principal: &GatewayPrincipal,
        command: Command,
        validate: impl Fn(&Configuration) -> Result<(), Failure>,
    ) -> Result<Value, Refusal> {
        if !permitted(principal) {
            return Err(Error::NotAllowed.into());
        }
        let owner = self.authority.owner().map_err(authority_error)?;
        self.validate_change_runtime(owner, &command, &validate)?;
        self.execute(principal, command).map_err(Into::into)
    }

    pub(in crate::bots_engine) fn validated_window_command(
        &self,
        command: BotCommand,
        validate: impl Fn(&Configuration) -> Result<(), Failure>,
    ) -> Result<BotResponse, Refusal> {
        let owner = self.authority.owner().map_err(authority_error)?;
        match &command {
            BotCommand::CreateBotWithRole {
                request_id,
                home_workspace,
                config,
                ..
            } => {
                let source_hash =
                    blake3::hash(&serde_json::to_vec(&command).map_err(|_| Error::InvalidRequest)?)
                        .to_hex()
                        .to_string();
                match self.creation_call(
                    owner,
                    c::Command::Check {
                        request: request_id.as_uuid(),
                        source_hash,
                    },
                )? {
                    c::Reply::Record(Some(_)) => {}
                    c::Reply::Record(None) => {
                        self.window_workspace(owner, *home_workspace)?;
                        validate(&configured(config, None)?).map_err(Refusal::Runtime)?;
                    }
                    _ => return Err(Error::EngineUnavailable.into()),
                }
            }
            BotCommand::Update {
                request_id,
                bot_id,
                expected_revision,
                config,
            } => {
                let action =
                    self.update_action(owner, *bot_id, *expected_revision, config.clone())?;
                self.validate_change_runtime(
                    owner,
                    &Command::Change {
                        id: request_id.as_uuid(),
                        action,
                    },
                    &validate,
                )?;
            }
            _ => {}
        }
        self.window_command(command).map_err(Into::into)
    }
}
