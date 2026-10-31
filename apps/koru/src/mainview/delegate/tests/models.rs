// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::{CapabilitySet, ProviderModel, ProviderModels, capability};
fn model_world() -> World {
    let mut world = a_world(workspace());
    world.objective = "Review this project".into();
    let mut open = world.agents[0].clone();
    open.provider = "opencode".parse().unwrap();
    open.name = "OpenCode".into();
    open.key = koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| {
            profile
                .provider
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "opencode")
        })
        .unwrap()
        .id;
    open.readiness.capabilities = CapabilitySet::new(
        open.readiness
            .capabilities
            .iter()
            .cloned()
            .chain([capability::PROVIDER_MODELS.parse().unwrap()]),
    );
    world.agents.insert(0, open);
    world
}
fn catalog(ids: &[&str]) -> ProviderModels {
    ids.iter()
        .map(|id| {
            ProviderModel::new(
                id.parse().unwrap(),
                id.split_once('/').unwrap().0.parse().unwrap(),
            )
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}
#[test]
fn folder_change_fences_late_catalog_and_retains_explicit_choice_until_validation() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, model_world());
    let (first, _) = modal.model_discovery().unwrap();
    assert!(modal.accept_models(first, Ok(catalog(&["deepseek/chat"]))));
    modal.activate(Hit::Model(0));
    assert!(modal.ready());
    modal.scope = Some(if modal.scope == Some(0) { 1 } else { 0 });
    let (next, _) = modal.model_discovery().unwrap();
    assert!(!modal.ready());
    assert!(!modal.accept_models(first, Ok(catalog(&["deepseek/chat"]))));
    assert!(modal.accept_models(next, Ok(catalog(&["other/model"]))));
    assert!(!modal.ready());
    assert!(modal.models.selected.is_some());
    modal.activate(Hit::ModelDefault);
    assert!(modal.ready());
}
#[test]
fn search_focus_paging_and_agent_switch_preserve_the_draft() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, model_world());
    let (request, _) = modal.model_discovery().unwrap();
    let ids: Vec<_> = (0..25)
        .map(|index| format!("provider/model-{index:02}"))
        .collect();
    assert!(modal.accept_models(
        request,
        Ok(catalog(&ids.iter().map(String::as_str).collect::<Vec<_>>()))
    ));
    assert_eq!(modal.models.visible().count(), 8);
    modal.activate(Hit::ModelNext);
    assert_eq!(modal.models.visible().next(), Some(8));
    modal.activate(Hit::Model(8));
    modal.activate(Hit::ModelSearch);
    assert_eq!(modal.focused(), Some(Hit::ModelSearch));
    modal.key(
        &mut engine,
        &Key::Character("24".into()),
        Some("24"),
        ModifiersState::empty(),
    );
    assert_eq!(modal.models.visible().collect::<Vec<_>>(), vec![24]);
    assert_eq!(
        modal.models.selected.as_ref().unwrap().as_str(),
        "provider/model-08"
    );
    modal.activate(Hit::Agent(1));
    assert!(modal.model_discovery().is_none());
    assert!(modal.models.selected.is_none());
    assert!(modal.ready());
    assert_eq!(modal.objective.value(), "Review this project");
}
#[test]
fn unsupported_retained_choice_is_visible_and_requires_an_explicit_default() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Review".into();
    world.chosen_model = Some(koru_delegate_core::ModelName::new("quoted\"model").unwrap());
    let mut modal = open(&mut engine, world);
    assert!(modal.model_discovery().is_none());
    assert!(modal.models.offered);
    assert!(!modal.ready());
    assert!(modal.controls().contains(&Hit::ModelDefault));
    modal.activate(Hit::ModelDefault);
    assert!(modal.ready());
}

#[test]
fn readiness_loss_retires_visible_model_rows_without_erasing_selected_intent() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, model_world());
    let (request, _) = modal.model_discovery().unwrap();
    modal.accept_models(request, Ok(catalog(&["deepseek/chat"])));
    modal.activate(Hit::Model(0));
    modal.world.agents[0].readiness.capabilities = Default::default();
    assert!(modal.model_discovery().is_none());
    assert_eq!(modal.models.visible().count(), 0);
    assert!(modal.models.selected.is_some());
    assert!(modal.models.offered);
    assert!(!modal.ready());
    let layout = super::super::paint::layout(&mut engine, &modal, (900.0, 900.0));
    assert!(layout.iter().any(|(hit, _)| *hit == Hit::ModelDefault));
}
