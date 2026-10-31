// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use koru_session_protocol::ErrorCode;
use serde_json::Value;

use super::{
    JsonLineReader, ProviderDiagnostic, ProviderFailure, ProviderFrame, ProviderStage,
    ProviderWake, write_json_line,
};

const INBOUND_FRAMES: usize = 256;
const SHUTDOWN_POLL: Duration = Duration::from_millis(10);

pub(super) trait JsonRpcTransport: Send {
    fn send(&mut self, message: &Value) -> Result<(), ProviderFailure>;
    fn receive(&mut self, timeout: Duration) -> Result<TransportReceive, ProviderFailure>;
    fn shutdown(&mut self, timeout: Duration) -> Result<(), ProviderFailure>;
    /// Close the input and end the owned process tree at once, for a provider
    /// that never exits on EOF. A transport with no process of its own has
    /// nothing to terminate, so the default is its ordinary shutdown.
    fn terminate_tree(&mut self) -> Result<(), ProviderFailure> {
        self.shutdown(Duration::ZERO)
    }
}

pub(super) enum TransportReceive {
    Frame(Value),
    Woken,
    Timeout,
}

struct InboundState {
    frames: VecDeque<Result<ProviderFrame, ProviderFailure>>,
    closed: bool,
    woken: bool,
}

struct InboundShared {
    state: Mutex<InboundState>,
    ready: Condvar,
    room: Condvar,
    capacity: usize,
}

/// Independent wake bit beside the bounded provider-frame queue. Local
/// commands therefore wake a blocked adapter even when every frame slot is in
/// use, and repeated wakes coalesce without allocating or carrying payloads.
pub(super) struct TransportWake {
    shared: Arc<InboundShared>,
}

impl TransportWake {
    pub(super) fn provider_default() -> Arc<Self> {
        Self::new(INBOUND_FRAMES)
    }

    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(InboundShared {
                state: Mutex::new(InboundState {
                    frames: VecDeque::with_capacity(capacity),
                    closed: false,
                    woken: false,
                }),
                ready: Condvar::new(),
                room: Condvar::new(),
                capacity,
            }),
        })
    }

    fn receive(&self, timeout: Duration) -> Result<TransportReceive, ProviderFailure> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        loop {
            if state.woken {
                state.woken = false;
                return Ok(TransportReceive::Woken);
            }
            if let Some(frame) = state.frames.pop_front() {
                self.shared.room.notify_one();
                return frame.map(|frame| TransportReceive::Frame(frame.into_value()));
            }
            if state.closed {
                return Err(ProviderFailure::new(
                    ErrorCode::ProviderFailed,
                    ProviderStage::Read,
                    ProviderDiagnostic::UnexpectedExit,
                ));
            }
            let Some(deadline) = deadline else {
                state = self
                    .shared
                    .ready
                    .wait(state)
                    .unwrap_or_else(|held| held.into_inner());
                continue;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(TransportReceive::Timeout);
            }
            let (next, waited) = self
                .shared
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(|held| held.into_inner());
            state = next;
            if waited.timed_out() && !state.woken && state.frames.is_empty() && !state.closed {
                return Ok(TransportReceive::Timeout);
            }
        }
    }

    fn push(&self, frame: Result<ProviderFrame, ProviderFailure>) -> bool {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        while state.frames.len() >= self.shared.capacity && !state.closed {
            state = self
                .shared
                .room
                .wait(state)
                .unwrap_or_else(|held| held.into_inner());
        }
        if state.closed {
            return false;
        }
        state.frames.push_back(frame);
        drop(state);
        self.shared.ready.notify_one();
        true
    }

    fn close(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        state.closed = true;
        drop(state);
        self.shared.ready.notify_all();
        self.shared.room.notify_all();
    }
}

impl ProviderWake for TransportWake {
    fn wake(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        state.woken = true;
        drop(state);
        self.shared.ready.notify_one();
    }
}

pub(super) struct StdioJsonRpc {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    inbound: Arc<TransportWake>,
    reader: Option<JoinHandle<()>>,
    process_tree: ProcessTree,
    /// The bound on what this process writes; reads have their own.
    maximum_outbound_frame_bytes: usize,
    shutting_down: bool,
    /// A directory only this tree writes into, removed once the tree is gone.
    scratch: Option<std::path::PathBuf>,
}

impl StdioJsonRpc {
    /// Hand this owner a directory that dies with the process tree — a
    /// provider's private temp folder, which a terminated process cannot
    /// clean up itself. It is removed after every termination path.
    pub(super) fn own_scratch(&mut self, directory: std::path::PathBuf) {
        self.scratch = Some(directory);
    }

