// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::providers::{DelegationFacts, acp_probe_failure};
use koru_session_protocol::CapabilitySet;

struct FixtureProbe {
    raw_available: bool,
    result: Result<AgentCapabilities, ProviderFailure>,
}

impl CompatibilityProbe for FixtureProbe {
    fn provider(&self) -> ProviderKind {
        "fixture".parse().unwrap()
    }

    fn raw_available(&self) -> bool {
        self.raw_available
    }

    fn probe(&self, _: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure> {
        self.result.clone()
    }
}

fn discover(
    raw_available: bool,
    result: Result<AgentCapabilities, ProviderFailure>,
) -> ProviderDiscovery {
    DiscoveryRun::spawn(
        vec![Box::new(FixtureProbe {
            raw_available,
            result,
        })],
        ProbeLimits::default(),
    )
    .unwrap()
    .recv_timeout(Duration::from_secs(5))
    .unwrap()
    .expect("fixture probe completes")
}

#[test]
fn every_discovery_state_preserves_independent_raw_presence() {
    let capabilities = AgentCapabilities {
        provider: "fixture".parse().unwrap(),
        executable_version: "1.0".parse().unwrap(),
        protocol_version: "1".parse().unwrap(),
        capabilities: CapabilitySet::default(),
        delegation: DelegationFacts::undeclared(),
    };
    let kimi: serde_json::Value = serde_json::from_str(include_str!(
        "../acp_failure/fixtures/kimi-auth-required.json"
    ))
    .unwrap();
    let wrapper: serde_json::Value = serde_json::from_str(include_str!(
        "../acp_failure/fixtures/claude-runtime-missing.json"
    ))
    .unwrap();
    let cases = [
        (
            Ok(capabilities.clone()),
            ManagedAvailability::Compatible(capabilities),
        ),
        (
            Err(ProviderFailure::new(
                ErrorCode::ProviderUnavailable,
                ProviderStage::Spawn,
                ProviderDiagnostic::MissingExecutable,
            )),
            ManagedAvailability::Missing {
                code: ErrorCode::ProviderUnavailable,
            },
        ),
        (
            Err(acp_probe_failure(&kimi["error"], false)),
            ManagedAvailability::SignedOut {
                code: ErrorCode::ProviderUnavailable,
            },
        ),
        (
            Err(ProviderFailure::new(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            )),
            ManagedAvailability::Incompatible {
                code: ErrorCode::ProviderIncompatible,
                found: None,
                needed: None,
            },
        ),
        (
            Err(acp_probe_failure(&wrapper["error"], true)),
            ManagedAvailability::Failed {
                code: ErrorCode::ProviderFailed,
                stage: ProviderStage::Initialize,
                diagnostic: ProviderDiagnostic::RuntimeUnavailable,
            },
        ),
    ];
    for (result, expected) in cases {
        for raw in [false, true] {
            let discovered = discover(raw, result.clone());
            assert_eq!(discovered.raw_available, raw);
            assert_eq!(discovered.managed, expected);
            assert_eq!(discovered.managed.enabled(), result.is_ok());
        }
    }
}

#[test]
fn timeout_diagnostic_wins_over_the_broad_code_and_preserves_its_stage() {
    for stage in [
        ProviderStage::Discovery,
        ProviderStage::Version,
        ProviderStage::Initialize,
    ] {
        for code in [
            ErrorCode::ProviderUnavailable,
            ErrorCode::ProviderIncompatible,
        ] {
            let discovered = discover(
                true,
                Err(ProviderFailure::new(
                    code.clone(),
                    stage,
                    ProviderDiagnostic::Timeout,
                )),
            );
            assert_eq!(
                discovered.managed,
                ManagedAvailability::Failed {
                    code,
                    stage,
                    diagnostic: ProviderDiagnostic::Timeout,
                }
            );
        }
    }
}

#[test]
fn unknown_unavailable_refusal_is_failed_never_missing_or_signed_out() {
    let discovered = discover(
        true,
        Err(ProviderFailure::new(
            ErrorCode::ProviderUnavailable,
            ProviderStage::Initialize,
            ProviderDiagnostic::HandshakeRejected,
        )),
    );
    assert_eq!(
        discovered.managed,
        ManagedAvailability::Failed {
            code: ErrorCode::ProviderUnavailable,
            stage: ProviderStage::Initialize,
            diagnostic: ProviderDiagnostic::HandshakeRejected,
        }
    );
}

#[test]
fn probe_environment_is_retained_only_for_the_child_and_never_debug() {
    let command = ProbeCommand::new("<private-executable>")
        .arg("<private-arg>")
        .env("<private-key>", "<private-value>");
    assert_eq!(
        command.environment,
        vec![("<private-key>".into(), "<private-value>".into())]
    );
    assert!(!format!("{command:?}").contains("private"));
}

#[test]
fn unusable_wrapper_override_is_failed_independently_of_raw_claude_presence() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../acp_failure/fixtures/claude-override-missing.json"
    ))
    .unwrap();
    for raw in [false, true] {
        let discovered = discover(raw, Err(acp_probe_failure(&fixture["error"], true)));
        assert_eq!(discovered.raw_available, raw);
        assert!(matches!(
            discovered.managed,
            ManagedAvailability::Failed {
                diagnostic: ProviderDiagnostic::RuntimeUnavailable,
                ..
            }
        ));
        assert!(!discovered.managed.enabled());
    }
}

