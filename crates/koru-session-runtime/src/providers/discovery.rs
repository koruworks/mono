// SPDX-License-Identifier: MPL-2.0

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use koru_session_protocol::{AgentModes, ErrorCode, ProviderKind, ProviderModels, VersionNeed};

use super::{
    AgentCapabilities, ProviderDiagnostic, ProviderFailure, ProviderStage, ProviderVersion,
};

/// Production factories resolve through the same PATH/PATHEXT candidate
/// selection as managed CreateProfile. This belongs in factory construction,
/// off dispatch; fixture factories never inspect the test machine.
pub fn resolve_probe_executable(executable: &OsStr) -> Result<OsString, ProviderFailure> {
    executable
        .to_str()
        .and_then(koru_session_profiles::resolve_executable)
        .map(std::path::PathBuf::into_os_string)
        .ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::ProviderUnavailable,
                ProviderStage::Discovery,
                ProviderDiagnostic::MissingExecutable,
            )
        })
}

pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_PROBE_OUTPUT_BYTES: usize = 256 * 1024;
const PROBE_POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeLimits {
    pub timeout: Duration,
    pub maximum_output_bytes: usize,
}

impl Default for ProbeLimits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_PROBE_TIMEOUT,
            maximum_output_bytes: DEFAULT_PROBE_OUTPUT_BYTES,
        }
    }
}

