// SPDX-License-Identifier: MPL-2.0

//! The readings a host takes, and what the surface makes of them.

use super::*;

#[test]
fn a_host_that_has_said_nothing_about_voice_reads_as_absent() {
    assert!(VoiceFacts::default().is_absent());
}

#[test]
fn a_switched_off_installation_is_present_and_blocked() {
    let facts = VoiceFacts {
        blocked: Some("Voice input is switched off on this installation.".to_owned()),
        ..VoiceFacts::default()
    };
    assert!(!facts.is_absent(), "there is a sentence to draw");
}

/// The quality note changes with the selection rather than appearing, so the
/// rows below it do not move when somebody presses.
#[test]
fn each_tier_answers_with_its_own_note() {
    let facts = VoiceFacts {
        tier_notes: vec![
            (DictationTier::Compact, "compact".to_owned()),
            (DictationTier::HighAccuracy, "accurate".to_owned()),
        ],
        ..VoiceFacts::default()
    };
    assert_eq!(facts.tier_note(DictationTier::Compact), "compact");
    assert_eq!(facts.tier_note(DictationTier::HighAccuracy), "accurate");
}

/// **No size rather than a guessed one.** A host that has read no descriptor
/// gives no note, and the row draws nothing rather than inventing a number.
#[test]
fn a_tier_with_no_note_draws_nothing() {
    assert_eq!(VoiceFacts::default().tier_note(DictationTier::Compact), "");
}

#[test]
fn a_host_with_one_backend_offers_no_renderer_row() {
    assert!(!RendererRow::none().is_offered());
    let offered = RendererRow {
        options: vec![("auto".to_owned(), "Auto".to_owned())],
        ..RendererRow::default()
    };
    assert!(offered.is_offered());
}

#[test]
fn an_unfilled_ai_slot_is_empty() {
    assert!(AiSlot::default().is_empty());
    assert!(
        !AiSlot {
            note: "OpenAI".to_owned(),
            action: None
        }
        .is_empty()
    );
}

/// The window's own budget is longer than the authority's, so a window that
/// opened while the Agent was still starting the Gateway is not called down
/// for being early.
#[test]
fn the_first_section_budget_is_longer_than_the_authoritys_own() {
    assert!(
        FIRST_SECTION_DEADLINE
            > std::time::Duration::from_millis(koru_settings::STARTUP_WAIT_MILLIS)
    );
    assert!(PATCH_DEADLINE > std::time::Duration::from_secs(1));
}

#[test]
fn live_is_the_default_and_down_is_the_exception() {
    assert_eq!(Availability::default(), Availability::Live);
    assert!(!Availability::Live.is_down());
    assert!(Availability::Down.is_down());
}

/// An agent that knows the command to run names it, in code style; one that
/// does not keeps the sentence it always had.
#[test]
fn an_attention_row_names_the_agents_command_when_it_has_one() {
    let lang = koru_i18n::Lang::En;
    let words = |why| AgentProviderStatus::NeedsAttention(why).words(lang);
    assert_eq!(
        words(AgentAttention::UnsupportedVersion {
            update: Some("devin update".into())
        }),
        "Needs attention — This version isn't one Koru supports yet. Run `devin update` to update it."
    );
    assert_eq!(
        words(AgentAttention::SignedOut {
            sign_in: Some("devin auth login".into())
        }),
        "Needs attention — Signed out. Run `devin auth login` to sign in."
    );
    assert!(
        words(AgentAttention::UnsupportedVersion { update: None })
            .ends_with("This version isn't one Koru supports yet.")
    );
    assert!(
        words(AgentAttention::SignedOut { sign_in: None })
            .ends_with("Signed out. Sign in from the agent's own CLI.")
    );
}