#[test]
fn probe_terminates_descendants_holding_output_pipes_on_exit_and_timeout() {
    for timeout in [false, true] {
        #[cfg(windows)]
        let command = {
            // Cargo's DLL search PATH can exceed cmd.exe's 8191-character
            // limit. Keep the fixture independent of that process environment.
            let system =
                std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
            let script = if timeout {
                "start /b ping.exe -n 30 127.0.0.1 & ping.exe -n 30 127.0.0.1"
            } else {
                // The child's first reply proves it holds the inherited pipe.
                "start /b ping.exe -n 30 127.0.0.1 & ping.exe -n 2 127.0.0.1 >nul & exit /b 0"
            };
            ProbeCommand::new(system.join("cmd.exe"))
                .env("PATH", system)
                .arg("/d")
                .arg("/s")
                .arg("/c")
                .arg(script)
        };
        #[cfg(unix)]
        let command = ProbeCommand::new("sh").arg("-c").arg(if timeout {
            "sleep 30 & sleep 30"
        } else {
            "sleep 30 & exit 0"
        });
        let started = Instant::now();
        let result = run_probe_command(
            command,
            ProbeLimits {
                timeout: if timeout {
                    Duration::from_millis(300)
                } else {
                    Duration::from_secs(3)
                },
                maximum_output_bytes: 1024,
            },
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        if timeout {
            assert_eq!(result.unwrap_err().diagnostic, ProviderDiagnostic::Timeout);
        } else {
            let output = result.unwrap();
            assert!(output.success());
            assert_eq!(output.stderr_bytes(), 0, "fixture child failed to launch");
            #[cfg(windows)]
            assert!(
                output
                    .stdout()
                    .windows(b"127.0.0.1".len())
                    .any(|part| part == b"127.0.0.1"),
                "descendant must write before the parent exits"
            );
        }
    }
}

/// **A `--version` run that fails or prints nothing is the program not
/// starting**, never a refused version: Kimi's CLI under a too-old Node.js
/// exits 1 with nothing on stdout, and readiness must call that *Failed*.
/// A print that is there is the route's to read.
#[test]
fn a_version_command_that_fails_or_prints_nothing_did_not_start() {
    for (success, stdout) in [
        (false, &b""[..]),
        (false, b"1.0.0\n"),
        (true, b""),
        (true, b" \r\n"),
    ] {
        let failure = version_print(success, stdout).unwrap_err();
        assert_eq!(failure.code, ErrorCode::ProviderFailed);
        assert_eq!(failure.stage, ProviderStage::Version);
        assert_eq!(failure.diagnostic, ProviderDiagnostic::UnexpectedExit);
        assert_eq!(
            classify_failure(failure),
            ManagedAvailability::Failed {
                code: ErrorCode::ProviderFailed,
                stage: ProviderStage::Version,
                diagnostic: ProviderDiagnostic::UnexpectedExit,
            }
        );
    }
    assert_eq!(
        version_print(true, b"1.18.25\n").unwrap(),
        Some("1.18.25\n")
    );
    assert_eq!(version_print(true, &[0xff, 0xfe]).unwrap(), None);
}

/// **A refused version reaches the wire with both facts**: discovery keeps
/// what the version gate found and needed, and readiness serializes them.
#[test]
fn a_refused_version_carries_found_and_needed_to_readiness() {
    use koru_session_protocol::{ExecutableAvailability, VersionNeed};
    use std::str::FromStr;
    let refusal = ProviderFailure::new(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Version,
        ProviderDiagnostic::VersionRejected,
    )
    .refusing(
        Some(ProviderVersion::from_str("1.18.25").unwrap()),
        Some(VersionNeed::AtLeast("1.18.27".into())),
    );
    let discovered = discover(true, Err(refusal));
    assert_eq!(
        discovered.managed,
        ManagedAvailability::Incompatible {
            code: ErrorCode::ProviderIncompatible,
            found: Some(ProviderVersion::from_str("1.18.25").unwrap()),
            needed: Some(VersionNeed::AtLeast("1.18.27".into())),
        }
    );
    let row = discovered.managed.readiness("opencode".parse().unwrap());
    assert_eq!(
        row.availability,
        ExecutableAvailability::Incompatible {
            code: ErrorCode::ProviderIncompatible,
            found: Some("1.18.25".into()),
            needed: Some(VersionNeed::AtLeast("1.18.27".into())),
        }
    );
    assert_eq!(row.failure, None);
    // The refused version stays out of a failure's `Debug`, as it was.
    let logged = format!(
        "{:?}",
        ProviderFailure::new(
            ErrorCode::ProviderIncompatible,
            ProviderStage::Version,
            ProviderDiagnostic::VersionRejected,
        )
        .refusing(Some(ProviderVersion::from_str("1.18.25").unwrap()), None)
    );
    assert!(!logged.contains("1.18.25"), "{logged}");
}