impl ProbeLimits {
    pub fn validate(self) -> Result<Self, ProviderFailure> {
        if self.timeout.is_zero() || self.maximum_output_bytes == 0 {
            return Err(ProviderFailure::new(
                ErrorCode::InvalidRequest,
                ProviderStage::Discovery,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        Ok(self)
    }
}

/// Result of structured discovery. Raw availability is deliberately separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderDiscovery {
    pub provider: ProviderKind,
    pub raw_available: bool,
    pub managed: ManagedAvailability,
    /// The model list the probe's session start received, for the catalog.
    pub models: Option<ProviderModels>,
    /// The approval modes the same start received (koru-ai D7), for the
    /// catalog; `None` where the agent advertised none.
    pub modes: Option<AgentModes>,
}

/// **What a probe's session start received** for the host's catalog: its
/// model list when the catalog comes from the probe, and the agent's
/// approval modes whatever the catalog's source (koru-ai D7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeCapture {
    pub models: Option<ProviderModels>,
    pub modes: Option<AgentModes>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedAvailability {
    Compatible(AgentCapabilities),
    Missing {
        code: ErrorCode,
    },
    SignedOut {
        code: ErrorCode,
    },
    /// Refused by the route. `found` and `needed` are the version refusal's
    /// facts when the refusal was the version, for the window to name.
    Incompatible {
        code: ErrorCode,
        found: Option<ProviderVersion>,
        needed: Option<VersionNeed>,
    },
    Failed {
        code: ErrorCode,
        stage: ProviderStage,
        diagnostic: ProviderDiagnostic,
    },
}

impl ManagedAvailability {
    pub fn readiness(&self, kind: ProviderKind) -> koru_session_protocol::ProviderReadiness {
        use koru_session_protocol::{
            ExecutableAvailability as A, ProviderReadiness, ProviderReadinessFailure,
        };
        let (availability, capabilities, failure) = match self {
            Self::Compatible(value) => {
                let mut capabilities = koru_session_protocol::CapabilitySet::new(
                    value
                        .capabilities
                        .iter()
                        .filter(|token| {
                            token.as_str() != koru_session_protocol::capability::MANAGED_DELEGATION
                        })
                        .cloned(),
                );
                let verdict =
                    super::DelegationVerdict::evaluate(&value.capabilities, &value.delegation);
                if verdict.delegatable() {
                    capabilities = koru_session_protocol::CapabilitySet::new(
                        capabilities.iter().cloned().chain([
                            koru_session_protocol::capability::MANAGED_DELEGATION
                                .parse()
                                .expect("built-in capability"),
                        ]),
                    );
                }
                (
                    A::Available {
                        version: Some(value.executable_version.as_str().into()),
                    },
                    capabilities,
                    None,
                )
            }
            Self::Missing { .. } => (A::Missing, Default::default(), None),
            Self::SignedOut { code } => (
                A::SignedOut { code: code.clone() },
                Default::default(),
                None,
            ),
            Self::Incompatible {
                code,
                found,
                needed,
            } => (
                A::Incompatible {
                    code: code.clone(),
                    found: found.as_ref().map(|version| version.as_str().to_owned()),
                    needed: needed.clone(),
                },
                Default::default(),
                None,
            ),
            Self::Failed {
                code,
                stage,
                diagnostic,
            } => (
                A::Failed { code: code.clone() },
                Default::default(),
                Some(ProviderReadinessFailure {
                    stage: *stage,
                    diagnostic: *diagnostic,
                }),
            ),
        };
        ProviderReadiness {
            kind,
            availability,
            capabilities,
            failure,
        }
    }

    pub fn enabled(&self) -> bool {
        matches!(self, Self::Compatible(_))
    }
}

/// One provider-specific version plus initialize/capability handshake.
pub trait CompatibilityProbe: Send + 'static {
    fn provider(&self) -> ProviderKind;
    fn raw_available(&self) -> bool;
    fn probe(&self, limits: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure>;

    /// The probe, and the model list its session start received when the
    /// provider's catalog comes from the probe (`CatalogSource::Probe`).
    /// Keeping it costs nothing: the start already happened.
    fn probe_with_models(
        &self,
        limits: ProbeLimits,
    ) -> Result<(AgentCapabilities, Option<ProviderModels>), ProviderFailure> {
        self.probe(limits).map(|capabilities| (capabilities, None))
    }

    /// The probe, its model list as [`probe_with_models`](Self::probe_with_models)
    /// keeps it, and the approval modes its start received (D7).
    fn probe_with_capture(
        &self,
        limits: ProbeLimits,
    ) -> Result<(AgentCapabilities, ProbeCapture), ProviderFailure> {
        self.probe_with_models(limits)
            .map(|(capabilities, models)| {
                (
                    capabilities,
                    ProbeCapture {
                        models,
                        modes: None,
                    },
                )
            })
    }
}

/// A discovery run starts bounded provider probes on named background threads.
/// A surface may poll `try_next`; it never executes or waits for a provider.
pub struct DiscoveryRun {
    receiver: Receiver<ProviderDiscovery>,
    workers: Vec<JoinHandle<()>>,
}

impl DiscoveryRun {
    pub fn spawn(
        probes: Vec<Box<dyn CompatibilityProbe>>,
        limits: ProbeLimits,
    ) -> Result<Self, ProviderFailure> {
        let limits = limits.validate()?;
        let (sender, receiver) = mpsc::channel();
        let mut workers = Vec::with_capacity(probes.len());
        for probe in probes {
            let provider = probe.provider();
            let sender = sender.clone();
            let thread_name = format!("koru-provider-probe-{provider}");
            let worker = std::thread::Builder::new()
                .name(thread_name)
                .stack_size(256 * 1024)
                .spawn(move || {
                    let raw_available =
                        catch_unwind(AssertUnwindSafe(|| probe.raw_available())).unwrap_or(false);
                    let mut capture = ProbeCapture::default();
                    let managed = match catch_unwind(AssertUnwindSafe(|| {
                        probe.probe_with_capture(limits)
                    })) {
                        Ok(Ok((capabilities, captured))) if capabilities.provider == provider => {
                            capture = captured;
                            ManagedAvailability::Compatible(capabilities)
                        }
                        Ok(Ok(_)) => ManagedAvailability::Incompatible {
                            code: ErrorCode::ProviderIncompatible,
                            found: None,
                            needed: None,
                        },
                        Ok(Err(failure)) => classify_failure(failure),
                        Err(_) => ManagedAvailability::Failed {
                            code: ErrorCode::ProviderFailed,
                            stage: ProviderStage::Discovery,
                            diagnostic: ProviderDiagnostic::UnexpectedExit,
                        },
                    };
                    let _ = sender.send(ProviderDiscovery {
                        provider,
                        raw_available,
                        managed,
                        models: capture.models,
                        modes: capture.modes,
                    });
                })
                .map_err(|_| {
                    ProviderFailure::new(
                        ErrorCode::ProviderFailed,
                        ProviderStage::Discovery,
                        ProviderDiagnostic::Spawn,
                    )
                })?;
            workers.push(worker);
        }
        drop(sender);
        Ok(Self { receiver, workers })
    }

    pub fn try_next(&self) -> Result<Option<ProviderDiscovery>, ProviderFailure> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Ok(None),
        }
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<ProviderDiscovery>, ProviderFailure> {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => Ok(Some(result)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => self.try_next(),
        }
    }

    /// Wait only on a background owner. A timed-out caller can publish a
    /// failure first while retaining its concurrency permit until these actual
    /// probe workers have stopped.
    pub(crate) fn wait_until_finished(self) {
        for worker in self.workers {
            let _ = worker.join();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.workers.iter().all(JoinHandle::is_finished)
    }
}

pub(crate) fn classify_failure(failure: ProviderFailure) -> ManagedAvailability {
    // Codes alone cannot distinguish missing executables from other refusals.
    match failure.diagnostic {
        ProviderDiagnostic::MissingExecutable => {
            return ManagedAvailability::Missing { code: failure.code };
        }
        ProviderDiagnostic::AuthenticationRequired => {
            return ManagedAvailability::SignedOut { code: failure.code };
        }
        ProviderDiagnostic::Timeout | ProviderDiagnostic::RuntimeUnavailable => {
            return failed_availability(failure);
        }
        _ => {}
    }
    match failure.code {
        ErrorCode::ProfileMissing => ManagedAvailability::Missing { code: failure.code },
        ErrorCode::ProviderIncompatible
        | ErrorCode::ProfileIncompatible
        | ErrorCode::ProtocolIncompatible
        | ErrorCode::CapabilityNotNegotiated => {
            let (found, needed) = failure
                .version
                .map_or((None, None), |refusal| (refusal.found, refusal.needed));
            ManagedAvailability::Incompatible {
                code: failure.code,
                found,
                needed,
            }
        }
        _ => failed_availability(failure),
    }
}

/// **What a `--version` run printed, or the failure that it said nothing.**
///
/// A run that exits non-zero, or prints nothing at all, never got as far as
/// naming a version: the agent's program does not start on this computer.
/// Kimi's CLI under a Node.js too old for it is one — it exits 1 with a
/// `SyntaxError` on stderr and nothing on stdout. That is `UnexpectedExit`
/// at the version stage with `ProviderFailed`, which readiness reads as
/// *Failed*: calling it an unsupported version sent people to update an
/// agent whose version was never the problem. Output that is there but is
/// not text is `Ok(None)`: the program ran and printed something no route
/// can read, which stays the route's refusal.
pub(crate) fn version_print(success: bool, stdout: &[u8]) -> Result<Option<&str>, ProviderFailure> {
    let did_not_start = || {
        ProviderFailure::new(
            ErrorCode::ProviderFailed,
            ProviderStage::Version,
            ProviderDiagnostic::UnexpectedExit,
        )
    };
    if !success || stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(did_not_start());
    }
    Ok(std::str::from_utf8(stdout).ok())
}

fn failed_availability(failure: ProviderFailure) -> ManagedAvailability {
    ManagedAvailability::Failed {
        code: failure.code,
        stage: failure.stage,
        diagnostic: failure.diagnostic,
    }
}

/// A command description used only by a provider probe. Its `Debug` output
/// never exposes the executable, arguments, stdin, or environment.
pub struct ProbeCommand {
    program: OsString,
    arguments: Vec<OsString>,
    stdin: Vec<u8>,
    environment: Vec<(OsString, OsString)>,
    working_directory: Option<std::path::PathBuf>,
}

impl ProbeCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            arguments: Vec::new(),
            stdin: Vec::new(),
            environment: Vec::new(),
            working_directory: None,
        }
    }

    pub fn arg(mut self, argument: impl AsRef<OsStr>) -> Self {
        self.arguments.push(argument.as_ref().to_owned());
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.environment
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    pub fn current_dir(mut self, path: std::path::PathBuf) -> Self {
        self.working_directory = Some(path);
        self
    }

    pub fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = bytes;
        self
    }
}

