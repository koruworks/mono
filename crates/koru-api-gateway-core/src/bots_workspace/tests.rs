// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_bots_engine::{authority::Binding, messages::Turn};
use std::cell::RefCell;

struct ResolverFixture(RefCell<Result<Resolution, Error>>, RefCell<usize>);
impl Resolver for ResolverFixture {
    fn catalog(&self, _: &Secret) -> Result<Value, Error> {
        self.0
            .borrow()
            .clone()
            .map(|resolved| resolved.captured_grant)
    }
    fn resolve(&self, _: &Secret, _: Uuid, _: &Call) -> Result<Resolution, Error> {
        *self.1.borrow_mut() += 1;
        self.0.borrow().clone()
    }
}

#[test]
fn discovery_uses_captured_grant_and_revocation_is_not_cached() {
    let (resolver, secret, _, _) = fixture();
    let tools = catalog(&resolver, &secret).unwrap();
    assert_eq!(tools.as_array().unwrap().len(), 4);
    for tool in tools.as_array().unwrap() {
        let write = matches!(
            tool["name"].as_str(),
            Some("create_document" | "edit_document")
        );
        assert_eq!(
            tool["inputSchema"]["properties"]
                .get("operation_id")
                .is_some(),
            write
        );
        if write {
            assert!(
                tool["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "operation_id")
            );
        }
    }
    assert_eq!(*resolver.1.borrow(), 0, "discovery has no resource call");
    *resolver.0.borrow_mut() = Err(Error::CredentialRevoked);
    assert_eq!(
        catalog(&resolver, &secret).err(),
        Some(Error::CredentialRevoked)
    );
}
fn fixture() -> (ResolverFixture, Secret, Uuid, Call) {
    let workspace = WorkspaceId::new();
    let grant = serde_json::to_value(CapturedBotPolicy {
        workspace,
        destination: workspace,
        access: koru_delegate_core::BotAccess::Default,
        selected_pages: Default::default(),
        connections: Default::default(),
    })
    .unwrap();
    let binding = Binding {
        id: Uuid::new_v4(),
        turn: Turn {
            id: Uuid::new_v4(),
            generation: 1,
            worker: Uuid::new_v4(),
            session: Uuid::new_v4(),
        },
    };
    let resolved = Resolution {
        scope: Uuid::new_v4().to_string(),
        bot: Uuid::new_v4(),
        thread: Uuid::new_v4(),
        binding,
        captured_grant: grant.clone(),
        current_grant: grant,
    };
    (
        ResolverFixture(RefCell::new(Ok(resolved)), RefCell::new(0)),
        Secret::parse("ab".repeat(32)).unwrap(),
        binding.id,
        Call::parse(Uuid::new_v4(), "read_document".into(), br#"{}"#).unwrap(),
    )
}

#[test]
fn every_forwarding_attempt_resolves_again_and_revocation_prevents_store_access() {
    let (resolver, secret, binding, call) = fixture();
    let request = || workspace::Request::new(secret.clone(), binding, &call).unwrap();
    assert!(execute(&resolver, request(), |_, _| Ok(Value::Null)).is_ok());
    *resolver.0.borrow_mut() = Err(Error::CredentialRevoked);
    assert_eq!(
        execute(&resolver, request(), |_, _| panic!(
            "revoked call reached Store"
        )),
        Err(Error::CredentialRevoked)
    );
    assert_eq!(*resolver.1.borrow(), 2);
}

#[test]
fn current_grant_cannot_move_a_captured_session_to_another_workspace() {
    let (resolver, secret, binding, call) = fixture();
    {
        let mut result = resolver.0.borrow_mut();
        let resolved = result.as_mut().unwrap();
        let other = WorkspaceId::new().to_storage();
        resolved.current_grant["workspace"] = Value::String(other.clone());
        resolved.current_grant["destination"] = Value::String(other);
    }
    let request = workspace::Request::new(secret, binding, &call).unwrap();
    assert_eq!(
        execute(&resolver, request, |_, _| panic!(
            "widened grant reached Store"
        )),
        Err(Error::NotAllowed)
    );
}

#[test]
fn no_mutation_or_unsupported_tool_can_use_the_read_adapter() {
    for tool in [
        "create_document",
        "edit_document",
        "remember_memory",
        "unknown",
    ] {
        let (resolver, secret, binding, _) = fixture();
        let call = Call::parse(Uuid::new_v4(), tool.into(), br#"{}"#).unwrap();
        let request = workspace::Request::new(secret, binding, &call).unwrap();
        assert_eq!(
            execute(&resolver, request, |_, _| panic!(
                "mutation reached read adapter"
            )),
            Err(Error::NotAllowed)
        );
    }
}

#[test]
fn duplicate_keys_are_refused_before_resolution() {
    let (resolver, secret, binding, call) = fixture();
    let mut request = workspace::Request::new(secret, binding, &call).unwrap();
    request.arguments = br#"{"document_id":"one","document_id":"two"}"#.to_vec();
    assert_eq!(
        execute(&resolver, request, |_, _| panic!()),
        Err(Error::InvalidRequest)
    );
    assert_eq!(*resolver.1.borrow(), 0);
}

#[test]
fn context_requires_engine_authority_and_reads_only_configured_current_pages() {
    let (resolver, engine_authority, _, _) = fixture();
    assert!(same_authority(&engine_authority, &engine_authority));
    assert!(!same_authority(
        &Secret::parse("cd".repeat(32)).unwrap(),
        &engine_authority
    ));
    let resolved = resolver.0.borrow().clone().unwrap();
    let role = Uuid::new_v4();
    let knowledge = Uuid::new_v4();
    let memory = Uuid::new_v4();
    let sources = workspace::Sources {
        scope: resolved.scope,
        grant: resolved.current_grant,
        role: Some(role),
        knowledge: vec![knowledge],
        memory: Some(memory),
    };
    let mut seen = vec![];
    let context = load_context(sources.clone(), |_, id| {
        seen.push(id);
        Ok(serde_json::json!({"text":"current synthetic page"}))
    })
    .unwrap();
    assert_eq!(seen, vec![role, knowledge, memory]);
    assert_eq!(context.role.unwrap().id, role);
    assert_eq!(context.knowledge[0].id, knowledge);
    assert_eq!(context.memory.unwrap().id, memory);
    assert_eq!(
        load_context(sources.clone(), |_, _| Err(Error::NotAllowed)).err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(
        load_context(sources, |_, _| Ok(Value::String("x".repeat(1024 * 1024)))).err(),
        Some(Error::InputTooSmall)
    );
}
