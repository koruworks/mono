// SPDX-License-Identifier: MPL-2.0

//! The same deliberate endpoint failure, before explicit cleanup, on each pin.
use super::*;
use koru_delegate_core::{ControlState, TimelineEntry, TurnOriginator};

const FAILURE_OBJECTIVE: &str = "This is an intentional missing-file test. Use only tool metadata discovery needed to locate the Koru run MCP create tool, then call create exactly once with path missing-fixture.txt, title Generic qualification, and mime text/plain. The staging directory exists but that file deliberately does not. After the expected tool error, finish. Do not retry, repair, create or read files, use shell or other actions, or start child agents.";

#[test]
fn live_codex_missing_file_when_opted_in() {
    qualify_failure(&CODEX);
}

#[test]
fn live_claude_missing_file_when_opted_in() {
    qualify_failure(&CLAUDE);
}

#[test]
fn live_grok_missing_file_when_opted_in() {
    qualify_failure(&GROK);
}

fn qualify_failure(provider: &Provider) {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let LiveFixture {
        agent,
        client,
        sessions,
        mut request,
        directory,
        version,
    } = live_fixture(provider);
    request.objective = Objective::new(FAILURE_OBJECTIVE).unwrap();
    let run = request.run;
    let staging = directory.join(".artifacts").join(run.to_storage());
    std::fs::create_dir_all(&staging).unwrap();
    assert!(!staging.join("missing-fixture.txt").exists());
    client
        .runs()
        .create(&request)
        .expect("create deliberate failure run");
    let mut finished = None;
    let mut answered = std::collections::BTreeSet::new();
    let started = Instant::now();
    let mut diagnosed = false;
    wait_until(
        "missing-file turn ends needing attention",
        LIVE_DEADLINE,
        || {
            let view = client.runs().run(run, None, 0, 500).unwrap();
            if !diagnosed
                && (view.header.status.is_terminal()
                    || started.elapsed() >= LIVE_DEADLINE.saturating_sub(Duration::from_secs(3)))
            {
                print_diagnostic(&agent, &sessions, run, &view);
                diagnosed = true;
            }
            assert!(
                !view.header.status.is_terminal(),
                "missing-file run must remain nonterminal: {}",
                view.header.status.name()
            );
            if matches!(&view.header.status, RunStatus::Running { attention, .. }
            if attention.contains(Attention::FinishedNeedingAttention))
            {
                finished = Some(view);
                true
            } else {
                answer_known_fixture_approval(&agent, &sessions, run, provider, &mut answered);
                false
            }
        },
    );
    let view = finished.unwrap();
    assert!(
        !staging.join("missing-fixture.txt").exists(),
        "the provider must not repair the deliberately absent file"
    );
    let versions = client.artifacts().run_versions(run).unwrap();
    let timeline = steps(&view);
    let entries: Vec<_> = view
        .timeline
        .items
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Step(entry) => Some(entry),
            _ => None,
        })
        .collect();
    bind_original_turn(&entries).expect("one original turn identity across start, tools and end");
    let mut endpoint_refusals = 0;
    wait_until(
        "gateway records the exact missing-file refusal",
        PROMPT,
        || {
            let (known, other) = endpoint_failure_counts(&agent.log("koru-api-gateway.log"), run);
            assert!(
                other == 0 && known <= 1,
                "unexpected or repeated run endpoint refusal"
            );
            endpoint_refusals = known;
            known == 1
        },
    );
    let (calls, failed) = failure_shape(
        &view.header.status,
        &timeline,
        view.header.artifacts.len(),
        versions.versions.len(),
        provider.maximum_discovery_calls,
    )
    .unwrap_or_else(|reason| {
        print_diagnostic(&agent, &sessions, run, &view);
        panic!("missing-file parity refused: {reason}")
    });
    let session = session_for(&agent, run).expect("failure attempt has a session");
    let summary = sessions
        .list()
        .unwrap()
        .into_iter()
        .find(|row| row.id == session)
        .expect("nonterminal failure keeps its session");
    assert!(
        summary.active_turn_id.is_none(),
        "missing-file result cannot hide a resubmitted turn"
    );
    assert!(summary.origin.chat().is_none());
    // Capture the real nonterminal failure shape before cleanup changes state.
    eprintln!(
        "live_failure_parity provider={} version={} version_source={} state=running attention=finished_needing_attention turns=1 tool_calls={} failed_tools={} artifacts=0 endpoint_file_not_found={}",
        provider.kind,
        version,
        if provider.package.is_empty() {
            "qualification_pin"
        } else {
            "installed_package"
        },
        calls,
        failed,
        endpoint_refusals
    );
    client
        .runs()
        .cancel(run)
        .expect("explicit cleanup of nonterminal failure");
    wait_until("failure cleanup cancelled", PROMPT, || {
        client
            .runs()
            .run(run, None, 0, 500)
            .is_ok_and(|view| view.header.status == RunStatus::Cancelled)
    });
    verify_closed(&agent, &client, &sessions, run);
    let log = agent.delegate_log();
    for private in [FAILURE_OBJECTIVE, "koru_run_", "missing-fixture.txt"] {
        assert!(
            !log.contains(private),
            "failure fixture content reached delegate log"
        );
    }
    eprintln!(
        "live_failure_cleanup provider={} authority=revoked session=exited",
        provider.kind
    );
}

