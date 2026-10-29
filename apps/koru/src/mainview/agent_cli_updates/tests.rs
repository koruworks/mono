// SPDX-License-Identifier: MPL-2.0

use super::super::harness::container;
use super::*;
use koru_agent_wire::{
    AgentCliChoice, AgentCliReport, AgentCliRow, AgentCliStage, FromShell, Request,
};

fn row(id: &str, name: &str, stage: AgentCliStage) -> AgentCliRow {
    AgentCliRow {
        id: id.into(),
        name: name.into(),
        installed: "0.163.0".into(),
        latest: "0.165.0".into(),
        stage,
        message: None,
        detail: None,
    }
}

fn report(rows: Vec<AgentCliRow>) -> AgentCliReport {
    AgentCliReport {
        checked: true,
        clis: rows,
    }
}

fn sent(queue: &std::sync::mpsc::Receiver<FromShell>) -> Vec<Request> {
    std::iter::from_fn(|| queue.try_recv().ok())
        .map(|FromShell::Request { body, .. }| body)
        .collect()
}

#[test]
fn only_rows_that_need_attention_become_cards_and_a_running_one_cannot_be_closed() {
    let (mut v, _workspace) = container();
    let (feed, _queue) = crate::agent::AgentCliFeed::for_test(report(vec![
        row("claude", "Claude", AgentCliStage::UpToDate),
        row("codex", "Codex", AgentCliStage::Available),
        row("grok", "Grok", AgentCliStage::Updating),
        row("kimi", "Kimi", AgentCliStage::Failed),
    ]));
    v.agent_clis = feed;
    let cards = v.agent_cli_cards();
    assert_eq!(
        cards
            .iter()
            .map(|card| card.row.id.as_str())
            .collect::<Vec<_>>(),
        ["codex", "grok", "kimi"],
        "an up-to-date CLI has no card"
    );
    assert_eq!(
        cards[0].action.map(|(key, _)| key),
        Some(Key::AgentCliUpdate)
    );
    assert!(cards[0].close.is_some());
    assert_eq!(cards[1].action, None, "a running updater cannot be re-run");
    assert_eq!(cards[1].close, None, "nor dismissed");
    assert_eq!(cards[2].action.map(|(key, _)| key), Some(Key::TryAgain));
    assert!(cards[2].close.is_some());
    // Stacked, not overlapping, from the top right.
    assert!(cards[1].rect.y >= cards[0].rect.y + cards[0].rect.h);
    assert!(cards[0].rect.x + cards[0].rect.w <= v.viewport.0);
    assert!(v.agent_cli_animating(), "a moving bar asks for frames");
}

#[test]
fn update_sends_one_choice_and_a_second_press_while_it_is_in_flight_sends_nothing() {
    let (mut v, mut workspace) = container();
    let (feed, queue) = crate::agent::AgentCliFeed::for_test(report(vec![row(
        "codex",
        "Codex",
        AgentCliStage::Available,
    )]));
    v.agent_clis = feed;
    let (_, button) = v.agent_cli_cards()[0].action.unwrap();
    assert!(
        v.mouse_down(&mut workspace.ws, button.x + 2.0, button.y + 2.0, false),
        "the card claims the press"
    );
    assert_eq!(
        sent(&queue),
        vec![Request::AgentCliChoice {
            choice: AgentCliChoice::Update { id: "codex".into() }
        }]
    );
    assert!(v.mouse_down(&mut workspace.ws, button.x + 2.0, button.y + 2.0, false));
    assert!(sent(&queue).is_empty(), "one press, one run");

    // The agent answered: the card moved to Updating, which ends the press in
    // flight, and the card for a later failure takes a fresh press.
    v.agent_clis
        .set_for_test(report(vec![row("codex", "Codex", AgentCliStage::Updating)]));
    assert_eq!(v.agent_cli_cards()[0].action, None);
    v.agent_clis
        .set_for_test(report(vec![row("codex", "Codex", AgentCliStage::Failed)]));
    let (key, retry) = v.agent_cli_cards()[0].action.unwrap();
    assert_eq!(key, Key::TryAgain);
    assert!(v.mouse_down(&mut workspace.ws, retry.x + 2.0, retry.y + 2.0, false));
    assert_eq!(
        sent(&queue),
        vec![Request::AgentCliChoice {
            choice: AgentCliChoice::Update { id: "codex".into() }
        }]
    );
}

#[test]
fn the_close_dismisses_and_a_press_on_the_card_body_is_claimed_but_sends_nothing() {
    let (mut v, mut workspace) = container();
    let (feed, queue) = crate::agent::AgentCliFeed::for_test(report(vec![row(
        "grok",
        "Grok",
        AgentCliStage::Available,
    )]));
    v.agent_clis = feed;
    let card = &v.agent_cli_cards()[0];
    let (rect, close) = (card.rect, card.close.unwrap());
    assert!(v.mouse_down(&mut workspace.ws, rect.x + 4.0, rect.y + 4.0, false));
    assert!(sent(&queue).is_empty());
    assert!(v.mouse_down(&mut workspace.ws, close.x + 2.0, close.y + 2.0, false));
    assert_eq!(
        sent(&queue),
        vec![Request::AgentCliChoice {
            choice: AgentCliChoice::Dismiss { id: "grok".into() }
        }]
    );
    assert!(
        !v.agent_cli_mouse_down(10.0, rect.y + 4.0),
        "a press beside the stack is not the stack's"
    );
}