    /// The tree has just been terminated, so its files may still be closing:
    /// retry briefly, then leave the folder rather than block a shutdown.
    fn release_scratch(&mut self) {
        let Some(directory) = self.scratch.take() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::remove_dir_all(&directory) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if Instant::now() < deadline => std::thread::sleep(SHUTDOWN_POLL),
                Err(error) => {
                    tracing::warn!(target: "koru::provider", event = "provider.scratch.left", error = %error.kind());
                    return;
                }
            }
        }
    }

    /// `argv` is the adapter's rendered launch: the subcommand and every
    /// element after it, already split, never a shell string. This layer
    /// adds nothing and reparses nothing.
    ///
    /// `env` is what the child gets that this process does not — a
    /// credential an adapter refuses to put on argv, where every process on
    /// the machine could read it. It is set on the child's block alone:
    /// this process's own environment is never touched, so two sessions can
    /// start at once with two different values.
    pub(super) fn spawn(
        executable: &OsString,
        argv: &[OsString],
        env: &[(OsString, OsString)],
        cwd: &Path,
        maximum_inbound_frame_bytes: usize,
        maximum_outbound_frame_bytes: usize,
        inbound: Arc<TransportWake>,
    ) -> Result<Self, ProviderFailure> {
        let process_tree = ProcessTree::new()?;
        let mut command = Command::new(executable);
        command
            .args(argv)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Provider stderr is not a protocol channel and may contain user
            // content. The adapter exposes only closed diagnostics.
            .stderr(Stdio::null());
        process_tree.prepare(&mut command);
        let mut child = command.spawn().map_err(|error| spawn_failure(&error))?;
        if let Err(failure) = process_tree.assign(&child) {
            // Assignment failed, so the tree owner cannot be trusted to reach
            // this primary process yet.
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure);
        }
        let Some(stdin) = child.stdin.take() else {
            terminate_and_wait(&process_tree, &mut child);
            return Err(spawn_pipe_failure());
        };
        let Some(stdout) = child.stdout.take() else {
            terminate_and_wait(&process_tree, &mut child);
            return Err(spawn_pipe_failure());
        };
        debug_assert_eq!(inbound.shared.capacity, INBOUND_FRAMES);
        let reader_inbound = Arc::clone(&inbound);
        let reader = match std::thread::Builder::new()
            .name("koru-codex-jsonl-reader".into())
            .stack_size(256 * 1024)
            .spawn(move || read_frames(stdout, maximum_inbound_frame_bytes, reader_inbound))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_and_wait(&process_tree, &mut child);
                return Err(ProviderFailure::io(ProviderStage::Spawn, &error));
            }
        };
        Ok(Self {
            child,
            stdin: Some(BufWriter::new(stdin)),
            inbound,
            reader: Some(reader),
            process_tree,
            maximum_outbound_frame_bytes,
            shutting_down: false,
            scratch: None,
        })
    }

    fn terminate(&mut self) -> Result<(), ProviderFailure> {
        let terminated = self.process_tree.terminate(&mut self.child);
        let waited = self
            .child
            .wait()
            .map(|_| ())
            .map_err(|error| ProviderFailure::io(ProviderStage::Shutdown, &error));
        self.release_scratch();
        terminated.and(waited)
    }
}

impl JsonRpcTransport for StdioJsonRpc {
    fn send(&mut self, message: &Value) -> Result<(), ProviderFailure> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::ProviderFailed,
                ProviderStage::Write,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        write_json_line(stdin, message, self.maximum_outbound_frame_bytes)?;
        std::io::Write::flush(stdin)
            .map_err(|error| ProviderFailure::io(ProviderStage::Write, &error))
    }

    fn receive(&mut self, timeout: Duration) -> Result<TransportReceive, ProviderFailure> {
        self.inbound.receive(timeout)
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<(), ProviderFailure> {
        if self.shutting_down {
            return Ok(());
        }
        self.shutting_down = true;
        self.stdin.take();
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    // The primary may exit before a tool it started. Reap the
                    // complete process group/job even on an orderly EOF.
                    self.terminate()?;
                    // Never let an inherited stdout handle turn cleanup into
                    // an unbounded reader join. Closing the receiver during
                    // drop wakes a reader blocked on delivery.
                    self.reader.take();
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(ProviderFailure::new(
                            ErrorCode::ProviderFailed,
                            ProviderStage::Shutdown,
                            ProviderDiagnostic::UnexpectedExit,
                        ))
                    };
                }
                Ok(None) if Instant::now() < deadline => std::thread::sleep(SHUTDOWN_POLL),
                Ok(None) => {
                    let _ = self.terminate();
                    return Err(ProviderFailure::new(
                        ErrorCode::ProviderFailed,
                        ProviderStage::Shutdown,
                        ProviderDiagnostic::Timeout,
                    ));
                }
                Err(error) => {
                    let _ = self.terminate();
                    return Err(ProviderFailure::io(ProviderStage::Shutdown, &error));
                }
            }
        }
    }

    fn terminate_tree(&mut self) -> Result<(), ProviderFailure> {
        if self.shutting_down {
            return Ok(());
        }
        self.shutting_down = true;
        self.stdin.take();
        let terminated = self.terminate();
        // As in `shutdown`: never join a reader an inherited handle may hold.
        self.reader.take();
        terminated
    }
}

impl Drop for StdioJsonRpc {
    fn drop(&mut self) {
        self.stdin.take();
        self.inbound.close();
        let _ = self.terminate();
    }
}