fn bind_original_turn(entries: &[&TimelineEntry]) -> Result<(), &'static str> {
    let original = entries
        .iter()
        .find(|entry| {
            matches!(
                entry.kind,
                StepKind::TurnStarted {
                    originator: TurnOriginator::Orchestrator
                }
            )
        })
        .ok_or("missing original turn start")?;
    let turn_of = |entry: &TimelineEntry| {
        let mut turns = entry.refs.iter().filter_map(|reference| match reference {
            TimelineRef::Turn { turn } => Some(*turn),
            _ => None,
        });
        let turn = turns.next()?;
        turns.next().is_none().then_some(turn)
    };
    let original_turn = turn_of(original).ok_or("missing or ambiguous original turn reference")?;
    for entry in entries {
        if matches!(
            entry.kind,
            StepKind::TurnStarted { .. }
                | StepKind::ToolsCompleted { .. }
                | StepKind::TurnCompleted { .. }
        ) && turn_of(entry) != Some(original_turn)
        {
            return Err("turn identity changed or evidence lost its reference");
        }
    }
    Ok(())
}

/// The real gateway already emits this closed warning at WARN, without debug
/// mode: run_mcp/tools.rs reports tool, local run, code and elapsed time. Parse
/// its exact non-ANSI fmt record, never a provider message or arbitrary prose.
/// The short run Debug identity is sufficient within this isolated one-run
/// failure fixture; it is never emitted by this observer.
fn endpoint_failure_counts(log: &str, run: RunId) -> (u64, u64) {
    let run_field = format!("run={run:?}");
    let mut known = 0;
    let mut other = 0;
    for line in log.lines() {
        let mut fields = line.split_ascii_whitespace();
        let _timestamp = fields.next();
        if fields.next() != Some("WARN")
            || fields.next() != Some("run")
            || fields.next() != Some("tool")
            || fields.next() != Some("refused")
        {
            continue;
        }
        let fields: Vec<_> = fields.collect();
        if !fields.contains(&run_field.as_str()) {
            continue;
        }
        let unique_field = |prefix: &str, expected: &str| {
            let mut values = fields.iter().filter_map(|field| field.strip_prefix(prefix));
            values.next() == Some(expected) && values.next().is_none()
        };
        if unique_field("tool=", "create")
            && unique_field("code=", "file_not_found")
            && fields.iter().filter(|field| **field == run_field).count() == 1
        {
            known += 1;
        } else {
            other += 1;
        }
    }
    (known, other)
}