impl fmt::Debug for ProbeCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeCommand")
            .field("argument_count", &self.arguments.len())
            .field("stdin_bytes", &self.stdin.len())
            .finish()
    }
}

/// Private probe bytes. Provider-specific code must parse and normalize them
/// before anything crosses its adapter or reaches diagnostics.
pub struct ProbeOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr_bytes: usize,
}

impl ProbeOutput {
    pub fn success(&self) -> bool {
        self.success
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr_bytes(&self) -> usize {
        self.stderr_bytes
    }
}

impl fmt::Debug for ProbeOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeOutput")
            .field("success", &self.success)
            .field("exit_code", &self.exit_code)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr_bytes)
            .finish()
    }
}

/// Runs one probe command with bounded retained output and a hard deadline.
/// This function is blocking and is intended only inside `CompatibilityProbe`.
pub fn run_probe_command(
    command: ProbeCommand,
    limits: ProbeLimits,
) -> Result<ProbeOutput, ProviderFailure> {
    let limits = limits.validate()?;
    if command.stdin.len() > limits.maximum_output_bytes {
        return Err(ProviderFailure::new(
            ErrorCode::InputTooLarge,
            ProviderStage::Write,
            ProviderDiagnostic::InvalidInput,
        ));
    }
    let mut process = Command::new(&command.program);
    process
        .args(&command.arguments)
        .envs(command.environment.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &command.working_directory {
        process.current_dir(cwd);
    }
    let process_tree = super::process::ProcessTree::new()?;
    process_tree.prepare(&mut process);
    let mut child = process.spawn().map_err(|error| {
        let diagnostic = if error.kind() == std::io::ErrorKind::NotFound {
            ProviderDiagnostic::MissingExecutable
        } else {
            ProviderDiagnostic::Spawn
        };
        ProviderFailure {
            code: if error.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::ProviderUnavailable
            } else {
                ErrorCode::ProviderFailed
            },
            stage: ProviderStage::Spawn,
            diagnostic,
            os_code: error.raw_os_error(),
            version: None,
        }
    })?;

    if let Err(error) = process_tree.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    if let Some(mut stdin) = child.stdin.take()
        && let Err(error) = stdin.write_all(&command.stdin)
    {
        let _ = process_tree.terminate(&mut child);
        let _ = child.wait();
        return Err(ProviderFailure::io(ProviderStage::Write, &error));
    }
    let stdout = child.stdout.take().expect("piped provider stdout");
    let stderr = child.stderr.take().expect("piped provider stderr");
    let output_limit = limits.maximum_output_bytes;
    let stdout_worker = std::thread::spawn(move || read_bounded(stdout, output_limit));
    let stderr_worker = std::thread::spawn(move || read_bounded(stderr, output_limit));

    let deadline = Instant::now() + limits.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(PROBE_POLL),
            Ok(None) => {
                let _ = process_tree.terminate(&mut child);
                let _ = child.wait();
                let _ = stdout_worker.join();
                let _ = stderr_worker.join();
                return Err(ProviderFailure::new(
                    ErrorCode::ProviderUnavailable,
                    ProviderStage::Discovery,
                    ProviderDiagnostic::Timeout,
                ));
            }
            Err(error) => {
                let _ = process_tree.terminate(&mut child);
                let _ = stdout_worker.join();
                let _ = stderr_worker.join();
                return Err(ProviderFailure::io(ProviderStage::Discovery, &error));
            }
        }
    };

    // A configured plugin cannot keep inherited output pipes alive after the probe exits.
    let _ = process_tree.terminate(&mut child);
    let stdout = join_output(stdout_worker)?;
    let stderr = join_output(stderr_worker)?;
    if stdout.overflowed || stderr.overflowed {
        return Err(ProviderFailure::new(
            ErrorCode::ProviderIncompatible,
            ProviderStage::Read,
            ProviderDiagnostic::FrameTooLarge,
        ));
    }
    Ok(ProbeOutput {
        success: status.success(),
        exit_code: status.code(),
        stdout: stdout.bytes,
        stderr_bytes: stderr.bytes.len(),
    })
}

struct BoundedOutput {
    bytes: Vec<u8>,
    overflowed: bool,
}

fn read_bounded(mut input: impl Read, limit: usize) -> std::io::Result<BoundedOutput> {
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut overflowed = false;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        overflowed |= read > remaining;
    }
    Ok(BoundedOutput {
        bytes: retained,
        overflowed,
    })
}

fn join_output(
    worker: JoinHandle<std::io::Result<BoundedOutput>>,
) -> Result<BoundedOutput, ProviderFailure> {
    worker
        .join()
        .map_err(|_| {
            ProviderFailure::new(
                ErrorCode::ProviderFailed,
                ProviderStage::Read,
                ProviderDiagnostic::UnexpectedExit,
            )
        })?
        .map_err(|error| ProviderFailure::io(ProviderStage::Read, &error))
}

#[cfg(test)]
mod tests;