fn read_frames(
    stdout: std::process::ChildStdout,
    maximum_frame_bytes: usize,
    inbound: Arc<TransportWake>,
) {
    let Ok(mut reader) = JsonLineReader::new(BufReader::new(stdout), maximum_frame_bytes) else {
        return;
    };
    loop {
        match reader.next_frame() {
            Ok(Some(frame)) => {
                if !inbound.push(Ok(frame)) {
                    break;
                }
            }
            Ok(None) => break,
            Err(failure) => {
                let _ = inbound.push(Err(failure));
                break;
            }
        }
    }
    inbound.close();
}

/// Whether a process with this id is running. **Unsure answers "running"**:
/// the caller deletes what a gone process left, so a wrong "gone" would cost
/// a live session its files, and a wrong "running" only leaves a folder.
#[cfg(windows)]
pub(super) fn process_exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query-only handle, closed before returning.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            // No such process; anything else (access denied) is somebody's.
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code = 0;
        let queried = GetExitCodeProcess(process, &mut code) != 0;
        CloseHandle(process);
        !queried || code == STILL_ACTIVE as u32
    }
}

#[cfg(unix)]
pub(super) fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only checks the id.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(any(windows, unix)))]
pub(super) fn process_exists(_pid: u32) -> bool {
    true
}

fn spawn_failure(error: &std::io::Error) -> ProviderFailure {
    ProviderFailure {
        code: if error.kind() == std::io::ErrorKind::NotFound {
            ErrorCode::ProviderUnavailable
        } else {
            ErrorCode::ProviderFailed
        },
        stage: ProviderStage::Spawn,
        diagnostic: if error.kind() == std::io::ErrorKind::NotFound {
            ProviderDiagnostic::MissingExecutable
        } else {
            ProviderDiagnostic::Spawn
        },
        os_code: error.raw_os_error(),
    }
}

fn spawn_pipe_failure() -> ProviderFailure {
    ProviderFailure::new(
        ErrorCode::ProviderFailed,
        ProviderStage::Spawn,
        ProviderDiagnostic::Spawn,
    )
}

fn terminate_and_wait(process_tree: &ProcessTree, child: &mut Child) {
    let _ = process_tree.terminate(child);
    let _ = child.wait();
}

#[cfg(unix)]
pub(super) struct ProcessTree;

#[cfg(unix)]
impl ProcessTree {
    pub(super) fn new() -> Result<Self, ProviderFailure> {
        Ok(Self)
    }

    pub(super) fn prepare(&self, command: &mut Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    pub(super) fn assign(&self, _child: &Child) -> Result<(), ProviderFailure> {
        Ok(())
    }

    pub(super) fn terminate(&self, child: &mut Child) -> Result<(), ProviderFailure> {
        let result = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(ProviderFailure::io(
                ProviderStage::Shutdown,
                &std::io::Error::last_os_error(),
            ))
        }
    }
}

#[cfg(windows)]
pub(super) struct ProcessTree(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for ProcessTree {}

#[cfg(windows)]
impl ProcessTree {
    pub(super) fn new() -> Result<Self, ProviderFailure> {
        use std::mem::{size_of, zeroed};
        use std::ptr::null;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(ProviderFailure::io(
                ProviderStage::Spawn,
                &std::io::Error::last_os_error(),
            ));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(ProviderFailure::io(
                ProviderStage::Spawn,
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Self(handle))
    }

    pub(super) fn prepare(&self, command: &mut Command) {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    pub(super) fn assign(&self, child: &Child) -> Result<(), ProviderFailure> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) } == 0 {
            return Err(ProviderFailure::io(
                ProviderStage::Spawn,
                &std::io::Error::last_os_error(),
            ));
        }
        resume_primary_thread(child.id())
    }

    pub(super) fn terminate(&self, _child: &mut Child) -> Result<(), ProviderFailure> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        if unsafe { TerminateJobObject(self.0, 1) } == 0 {
            Err(ProviderFailure::io(
                ProviderStage::Shutdown,
                &std::io::Error::last_os_error(),
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
fn resume_primary_thread(process_id: u32) -> Result<(), ProviderFailure> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(ProviderFailure::io(
            ProviderStage::Spawn,
            &std::io::Error::last_os_error(),
        ));
    }
    let mut entry: THREADENTRY32 = unsafe { zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut thread_id = None;
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while has_entry {
        if entry.th32OwnerProcessID == process_id {
            thread_id = Some(entry.th32ThreadID);
            break;
        }
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    let Some(thread_id) = thread_id else {
        return Err(ProviderFailure::new(
            ErrorCode::ProviderFailed,
            ProviderStage::Spawn,
            ProviderDiagnostic::Spawn,
        ));
    };
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(ProviderFailure::io(
            ProviderStage::Spawn,
            &std::io::Error::last_os_error(),
        ));
    }
    let resumed = unsafe { ResumeThread(thread) };
    unsafe { CloseHandle(thread) };
    if resumed == u32::MAX {
        Err(ProviderFailure::io(
            ProviderStage::Spawn,
            &std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests;