/// A failed MCP operation still ends a normal model turn. It must not be
/// reclassified from prose, or hidden by a successful discovery in the same
/// coalesced tool batch.
fn failure_shape(
    status: &RunStatus,
    timeline: &[StepKind],
    artifacts: usize,
    versions: usize,
    maximum_discovery_calls: u64,
) -> Result<(u64, u64), &'static str> {
    if !matches!(status, RunStatus::Running { attention, control: ControlState::Orchestrator }
        if attention.contains(Attention::FinishedNeedingAttention))
    {
        return Err("expected orchestrator-owned finished-needing-attention state");
    }
    if artifacts != 0 || versions != 0 {
        return Err("unexpected artifact or version");
    }
    if !timeline.contains(&StepKind::RunCreated) || !timeline.contains(&StepKind::Started) {
        return Err("missing creation or startup");
    }
    let mut starts = 0;
    let mut completions = 0;
    let mut calls = 0_u64;
    let mut failed = 0_u64;
    for step in timeline {
        match step {
            StepKind::TurnStarted {
                originator: TurnOriginator::Orchestrator,
            } => starts += 1,
            StepKind::TurnCompleted {
                originator: TurnOriginator::Orchestrator,
                artifacts_registered: 0,
            } => completions += 1,
            StepKind::ToolsCompleted {
                calls: count,
                failed: failures,
                ..
            } => {
                if failures > count {
                    return Err("invalid failed tool count");
                }
                calls += u64::from(*count);
                failed += u64::from(*failures);
            }
            StepKind::TurnStarted { .. }
            | StepKind::TurnCompleted { .. }
            | StepKind::ArtifactRegistered
            | StepKind::Completed
            | StepKind::TurnFailed { .. }
            | StepKind::TurnCancelled { .. }
            | StepKind::Failed { .. }
            | StepKind::Cancelled { .. }
            | StepKind::Interrupted { .. }
            | StepKind::Resumed { .. }
            | StepKind::ChildSessionDetected => return Err("unexpected turn or terminal outcome"),
            _ => {}
        }
    }
    if starts != 1 || completions != 1 {
        return Err("expected one original turn without retry");
    }
    let created = timeline
        .iter()
        .position(|step| *step == StepKind::RunCreated)
        .unwrap();
    let started = timeline
        .iter()
        .position(|step| *step == StepKind::Started)
        .unwrap();
    let turn_start = timeline
        .iter()
        .position(|step| matches!(step, StepKind::TurnStarted { .. }))
        .unwrap();
    let turn_end = timeline
        .iter()
        .position(|step| matches!(step, StepKind::TurnCompleted { .. }))
        .unwrap();
    if !(created < started && started < turn_start && turn_start < turn_end)
        || timeline.iter().enumerate().any(|(index, step)| {
            matches!(step, StepKind::ToolsCompleted { .. })
                && !(turn_start < index && index < turn_end)
        })
    {
        return Err("unexpected semantic timeline order");
    }
    if failed != 1 || !(1..=1 + maximum_discovery_calls).contains(&calls) {
        return Err("expected one failed create and only measured discovery");
    }
    Ok((calls, failed))
}

#[test]
fn failure_parity_keeps_failed_call_inside_coalesced_discovery_batch() {
    let status = attention_status();
    let mut timeline = failure_timeline(2, 1);
    assert_eq!(failure_shape(&status, &timeline, 0, 0, 1), Ok((2, 1)));
    timeline[3] = StepKind::ToolsCompleted {
        calls: 1,
        failed: 0,
        failures: Default::default(),
    };
    timeline.insert(
        4,
        StepKind::ToolsCompleted {
            calls: 1,
            failed: 1,
            failures: Default::default(),
        },
    );
    assert_eq!(failure_shape(&status, &timeline, 0, 0, 1), Ok((2, 1)));
}