#[test]
fn the_card_says_what_it_offers_in_the_person_s_words() {
    let mut available = row("codex", "Codex", AgentCliStage::Available);
    assert_eq!(
        copy(&available, Lang::En),
        (
            "Codex update available".to_owned(),
            "v0.163.0 → v0.165.0".to_owned()
        )
    );
    available.stage = AgentCliStage::Updating;
    assert_eq!(copy(&available, Lang::En).1, "Starting…", "no output yet");
    available.message = Some("Downloading…".into());
    assert_eq!(
        copy(&available, Lang::En),
        ("Updating Codex".to_owned(), "Downloading…".to_owned())
    );
    available.stage = AgentCliStage::Updated;
    available.installed = "0.165.0".into();
    assert_eq!(
        copy(&available, Lang::En),
        ("Codex updated".to_owned(), "v0.165.0".to_owned())
    );
    available.stage = AgentCliStage::Failed;
    available.message = Some("npm ERR! code EACCES".into());
    assert_eq!(
        copy(&available, Lang::En),
        (
            "Codex update failed".to_owned(),
            "npm ERR! code EACCES".to_owned()
        )
    );
    assert_eq!(
        copy(&row("grok", "Grok", AgentCliStage::Available), Lang::De).0,
        "Update für Grok verfügbar",
        "the brand name is placed by the translation, not appended"
    );
}

#[test]
fn a_finished_card_puts_itself_away_after_its_time_and_only_once() {
    let (mut v, _workspace) = container();
    let (feed, queue) = crate::agent::AgentCliFeed::for_test(report(vec![row(
        "grok",
        "Grok",
        AgentCliStage::Updated,
    )]));
    v.agent_clis = feed;
    let cards = v.agent_cli_cards();
    let start = Instant::now();
    v.agent_cli_tend(&cards, start);
    v.agent_cli_tend(&cards, start + Duration::from_secs(1));
    assert!(sent(&queue).is_empty(), "still being read");
    v.agent_cli_tend(&cards, start + SHOWN_WHEN_DONE);
    assert_eq!(
        sent(&queue),
        vec![Request::AgentCliChoice {
            choice: AgentCliChoice::Dismiss { id: "grok".into() }
        }]
    );
    v.agent_cli_tend(&cards, start + SHOWN_WHEN_DONE + Duration::from_secs(1));
    assert!(
        sent(&queue).is_empty(),
        "the dismissal is sent once; the agent's next report removes the card"
    );
}

#[test]
fn a_card_offers_dismiss_beside_its_action_and_the_label_sends_it() {
    let (mut v, mut workspace) = container();
    let (feed, queue) = crate::agent::AgentCliFeed::for_test(report(vec![
        row("codex", "Codex", AgentCliStage::Available),
        row("kimi", "Kimi", AgentCliStage::Updating),
        row("grok", "Grok", AgentCliStage::Updated),
    ]));
    v.agent_clis = feed;
    let cards = v.agent_cli_cards();
    let (action, dismiss) = (
        cards[0].action.as_ref().unwrap().1,
        cards[0].dismiss.unwrap(),
    );
    assert!(
        dismiss.x + dismiss.w <= action.x,
        "the label sits left of the action, not over it"
    );
    assert_eq!(dismiss.y, action.y, "on the same line");
    assert!(cards[0].close.is_some(), "the ✕ stays at the corner");
    assert_eq!(
        cards[1].dismiss, None,
        "a running updater cannot be dismissed"
    );
    assert_eq!(
        cards[2].dismiss, None,
        "a finished card has no action to sit beside"
    );

    assert!(v.mouse_down(&mut workspace.ws, dismiss.x + 2.0, dismiss.y + 2.0, false));
    assert_eq!(
        sent(&queue),
        vec![Request::AgentCliChoice {
            choice: AgentCliChoice::Dismiss { id: "codex".into() }
        }]
    );
}

#[test]
fn over_a_card_the_pointer_is_the_plain_arrow_and_elsewhere_it_is_not_claimed() {
    let (mut v, _workspace) = container();
    let (feed, _queue) = crate::agent::AgentCliFeed::for_test(report(vec![row(
        "kimi",
        "Kimi",
        AgentCliStage::Available,
    )]));
    v.agent_clis = feed;
    let card = &v.agent_cli_cards()[0];
    let (action, rect) = (card.action.as_ref().unwrap().1, card.rect);
    for (x, y) in [
        (action.x + 2.0, action.y + 2.0),
        (rect.x + 4.0, rect.y + 4.0),
    ] {
        v.pointer = (x, y);
        assert_eq!(v.agent_cli_cursor(), Some(koru_ui::cursor::PRESS));
        assert_eq!(
            koru_workspace_host::View::cursor_icon(&v),
            koru_ui::cursor::PRESS
        );
    }
    v.pointer = (4.0, rect.y + 4.0);
    assert_eq!(v.agent_cli_cursor(), None);
}

#[test]
fn the_card_contents_nest_under_its_floating_panel() {
    use koru_draw::inspect;
    let (mut v, _workspace) = container();
    let (feed, _queue) = crate::agent::AgentCliFeed::for_test(report(vec![row(
        "kimi",
        "Kimi",
        AgentCliStage::Available,
    )]));
    v.agent_clis = feed;
    let mut canvas = vello::Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 1200.0, 800.0));
    v.draw_agent_cli_cards(&mut canvas, root);
    let records = capture.finish().records;
    let panel = records
        .iter()
        .position(|r| r.name == "FloatingPanel")
        .expect("the panel");
    let mut inside = 0;
    for (index, record) in records.iter().enumerate() {
        if index == panel {
            continue;
        }
        let mut at = index;
        while records[at].parent != Some(panel) {
            at = records[at]
                .parent
                .unwrap_or_else(|| panic!("{} escaped the panel", record.name));
        }
        if record.name == "text line" || record.name == "KoruButton" {
            inside += 1;
        }
    }
    assert!(
        inside >= 4,
        "the title, the version line and both buttons are the panel's: {inside}"
    );
}
