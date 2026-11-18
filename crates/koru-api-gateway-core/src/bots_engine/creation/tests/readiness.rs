// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots_engine::window::readiness::{Refusal, validate};
use koru_ai_core::bots::*;

#[test]
fn discovery_refusal_keeps_its_code_and_the_shared_readiness_explanation() {
    use crate::bots_engine::window::readiness::explain;
    use koru_ai_wire::{
        Failure, ProviderValidationResponse, ProviderValidationStatus,
        agent_readiness_reasons as reason,
    };
    let failure = Failure {
        code: "agent_models_failed".into(),
        message: "Listing failed".into(),
        retryable: true,
        details: Default::default(),
    };
    let explained = explain(failure.clone(), || {
        Ok(ProviderValidationResponse {
            provider: koru_ai_core::AiProviderId::parse("agent-claude").unwrap(),
            status: ProviderValidationStatus::Unavailable,
            models: vec![],
            selected_model: None,
            failure: Some(Failure {
                code: "agent_signed_out".into(),
                message: "Sign in".into(),
                retryable: true,
                details: std::collections::BTreeMap::from([(
                    "reason".into(),
                    serde_json::json!(reason::SIGNED_OUT),
                )]),
            }),
        })
    });
    assert_eq!(explained.code, failure.code);
    assert_eq!(explained.message, failure.message);
    assert_eq!(explained.retryable, failure.retryable);
    assert_eq!(explained.details["reason"], reason::SIGNED_OUT);
    assert_eq!(explain(failure.clone(), || Err(failure.clone())), failure);
    let absent = explain(
        Failure {
            code: "agent_service_unavailable".into(),
            ..failure
        },
        || panic!("no service to ask"),
    );
    assert_eq!(absent.code, "runtime_unavailable");
    assert_eq!(absent.details["reason"], reason::SERVICE_UNAVAILABLE);
}

#[test]
fn unavailable_runtime_creates_nothing_and_exact_retries_skip_discovery() {
    let f = Fixture::new();
    let command = form(&f);
    let BotCommand::CreateBotWithRole { request_id, .. } = &command else {
        panic!()
    };
    let Err(Refusal::Runtime(failure)) = f
        .gateway
        .validated_window_command(command.clone(), |c| validate(None, c))
    else {
        panic!()
    };
    assert_eq!(failure.code, "runtime_unavailable");
    assert_eq!(
        failure.details["reason"],
        koru_ai_wire::agent_readiness_reasons::SERVICE_UNAVAILABLE
    );
    assert!(
        f.gateway
            .creation_read(f.store.owner, request_id.as_uuid())
            .unwrap()
            .is_none()
    );
    assert_eq!(f.fs.creates.load(Ordering::Relaxed), 0);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 0);
    let accepted = creation(
        f.gateway
            .validated_window_command(command.clone(), |_| Ok(()))
            .ok()
            .unwrap(),
    );
    assert!(accepted.bot.is_some());
    let retried = creation(
        f.gateway
            .validated_window_command(command.clone(), |_| {
                panic!("exact retry rediscovered runtime")
            })
            .ok()
            .unwrap(),
    );
    assert_eq!(accepted, retried);
    let mut changed = command;
    if let BotCommand::CreateBotWithRole { config, .. } = &mut changed {
        config.name.push('!');
    }
    assert!(matches!(
        f.gateway.validated_window_command(changed, |_| panic!(
            "conflicting retry rediscovered runtime"
        )),
        Err(Refusal::Engine(Error::Conflict))
    ));
}

#[test]
fn settings_validate_before_commit_and_retained_receipt_survives_runtime_loss() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let mut config = bot.config.clone();
    config.name = "Validated".into();
    let update = BotCommand::Update {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: bot.id,
        expected_revision: bot.revision,
        config,
    };
    assert!(matches!(
        f.gateway
            .validated_window_command(update.clone(), |c| validate(None, c)),
        Err(Refusal::Runtime(_))
    ));
    let BotResponse::Bot { bot: current } = f
        .gateway
        .window_command(BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(current, bot);
    let BotResponse::Bot { bot: saved } = f
        .gateway
        .validated_window_command(update.clone(), |_| Ok(()))
        .ok()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(saved.config.name, "Validated");
    let BotResponse::Bot { bot: replayed } = f
        .gateway
        .validated_window_command(update, |_| panic!("retained settings rediscovered runtime"))
        .ok()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(replayed, saved);
}

#[test]
fn private_person_carrier_cannot_bypass_readiness_for_new_configuration() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let current: person::Bot = serde_json::from_value(
        f.gateway
            .execute_owner(
                f.store.owner,
                Command::Read {
                    query: Query::Bot {
                        bot: bot.id.as_uuid(),
                    },
                },
            )
            .unwrap(),
    )
    .unwrap();
    let command = Command::Change {
        id: Uuid::new_v4(),
        action: Change::Configure {
            bot: current.id,
            revision: current.revision,
            name: "Not saved".into(),
            avatar: current.definition.avatar,
            configuration: current.definition.configuration,
            role: current.definition.role,
        },
    };
    let principal = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![koru_ai_api::AiScope::ManageBots.as_str().into()],
    };
    assert!(matches!(
        f.gateway
            .validated_execute(&principal, command, |c| validate(None, c)),
        Err(Refusal::Runtime(_))
    ));
    let BotResponse::Bot { bot: current } = f
        .gateway
        .window_command(BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(current, bot);
}