#[test]
fn failure_parity_refuses_success_retries_artifacts_and_terminal_substitutes() {
    let status = attention_status();
    for (calls, failed) in [(1, 0), (2, 2), (3, 1), (0, 1)] {
        assert!(failure_shape(&status, &failure_timeline(calls, failed), 0, 0, 1).is_err());
    }
    let timeline = failure_timeline(1, 1);
    let mut reordered = timeline.clone();
    reordered.swap(3, 4);
    assert!(failure_shape(&status, &reordered, 0, 0, 1).is_err());
    for other in [
        RunStatus::running(),
        RunStatus::Completed,
        RunStatus::Cancelled,
    ] {
        assert!(failure_shape(&other, &timeline, 0, 0, 1).is_err());
    }
    for (artifacts, versions) in [(1, 0), (0, 1)] {
        assert!(failure_shape(&status, &timeline, artifacts, versions, 1).is_err());
    }
    for extra in [
        StepKind::TurnStarted {
            originator: TurnOriginator::Orchestrator,
        },
        StepKind::TurnCompleted {
            originator: TurnOriginator::Orchestrator,
            artifacts_registered: 0,
        },
        StepKind::ArtifactRegistered,
        StepKind::Completed,
    ] {
        let mut repeated = timeline.clone();
        repeated.push(extra);
        assert!(failure_shape(&status, &repeated, 0, 0, 1).is_err());
    }
}

fn attention_status() -> RunStatus {
    let mut status = RunStatus::running();
    if let RunStatus::Running { attention, .. } = &mut status {
        *attention = koru_delegate_core::AttentionSet::new([Attention::FinishedNeedingAttention]);
    }
    status
}

fn failure_timeline(calls: u32, failed: u32) -> Vec<StepKind> {
    vec![
        StepKind::RunCreated,
        StepKind::Started,
        StepKind::TurnStarted {
            originator: TurnOriginator::Orchestrator,
        },
        StepKind::ToolsCompleted {
            calls,
            failed,
            failures: Default::default(),
        },
        StepKind::TurnCompleted {
            originator: TurnOriginator::Orchestrator,
            artifacts_registered: 0,
        },
    ]
}

#[test]
fn failure_turn_binding_refuses_missing_duplicate_and_foreign_references() {
    use koru_delegate_core::{AgentTurnId, TimelineRefs};
    let turn = AgentTurnId::mint();
    let entry = |kind, turns: Vec<_>| {
        TimelineEntry::new(
            kind,
            TimelineRefs::new(turns.into_iter().map(|turn| TimelineRef::Turn { turn })).unwrap(),
            0,
        )
    };
    let start = entry(
        StepKind::TurnStarted {
            originator: TurnOriginator::Orchestrator,
        },
        vec![turn],
    );
    let tool_kind = StepKind::ToolsCompleted {
        calls: 1,
        failed: 1,
        failures: Default::default(),
    };
    let tools = entry(tool_kind, vec![turn]);
    let end = entry(
        StepKind::TurnCompleted {
            originator: TurnOriginator::Orchestrator,
            artifacts_registered: 0,
        },
        vec![turn],
    );
    assert!(bind_original_turn(&[&start, &tools, &end]).is_ok());
    for turns in [vec![], vec![turn, turn], vec![AgentTurnId::mint()]] {
        let bad_tools = entry(tool_kind, turns.clone());
        assert!(bind_original_turn(&[&start, &bad_tools, &end]).is_err());
        let bad_end = entry(end.kind, turns);
        assert!(bind_original_turn(&[&start, &tools, &bad_end]).is_err());
    }
}

#[test]
fn endpoint_proof_requires_exact_warning_fields_for_the_isolated_run() {
    let run = RunId::mint();
    let line = format!(
        "2026-09-10T00:00:00Z WARN run tool refused tool=create run={run:?} code=file_not_found elapsed_ms=1"
    );
    assert_eq!(endpoint_failure_counts(&line, run), (1, 0));
    assert_eq!(
        endpoint_failure_counts(&format!("{line}\n{line}"), run),
        (2, 0)
    );
    assert_eq!(endpoint_failure_counts(&line, RunId::mint()), (0, 0));
    for wrong in [
        line.replace("WARN", "INFO"),
        line.replace("run tool refused", "provider said run tool refused"),
        line.replace(&format!("run={run:?}"), &format!("run={run:?}-suffix")),
    ] {
        assert_eq!(endpoint_failure_counts(&wrong, run), (0, 0));
    }
    for wrong in [
        line.replace("tool=create", "tool=search"),
        line.replace("file_not_found", "invalid_arguments"),
        format!("{line} code=file_not_found"),
    ] {
        assert_eq!(endpoint_failure_counts(&wrong, run), (0, 1));
    }
}
