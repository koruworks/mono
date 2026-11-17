// SPDX-License-Identifier: MPL-2.0

//! Starting the browser: the argument list, the profile folder rule, the
//! process tree, the launch lock and the stop handle.
//!
//! **Nothing here has been run against a browser.** The argument list, the
//! way the pipe is handed over and what the window does on launch are each
//! "verify at P7.1" (assumptions 1, 2, 4, 5, 12, 22).
//!
//! What a launch is:
//!
//! 1. two anonymous pipes, the parent's ends never inheritable;
//! 2. a kill-on-close job;
//! 3. the process created suspended, with an explicit handle list that names
//!    the two child ends and nothing else;
//! 4. the process assigned to the job, then resumed. A failure to assign
//!    kills it.
//!
//! The two child ends are inheritable from step 1 to step 3. Every launch
//! holds [`launch_lock`] for that time, and so must any other code in the
//! same process that creates a child.
//!
//! The code that creates a process is compiled only on Windows, outside
//! tests, and without the `no-browser-process` feature ([`CAN_LAUNCH`]).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::installed::is_rooted;

/// Whether this build holds any code that can start a browser.
pub const CAN_LAUNCH: bool = cfg!(all(windows, not(test), not(feature = "no-browser-process")));

/// How long an orderly close may take before the process tree is ended.
pub const ORDERLY_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// The folder, under the root a caller passes, that holds every profile.
pub const PROFILES_FOLDER: &str = "browser";

/// The arguments that never change, in the order they are passed. The whole
/// list is these, the profile folder and the two pipe handles. Nothing a
/// model or a page supplies reaches it.
pub const FIXED_ARGUMENTS: [&str; 5] = [
    PIPE_ARGUMENT,
    "--no-first-run",
    "--no-default-browser-check",
    "--new-window",
    "about:blank",
];

const PROFILE_ARGUMENT: &str = "--user-data-dir=";
const PIPE_ARGUMENT: &str = "--remote-debugging-pipe";
const PIPE_HANDLES_ARGUMENT: &str = "--remote-debugging-io-pipes=";
/// Step A2b of the qualification only: a port the browser chooses.
const PORT_ARGUMENT: &str = "--remote-debugging-port=0";

/// What must never be in the argument list, as the text a flag would hold:
/// no hidden window, no weakened sandbox, no weakened web security, no
/// ignored certificate errors, no extension, nothing that hides automation
/// from a site, and no listening port.
pub const FORBIDDEN_ARGUMENT_TEXT: [&str; 14] = [
    "headless",
    "no-sandbox",
    "disable-sandbox",
    "disable-web-security",
    "ignore-certificate-errors",
    "allow-running-insecure-content",
    "allow-insecure-localhost",
    "load-extension",
    "disable-extensions-except",
    "disable-blink-features",
    "enable-automation",
    "user-agent",
    "remote-debugging-port",
    "remote-allow-origins",
];

/// The two pipe ends the browser is given, as the handle values written on
/// its command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeHandleValues {
    /// The end the browser reads commands from.
    pub browser_reads: u32,
    /// The end the browser writes replies and events to.
    pub browser_writes: u32,
}

/// How Koru attaches to the browser it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// The pipe. The route this crate is built for.
    Pipe,
    /// A loopback port the browser chooses and writes into its profile
    /// folder. **Koru does not use it.** It exists so that step A2b of the
    /// qualification can be run if the pipe fails; any program on the
    /// computer can attach to a browser started this way.
    LoopbackPort,
}

/// The argument list for [`Attachment::LoopbackPort`]: the pipe's list with
/// its two pipe arguments replaced by one.
pub fn loopback_port_arguments(profile: &Path) -> Vec<OsString> {
    let mut list = arguments(
        profile,
        PipeHandleValues {
            browser_reads: 0,
            browser_writes: 0,
        },
    );
    list.splice(1..3, [OsString::from(PORT_ARGUMENT)]);
    list
}

/// The argument list after the executable, for the pipe.
pub fn arguments(profile: &Path, pipes: PipeHandleValues) -> Vec<OsString> {
    let mut folder = OsString::from(PROFILE_ARGUMENT);
    folder.push(profile);
    let [pipe, first_run, default_check, window, page] = FIXED_ARGUMENTS;
    vec![
        folder,
        pipe.into(),
        format!(
            "{PIPE_HANDLES_ARGUMENT}{},{}",
            pipes.browser_reads, pipes.browser_writes
        )
        .into(),
        first_run.into(),
        default_check.into(),
        window.into(),
        page.into(),
    ]
}

/// How the first window is shown. Which of these keeps focus where the
/// person has it is what step A3 of the qualification records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowShow {
    /// Shown, and asked not to become the active window.
    NoActivate,
    /// Whatever the browser does by itself.
    BrowserDefault,
}

/// Where a launch stopped. A token and at most an operating-system code:
/// never a path, an argument or a handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchStage {
    /// This build cannot start a process ([`CAN_LAUNCH`]).
    Unavailable,
    /// The executable or the profile path is not a full, plain path.
    Plan,
    /// The profile folder could not be made, or resolves elsewhere.
    Folder,
    Pipe,
    ProcessTree,
    Spawn,
    Assign,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchFailure {
    pub stage: LaunchStage,
    pub code: Option<i32>,
}

impl LaunchFailure {
    pub const fn at(stage: LaunchStage) -> Self {
        Self { stage, code: None }
    }
}

/// One launch: which executable, on which profile folder.
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    executable: PathBuf,
    profile: PathBuf,
    show: WindowShow,
    attachment: Attachment,
}

impl LaunchPlan {
    /// Both paths must be full paths with no quote and no control character.
    /// The profile is the folder [`prepare_profile_folder`] answered.
    pub fn new(executable: &Path, profile: &Path) -> Result<Self, LaunchFailure> {
        for path in [executable, profile] {
            let text = path.to_str().ok_or(LaunchFailure::at(LaunchStage::Plan))?;
            if !is_rooted(text) || text.chars().any(|c| c == '"' || c.is_control()) {
                return Err(LaunchFailure::at(LaunchStage::Plan));
            }
        }
        Ok(Self {
            executable: executable.to_path_buf(),
            profile: profile.to_path_buf(),
            show: WindowShow::NoActivate,
            attachment: Attachment::Pipe,
        })
    }

    /// Step A2b of the qualification only.
    pub fn attaching(mut self, attachment: Attachment) -> Self {
        self.attachment = attachment;
        self
    }

    pub fn attachment(&self) -> Attachment {
        self.attachment
    }

    pub fn showing(mut self, show: WindowShow) -> Self {
        self.show = show;
        self
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn profile(&self) -> &Path {
        &self.profile
    }

    pub fn show(&self) -> WindowShow {
        self.show
    }
}

impl fmt::Debug for LaunchPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LaunchPlan({:?}, {:?})", self.show, self.attachment)
    }
}

// ------------------------------------------------------------ command line

/// One argument in the units a command line is made of.
fn units(argument: &OsStr) -> Vec<u16> {
    #[cfg(windows)]
    {
        std::os::windows::ffi::OsStrExt::encode_wide(argument).collect()
    }
    #[cfg(not(windows))]
    {
        argument.to_string_lossy().encode_utf16().collect()
    }
}

/// The command line a process is created with: the executable and each
/// argument quoted by the rules the receiving program splits it by, so an
/// argument's boundaries never move. Not terminated.
pub fn command_line(executable: &Path, arguments: &[OsString]) -> Vec<u16> {
    let mut line = Vec::new();
    quote_argument(&mut line, &units(executable.as_os_str()));
    for argument in arguments {
        line.push(u16::from(b' '));
        quote_argument(&mut line, &units(argument));
    }
    line
}

fn quote_argument(output: &mut Vec<u16>, argument: &[u16]) {
    let plain = !argument.is_empty()
        && !argument
            .iter()
            .any(|unit| matches!(*unit, 0x20 | 0x09 | 0x22));
    if plain {
        output.extend_from_slice(argument);
        return;
    }
    let slash = u16::from(b'\\');
    let quote = u16::from(b'"');
    output.push(quote);
    let mut slashes = 0;
    for &unit in argument {
        if unit == slash {
            slashes += 1;
            continue;
        }
        // Backslashes before a quote are doubled and the quote escaped;
        // anywhere else they are themselves.
        let run = if unit == quote {
            slashes * 2 + 1
        } else {
            slashes
        };
        output.extend(std::iter::repeat_n(slash, run));
        slashes = 0;
        output.push(unit);
    }
    output.extend(std::iter::repeat_n(slash, slashes * 2));
    output.push(quote);
}

// ---------------------------------------------------------- profile folder

/// A profile folder's name: 1 to 64 bytes of lower-case letters, digits and
/// hyphens, starting with a letter or a digit. A profile id in its stored
/// form is one. Nothing else can be a path component here.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProfileFolderName(String);

impl ProfileFolderName {
    pub fn new(name: &str) -> Option<Self> {
        let valid = (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-');
        valid.then(|| Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProfileFolderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProfileFolderName(..)")
    }
}

/// The folder operations the rule needs, so the rule is tested without a
/// disk.
pub trait FolderSystem {
    /// The path with every link resolved.
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf>;
    fn create_dir(&self, path: &Path) -> std::io::Result<()>;
    fn is_dir(&self, path: &Path) -> std::io::Result<bool>;
}

/// The real disk.
pub struct DiskFolders;

impl FolderSystem for DiskFolders {
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        std::fs::create_dir(path)
    }

    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        std::fs::metadata(path).map(|found| found.is_dir())
    }
}

/// Exactly `<root>/browser/<name>`, made level by level under the canonical
/// root. A level that is a link resolving elsewhere, or is not a folder,
/// refuses. An existing folder is used as it is: a profile outlives its
/// window. Nothing is ever removed here.
///
/// Answers the folder in its canonical form, which on Windows is the
/// verbatim spelling. [`plain_path`] gives the spelling to pass on.
pub fn prepare_profile_folder(
    fs: &dyn FolderSystem,
    root: &Path,
    name: &ProfileFolderName,
) -> Result<PathBuf, LaunchFailure> {
    let refuse = |_| LaunchFailure::at(LaunchStage::Folder);
    let root = fs.canonical(root).map_err(refuse)?;
    let mut level = root;
    for part in [PROFILES_FOLDER, name.as_str()] {
        level = level.join(part);
        match fs.create_dir(&level) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(refuse(error)),
        }
        if fs.canonical(&level).map_err(refuse)? != level || !fs.is_dir(&level).map_err(refuse)? {
            return Err(LaunchFailure::at(LaunchStage::Folder));
        }
    }
    Ok(level)
}

/// A canonical Windows path without its verbatim prefix, which a program's
/// own argument parsing may not accept. Any other path is unchanged.
pub fn plain_path(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

// ------------------------------------------------------------- launch lock

static LAUNCH_LOCK: Mutex<()> = Mutex::new(());

/// The one process-wide lock held while inheritable handles exist. **Any
/// code in the same process that creates a child must hold it too**, or that
/// child can inherit the browser's pipe ends.
pub fn launch_lock() -> MutexGuard<'static, ()> {
    LAUNCH_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn under_launch_lock<T>(create: impl FnOnce() -> T) -> T {
    let _held = launch_lock();
    create()
}

// ------------------------------------------------------------ process tree

/// One browser's whole process tree. Dropping the last holder ends it.
pub trait ProcessTree: Send + Sync {
    /// End every process in the tree now. Safe to repeat.
    fn end(&self);
    /// Wait until no process of the tree is left, at most `limit`. `true`
    /// when none is left.
    fn wait_empty(&self, limit: Duration) -> bool;
    /// How many processes of the tree are running, when it can be told.
    fn active_processes(&self) -> Option<u32>;
    /// Their process ids, when they can be told.
    fn process_ids(&self) -> Vec<u32>;
    /// The most memory the tree has committed at once, in bytes, when the
    /// system kept the number.
    fn peak_memory_bytes(&self) -> Option<u64>;
}

type OrderlyClose = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct StopState {
    cancelled: bool,
    closing: bool,
    closed: bool,
    orderly: Option<OrderlyClose>,
}

struct StopShared {
    tree: Arc<dyn ProcessTree>,
    grace: Duration,
    state: Mutex<StopState>,
    wake: Condvar,
}

impl StopShared {
    fn state(&self) -> MutexGuard<'_, StopState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish(&self, orderly: Option<OrderlyClose>) {
        if let Some(ask) = orderly {
            ask();
            self.tree.wait_empty(self.grace);
        }
        self.tree.end();
        self.state().closed = true;
        self.wake.notify_all();
    }
}

/// The handle that stops a browser from any thread, without waiting and
/// without reaching whoever is using it.
#[derive(Clone)]
pub struct BrowserStop {
    shared: Arc<StopShared>,
}

impl BrowserStop {
    pub fn new(tree: Arc<dyn ProcessTree>, grace: Duration) -> Self {
        Self {
            shared: Arc::new(StopShared {
                tree,
                grace,
                state: Mutex::new(StopState::default()),
                wake: Condvar::new(),
            }),
        }
    }

    /// How the browser is asked to close by itself: one message on its pipe,
    /// set by whoever owns the pipe's write end. Without it a close ends the
    /// tree at once.
    pub fn set_orderly_close(&self, ask: impl FnOnce() + Send + 'static) {
        let mut state = self.shared.state();
        if !state.closing {
            state.orderly = Some(Box::new(ask));
        }
    }

    /// Never waits. A call in flight sees it at its next check.
    pub fn cancel(&self) {
        self.shared.state().cancelled = true;
        self.shared.wake.notify_all();
    }

    pub fn cancelled(&self) -> bool {
        self.shared.state().cancelled
    }

    /// For a wait loop: sleep at most `limit`, less if a cancel arrives.
    /// `true` when cancelled.
    pub fn wait_cancelled(&self, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        let mut state = self.shared.state();
        while !state.cancelled {
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                break;
            };
            if left.is_zero() {
                break;
            }
            state = self
                .shared
                .wake
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.cancelled
    }

    /// Never waits. Cancels, asks the browser to close, and ends the tree
    /// when the grace is over or no process is left, on a thread of its own.
    /// Only the first call does anything.
    pub fn close(&self) {
        let orderly = {
            let mut state = self.shared.state();
            state.cancelled = true;
            if state.closing {
                return;
            }
            state.closing = true;
            state.orderly.take()
        };
        self.shared.wake.notify_all();
        let Some(ask) = orderly else {
            // Nothing to ask: ending a job does not block.
            self.shared.finish(None);
            return;
        };
        // The ask travels in a slot, so that it can still be dropped unrun
        // if the thread cannot be made.
        let slot = Arc::new(Mutex::new(Some(ask)));
        let taken = slot.clone();
        let shared = self.shared.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-close".into())
            .spawn(move || {
                let ask = taken.lock().unwrap_or_else(PoisonError::into_inner).take();
                shared.finish(ask);
            });
        if spawned.is_err() {
            drop(slot.lock().unwrap_or_else(PoisonError::into_inner).take());
            self.shared.finish(None);
        }
    }

    /// Whether the tree has been ended by a close.
    pub fn closed(&self) -> bool {
        self.shared.state().closed
    }

    /// Wait for a close to finish, at most `limit`. `true` when it has.
    pub fn wait_closed(&self, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        let mut state = self.shared.state();
        while !state.closed {
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                break;
            };
            if left.is_zero() {
                break;
            }
            state = self
                .shared
                .wake
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.closed
    }
}

impl fmt::Debug for BrowserStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.state();
        f.debug_struct("BrowserStop")
            .field("cancelled", &state.cancelled)
            .field("closing", &state.closing)
            .field("closed", &state.closed)
            .finish()
    }
}

/// The parent's two pipe ends.
pub struct PipeEnds {
    /// Replies and events arrive here.
    pub from_browser: File,
    /// Commands are written here.
    pub to_browser: File,
}

/// A started browser: its pipe ends, its tree and its stop handle.
pub struct LaunchedProcess {
    pub process_id: u32,
    /// `None` only for [`Attachment::LoopbackPort`].
    pub pipe: Option<PipeEnds>,
    pub tree: Arc<dyn ProcessTree>,
    pub stop: BrowserStop,
}

impl fmt::Debug for LaunchedProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchedProcess")
            .field("process_id", &self.process_id)
            .finish_non_exhaustive()
    }
}

/// Start the browser. Holds the launch lock for the whole creation.
///
/// In a build that cannot start a process ([`CAN_LAUNCH`] is `false`) this
/// answers `Unavailable` and there is no other outcome.
pub fn launch(plan: &LaunchPlan) -> Result<LaunchedProcess, LaunchFailure> {
    under_launch_lock(|| create(plan))
}

#[cfg(not(all(windows, not(test), not(feature = "no-browser-process"))))]
fn create(_plan: &LaunchPlan) -> Result<LaunchedProcess, LaunchFailure> {
    Err(LaunchFailure::at(LaunchStage::Unavailable))
}

#[cfg(all(windows, not(test), not(feature = "no-browser-process")))]
fn create(plan: &LaunchPlan) -> Result<LaunchedProcess, LaunchFailure> {
    process::create(plan)
}

// Everything that can start a process is in this one module, and this is
// its only gate.
#[cfg(all(windows, not(test), not(feature = "no-browser-process")))]
mod process {
    use std::ffi::c_void;
    use std::fs::File;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::ptr::{null, null_mut};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
        JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESHOWWINDOW,
        STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::{
        Attachment, BrowserStop, LaunchFailure, LaunchPlan, LaunchStage, LaunchedProcess,
        ORDERLY_CLOSE_GRACE, PipeEnds, PipeHandleValues, ProcessTree, WindowShow, arguments,
        command_line, loopback_port_arguments,
    };

    /// Shown in its most recent size and place, and not made active.
    const SHOW_NO_ACTIVATE: u16 = 4;
    /// The most process ids one listing asks for.
    const LISTED_PROCESSES: usize = 512;

    fn failed(stage: LaunchStage) -> LaunchFailure {
        LaunchFailure {
            stage,
            code: std::io::Error::last_os_error().raw_os_error(),
        }
    }

    struct OwnedHandle(HANDLE);

    // A kernel handle is usable from any thread.
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}

    impl OwnedHandle {
        fn raw(&self) -> HANDLE {
            self.0
        }

        fn inheritable(&self) -> Result<(), LaunchFailure> {
            let set =
                unsafe { SetHandleInformation(self.0, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            if set == 0 {
                Err(failed(LaunchStage::Pipe))
            } else {
                Ok(())
            }
        }

        /// The value a command line carries. A kernel handle fits 32 bits.
        fn value(&self) -> Result<u32, LaunchFailure> {
            u32::try_from(self.0 as usize).map_err(|_| LaunchFailure::at(LaunchStage::Pipe))
        }

        fn into_file(self) -> File {
            let handle = self.0;
            std::mem::forget(self);
            unsafe { File::from_raw_handle(handle as RawHandle) }
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// One anonymous pipe as (read end, write end), neither inheritable.
    fn pipe() -> Result<(OwnedHandle, OwnedHandle), LaunchFailure> {
        let mut read = null_mut();
        let mut write = null_mut();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 0,
        };
        if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
            Err(failed(LaunchStage::Pipe))
        } else {
            Ok((OwnedHandle(read), OwnedHandle(write)))
        }
    }

    fn kill_on_close_job() -> Result<OwnedHandle, LaunchFailure> {
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(failed(LaunchStage::ProcessTree));
        }
        let job = OwnedHandle(handle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(failed(LaunchStage::ProcessTree));
        }
        Ok(job)
    }

    /// The attribute list that names the only handles the child inherits.
    /// It points into `handles`, so it owns them for as long as it lives.
    struct HandleList {
        storage: Vec<usize>,
        handles: Box<[HANDLE; 2]>,
    }

    impl HandleList {
        fn new(handles: [HANDLE; 2]) -> Result<Self, LaunchFailure> {
            let handles = Box::new(handles);
            let mut bytes = 0usize;
            unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
            if bytes == 0 {
                return Err(failed(LaunchStage::Spawn));
            }
            let mut list = Self {
                storage: vec![0; bytes.div_ceil(size_of::<usize>())],
                handles: handles,
            };
            if unsafe { InitializeProcThreadAttributeList(list.pointer(), 1, 0, &mut bytes) } == 0 {
                // Never initialized: nothing for `Drop` to delete.
                let failure = failed(LaunchStage::Spawn);
                std::mem::forget(list);
                return Err(failure);
            }
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    list.pointer(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    list.handles.as_ptr().cast::<c_void>(),
                    size_of::<[HANDLE; 2]>(),
                    null_mut(),
                    null(),
                )
            };
            if updated == 0 {
                return Err(failed(LaunchStage::Spawn));
            }
            Ok(list)
        }

        fn pointer(&mut self) -> *mut c_void {
            self.storage.as_mut_ptr().cast()
        }
    }

    impl Drop for HandleList {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.pointer()) };
        }
    }

    struct JobTree {
        job: OwnedHandle,
        process: OwnedHandle,
    }

    impl JobTree {
        fn accounting(&self) -> Option<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            let read = unsafe {
                QueryInformationJobObject(
                    self.job.raw(),
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    null_mut(),
                )
            };
            (read != 0).then_some(info)
        }
    }

    impl ProcessTree for JobTree {
        fn end(&self) {
            unsafe { TerminateJobObject(self.job.raw(), 1) };
        }

        fn wait_empty(&self, limit: Duration) -> bool {
            let until = Instant::now() + limit;
            loop {
                match self.active_processes() {
                    Some(0) => return true,
                    Some(_) => {}
                    // The job cannot be asked: the first process stands in.
                    None => {
                        let left = until.saturating_duration_since(Instant::now());
                        let waited = unsafe {
                            WaitForSingleObject(self.process.raw(), left.as_millis() as u32)
                        };
                        return waited == WAIT_OBJECT_0;
                    }
                }
                if Instant::now() >= until {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn active_processes(&self) -> Option<u32> {
            self.accounting().map(|info| info.ActiveProcesses)
        }

        fn process_ids(&self) -> Vec<u32> {
            // Two counts, then the ids. Words keep the block aligned.
            let header = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() - size_of::<usize>();
            let words = header.div_ceil(size_of::<usize>()) + LISTED_PROCESSES;
            let mut block = vec![0usize; words];
            let read = unsafe {
                QueryInformationJobObject(
                    self.job.raw(),
                    JobObjectBasicProcessIdList,
                    block.as_mut_ptr().cast(),
                    (words * size_of::<usize>()) as u32,
                    null_mut(),
                )
            };
            if read == 0 {
                return Vec::new();
            }
            let list = block.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            let count =
                (unsafe { (*list).NumberOfProcessIdsInList } as usize).min(LISTED_PROCESSES);
            let first = unsafe { (&raw const (*list).ProcessIdList).cast::<usize>() };
            (0..count)
                .map(|index| unsafe { first.add(index).read() } as u32)
                .collect()
        }

        fn peak_memory_bytes(&self) -> Option<u64> {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            let read = unsafe {
                QueryInformationJobObject(
                    self.job.raw(),
                    JobObjectExtendedLimitInformation,
                    (&raw mut info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    null_mut(),
                )
            };
            (read != 0 && info.PeakJobMemoryUsed != 0).then_some(info.PeakJobMemoryUsed as u64)
        }
    }

    /// The two pipes of one launch: the child's ends and the parent's.
    struct Pipes {
        browser_reads: OwnedHandle,
        browser_writes: OwnedHandle,
        from_browser: OwnedHandle,
        to_browser: OwnedHandle,
    }

    pub(super) fn create(plan: &LaunchPlan) -> Result<LaunchedProcess, LaunchFailure> {
        let pipes = match plan.attachment() {
            Attachment::Pipe => {
                // Commands: the parent writes, the browser reads. Replies:
                // the browser writes, the parent reads.
                let (browser_reads, to_browser) = pipe()?;
                let (from_browser, browser_writes) = pipe()?;
                Some(Pipes {
                    browser_reads,
                    browser_writes,
                    from_browser,
                    to_browser,
                })
            }
            Attachment::LoopbackPort => None,
        };
        let job = kill_on_close_job()?;
        let arguments = match &pipes {
            Some(pipes) => arguments(
                plan.profile(),
                PipeHandleValues {
                    browser_reads: pipes.browser_reads.value()?,
                    browser_writes: pipes.browser_writes.value()?,
                },
            ),
            None => loopback_port_arguments(plan.profile()),
        };
        let application: Vec<u16> = plan
            .executable()
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut line = command_line(plan.executable(), &arguments);
        line.push(0);

        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        if plan.show() == WindowShow::NoActivate {
            startup.StartupInfo.dwFlags = STARTF_USESHOWWINDOW;
            startup.StartupInfo.wShowWindow = SHOW_NO_ACTIVATE;
        }
        // From here until the child's ends are closed below, two inheritable
        // handles exist. The caller holds the launch lock. The handle list
        // names them and nothing else; with no pipe nothing is inherited.
        let mut inherited = match &pipes {
            Some(pipes) => {
                pipes.browser_reads.inheritable()?;
                pipes.browser_writes.inheritable()?;
                Some(HandleList::new([
                    pipes.browser_reads.raw(),
                    pipes.browser_writes.raw(),
                ])?)
            }
            None => None,
        };
        let mut flags = CREATE_SUSPENDED;
        if let Some(list) = inherited.as_mut() {
            startup.lpAttributeList = list.pointer();
            flags |= EXTENDED_STARTUPINFO_PRESENT;
        }
        let mut created: PROCESS_INFORMATION = unsafe { zeroed() };
        let started = unsafe {
            CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                i32::from(inherited.is_some()),
                flags,
                null(),
                null(),
                &startup.StartupInfo,
                &mut created,
            )
        };
        let failure = (started == 0).then(|| failed(LaunchStage::Spawn));
        drop(inherited);
        // The child's ends are the child's now: close the parent's copies.
        let pipe = pipes.map(|pipes| {
            drop(pipes.browser_reads);
            drop(pipes.browser_writes);
            (pipes.from_browser, pipes.to_browser)
        });
        if let Some(failure) = failure {
            return Err(failure);
        }
        let process = OwnedHandle(created.hProcess);
        let thread = OwnedHandle(created.hThread);

        if unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } == 0 {
            let failure = failed(LaunchStage::Assign);
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(failure);
        }
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let failure = failed(LaunchStage::Resume);
            unsafe { TerminateJobObject(job.raw(), 1) };
            return Err(failure);
        }
        drop(thread);

        let tree: Arc<dyn ProcessTree> = Arc::new(JobTree { job, process });
        Ok(LaunchedProcess {
            process_id: created.dwProcessId,
            pipe: pipe.map(|(from_browser, to_browser)| PipeEnds {
                from_browser: from_browser.into_file(),
                to_browser: to_browser.into_file(),
            }),
            stop: BrowserStop::new(tree.clone(), ORDERLY_CLOSE_GRACE),
            tree,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    const PIPES: PipeHandleValues = PipeHandleValues {
        browser_reads: 640,
        browser_writes: 644,
    };

    fn text(arguments: &[OsString]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn m7_launch_args_are_the_fixed_list_plus_one_path() {
        let profile = Path::new(r"C:\data\ai\browser\0f3a");
        let arguments = text(&arguments(profile, PIPES));
        assert_eq!(
            arguments,
            [
                r"--user-data-dir=C:\data\ai\browser\0f3a",
                "--remote-debugging-pipe",
                "--remote-debugging-io-pipes=640,644",
                "--no-first-run",
                "--no-default-browser-check",
                "--new-window",
                "about:blank",
            ]
        );
        // Take away the path and the two handle numbers Koru minted, and
        // what is left is the fixed list, in order.
        let fixed: Vec<&str> = arguments
            .iter()
            .map(String::as_str)
            .filter(|argument| {
                !argument.starts_with(PROFILE_ARGUMENT)
                    && !argument.starts_with(PIPE_HANDLES_ARGUMENT)
            })
            .collect();
        assert_eq!(fixed, FIXED_ARGUMENTS);
        // Another profile changes one argument and nothing else.
        let other = text(&super::arguments(Path::new(r"D:\p"), PIPES));
        let changed: Vec<usize> = (0..arguments.len())
            .filter(|index| arguments[*index] != other[*index])
            .collect();
        assert_eq!(changed, [0]);
        // One window on the empty page: the only argument that is not a flag.
        let pages: Vec<&String> = arguments.iter().filter(|a| !a.starts_with("--")).collect();
        assert_eq!(pages, ["about:blank"]);
    }

    #[test]
    fn m7_launch_args_hold_no_forbidden_flag() {
        let arguments = text(&arguments(Path::new(r"C:\data\ai\browser\0f3a"), PIPES));
        for argument in &arguments[1..] {
            for forbidden in FORBIDDEN_ARGUMENT_TEXT {
                assert!(!argument.contains(forbidden), "{argument} / {forbidden}");
            }
        }
        // The transport is the pipe: no port is opened.
        assert!(!arguments.iter().any(|a| a.contains("port")));
        // The one other list exists for the qualification's step A2b. It
        // differs in the attachment and in nothing else, and a plan never
        // chooses it by itself.
        let profile = Path::new(r"C:\data\ai\browser\0f3a");
        let port = text(&loopback_port_arguments(profile));
        assert_eq!(port[0], arguments[0]);
        assert_eq!(port[1], "--remote-debugging-port=0");
        assert_eq!(port[2..], arguments[3..]);
        let plan = LaunchPlan::new(Path::new(r"C:\Apps\b.exe"), profile).unwrap();
        assert_eq!(plan.attachment(), Attachment::Pipe);
        assert_eq!(
            plan.attaching(Attachment::LoopbackPort).attachment(),
            Attachment::LoopbackPort
        );
        // A profile path cannot smuggle a second argument: it is quoted as
        // one, whatever it holds.
        let odd = Path::new(r"C:\a b\x --headless\");
        let line = command_line(
            Path::new(r"C:\Apps\browser.exe"),
            &super::arguments(odd, PIPES),
        );
        let line = String::from_utf16(&line).unwrap();
        assert!(
            line.starts_with(
                r#"C:\Apps\browser.exe "--user-data-dir=C:\a b\x --headless\\" --remote-debugging-pipe "#
            ),
            "{line}"
        );
    }

    #[test]
    fn m7_launch_args_quoting_keeps_argument_boundaries() {
        let arguments: Vec<OsString> = ["plain", "two words", "quoted\"value", "trailing\\", ""]
            .into_iter()
            .map(OsString::from)
            .collect();
        let line = command_line(Path::new(r"C:\Program Files\App\tool.exe"), &arguments);
        assert_eq!(
            String::from_utf16(&line).unwrap(),
            "\"C:\\Program Files\\App\\tool.exe\" plain \"two words\" \"quoted\\\"value\" trailing\\ \"\""
        );
        let slashes: Vec<OsString> = vec![OsString::from("a\\\\\"b c\\")];
        let line = command_line(Path::new("t.exe"), &slashes);
        assert_eq!(
            String::from_utf16(&line).unwrap(),
            "t.exe \"a\\\\\\\\\\\"b c\\\\\""
        );
    }

    #[test]
    fn m7_launch_args_a_plan_takes_full_plain_paths_only() {
        let executable = Path::new(r"C:\Apps\browser.exe");
        let profile = Path::new(r"C:\data\ai\browser\0f3a");
        let plan = LaunchPlan::new(executable, profile).unwrap();
        assert_eq!(plan.show(), WindowShow::NoActivate);
        assert_eq!(
            plan.clone().showing(WindowShow::BrowserDefault).show(),
            WindowShow::BrowserDefault
        );
        for (executable, profile) in [
            ("browser.exe", r"C:\p"),
            (r"C:\Apps\browser.exe", "p"),
            (r"C:\Apps\browser.exe", r"..\p"),
            (r"C:\Apps\browser.exe", "C:\\p\"x"),
            (r"C:\Apps\browser.exe", "C:\\p\nx"),
            ("", r"C:\p"),
        ] {
            assert_eq!(
                LaunchPlan::new(Path::new(executable), Path::new(profile)).err(),
                Some(LaunchFailure::at(LaunchStage::Plan)),
                "{executable:?} {profile:?}"
            );
        }
        // Neither path is shown.
        let shown = format!("{plan:?}");
        assert!(
            !shown.contains("Apps") && !shown.contains("0f3a"),
            "{shown}"
        );
    }

    #[test]
    fn m7_launch_args_the_launch_lock_is_taken() {
        // Held for the whole creation.
        let held = under_launch_lock(|| LAUNCH_LOCK.try_lock().is_err());
        assert!(held);
        // And `launch` creates nothing outside it.
        let source = include_str!("launch.rs");
        let body = source
            .split_once("pub fn launch(plan: &LaunchPlan)")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .unwrap();
        assert!(
            body.contains("under_launch_lock(|| create(plan))"),
            "{body}"
        );
        assert_eq!(body.matches("create(").count(), 1);
        // Free again afterwards, and usable by other code that spawns.
        drop(launch_lock());
    }

    #[test]
    fn m7_no_process_guard_nothing_can_launch() {
        assert!(!CAN_LAUNCH);
        let plan = LaunchPlan::new(
            Path::new(r"C:\Apps\browser.exe"),
            Path::new(r"C:\data\ai\browser\0f3a"),
        )
        .unwrap();
        assert_eq!(
            launch(&plan).err(),
            Some(LaunchFailure::at(LaunchStage::Unavailable))
        );
    }

    #[test]
    fn m7_no_process_guard_process_creation_sits_behind_one_gate() {
        const GATE: &str =
            "#[cfg(all(windows, not(test), not(feature = \"no-browser-process\")))]\nmod process {";
        let source = include_str!("launch.rs");
        let (before, rest) = source.split_once(GATE).expect("the gated module");
        let (_, tests) = rest.split_once("\n#[cfg(test)]\nmod tests {").unwrap();
        // The module is the last item before the tests, so everything that
        // is neither it nor a test is `before`.
        let starters = [
            ["Create", "ProcessW"].concat(),
            ["process::", "Command"].concat(),
            ["Command", "::new"].concat(),
            ["Shell", "Execute"].concat(),
        ];
        // No test opens a socket, and nothing in the crate's own code does.
        let sockets = [
            ["Tcp", "Listener"].concat(),
            ["Tcp", "Stream"].concat(),
            ["Udp", "Socket"].concat(),
        ];
        for starter in &starters {
            assert!(
                !before.contains(starter.as_str()),
                "{starter} outside the gate"
            );
            assert!(!tests.contains(starter.as_str()), "{starter} in a test");
        }
        assert!(rest.contains(starters[0].as_str()));
        // The feature turns the same gate off, and the stand-in is the
        // gate's exact complement.
        assert!(source.contains(
            "#[cfg(not(all(windows, not(test), not(feature = \"no-browser-process\"))))]\nfn create("
        ));
        // No other file of the crate can start a process either.
        for (name, file) in [
            ("lib.rs", include_str!("lib.rs")),
            ("origin.rs", include_str!("origin.rs")),
            ("installed.rs", include_str!("installed.rs")),
            ("transport.rs", include_str!("transport.rs")),
            ("protocol.rs", include_str!("protocol.rs")),
            ("fixture.rs", include_str!("fixture.rs")),
        ] {
            // Its tests may name what they forbid; its code may not.
            let code = file
                .split_once(
                    "
#[cfg(test)]
mod tests {",
                )
                .map_or(file, |(code, _)| code);
            for starter in &starters {
                assert!(!code.contains(starter.as_str()), "{starter} in {name}");
            }
            for socket in &sockets {
                assert!(!code.contains(socket.as_str()), "{socket} in {name}");
            }
        }
        for socket in &sockets {
            assert!(!source.contains(socket.as_str()), "{socket} in launch.rs");
        }
    }

    // ------------------------------------------------------ profile folder

    /// A disk in memory: folders, and links that resolve somewhere else.
    #[derive(Default)]
    struct Folders {
        folders: Mutex<Vec<PathBuf>>,
        links: BTreeMap<PathBuf, PathBuf>,
        files: Vec<PathBuf>,
    }

    impl Folders {
        fn with(folders: &[&str]) -> Self {
            Self {
                folders: Mutex::new(folders.iter().map(PathBuf::from).collect()),
                ..Self::default()
            }
        }

        fn has(&self, path: &str) -> bool {
            self.folders.lock().unwrap().contains(&PathBuf::from(path))
        }
    }

    impl FolderSystem for Folders {
        fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
            if let Some(target) = self.links.get(path) {
                return Ok(target.clone());
            }
            if self.folders.lock().unwrap().iter().any(|f| f == path)
                || self.files.iter().any(|f| f == path)
            {
                Ok(path.to_path_buf())
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        }

        fn create_dir(&self, path: &Path) -> std::io::Result<()> {
            let mut folders = self.folders.lock().unwrap();
            if folders.iter().any(|f| f == path)
                || self.links.contains_key(path)
                || self.files.iter().any(|f| f == path)
            {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            folders.push(path.to_path_buf());
            Ok(())
        }

        fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
            Ok(self.folders.lock().unwrap().iter().any(|f| f == path))
        }
    }

    #[test]
    fn m7_launch_args_the_profile_folder_rule() {
        let name = ProfileFolderName::new("0f3a-77").unwrap();
        // Made level by level under the root, and used again as it is.
        let disk = Folders::with(&["/root"]);
        let made = prepare_profile_folder(&disk, Path::new("/root"), &name).unwrap();
        assert_eq!(made, Path::new("/root").join("browser").join("0f3a-77"));
        assert!(disk.has(&Path::new("/root").join("browser").to_string_lossy()));
        assert_eq!(
            prepare_profile_folder(&disk, Path::new("/root"), &name).unwrap(),
            made
        );
        let refused = Err(LaunchFailure::at(LaunchStage::Folder));
        // No root: nothing is made.
        let disk = Folders::with(&[]);
        assert_eq!(
            prepare_profile_folder(&disk, Path::new("/root"), &name),
            refused
        );
        assert!(disk.folders.lock().unwrap().is_empty());
        // A link at either level that resolves elsewhere refuses.
        for level in ["browser", "browser/0f3a-77"] {
            let mut disk = Folders::with(&["/root", "/elsewhere"]);
            if level != "browser" {
                disk.folders
                    .lock()
                    .unwrap()
                    .push(Path::new("/root").join("browser"));
            }
            let mut link = PathBuf::from("/root");
            for part in level.split('/') {
                link = link.join(part);
            }
            disk.links.insert(link, PathBuf::from("/elsewhere"));
            assert_eq!(
                prepare_profile_folder(&disk, Path::new("/root"), &name),
                refused,
                "{level}"
            );
        }
        // A file where a folder should be refuses.
        let mut disk = Folders::with(&["/root"]);
        disk.files.push(Path::new("/root").join("browser"));
        assert_eq!(
            prepare_profile_folder(&disk, Path::new("/root"), &name),
            refused
        );
        // The root itself is taken in its canonical form.
        let mut disk = Folders::with(&["/real"]);
        disk.links
            .insert(PathBuf::from("/root"), PathBuf::from("/real"));
        assert_eq!(
            prepare_profile_folder(&disk, Path::new("/root"), &name).unwrap(),
            Path::new("/real").join("browser").join("0f3a-77")
        );
    }

    #[test]
    fn m7_launch_args_a_profile_folder_name_is_one_plain_component() {
        for name in ["a", "0", "0f3a-77", &"a".repeat(64)] {
            assert!(ProfileFolderName::new(name).is_some(), "{name:?}");
        }
        for name in [
            "",
            "-a",
            "A",
            "a b",
            "a.b",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "caf\u{e9}",
            &"a".repeat(65),
        ] {
            assert!(ProfileFolderName::new(name).is_none(), "{name:?}");
        }
        assert_eq!(
            plain_path(Path::new(r"\\?\C:\data\ai\browser\0f3a")),
            Path::new(r"C:\data\ai\browser\0f3a")
        );
        assert_eq!(
            plain_path(Path::new(r"\\?\UNC\server\share\p")),
            Path::new(r"\\server\share\p")
        );
        assert_eq!(plain_path(Path::new(r"C:\p")), Path::new(r"C:\p"));
    }

    // ---------------------------------------------------------------- stop

    #[derive(Default)]
    struct Tree {
        ended: AtomicU32,
        empty: AtomicBool,
        waited: AtomicU32,
    }

    impl ProcessTree for Tree {
        fn end(&self) {
            self.ended.fetch_add(1, Ordering::SeqCst);
            self.empty.store(true, Ordering::SeqCst);
        }

        fn wait_empty(&self, limit: Duration) -> bool {
            self.waited.fetch_add(1, Ordering::SeqCst);
            let until = Instant::now() + limit;
            while !self.empty.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.empty.load(Ordering::SeqCst)
        }

        fn active_processes(&self) -> Option<u32> {
            Some(u32::from(!self.empty.load(Ordering::SeqCst)))
        }

        fn process_ids(&self) -> Vec<u32> {
            Vec::new()
        }

        fn peak_memory_bytes(&self) -> Option<u64> {
            None
        }
    }

    const LONG: Duration = Duration::from_secs(20);

    #[test]
    fn launch_stop_cancel_never_waits_and_wakes_a_waiter() {
        let tree = Arc::new(Tree::default());
        let stop = BrowserStop::new(tree.clone(), LONG);
        assert!(!stop.cancelled());
        assert!(!stop.wait_cancelled(Duration::from_millis(1)));
        let waiter = stop.clone();
        let waiting = std::thread::spawn(move || {
            let started = Instant::now();
            (waiter.wait_cancelled(LONG), started.elapsed())
        });
        stop.cancel();
        let (cancelled, took) = waiting.join().unwrap();
        assert!(cancelled);
        assert!(took < LONG / 2);
        assert!(stop.cancelled());
        // A cancel ends nothing.
        assert_eq!(tree.ended.load(Ordering::SeqCst), 0);
        assert!(!stop.closed());
    }

    #[test]
    fn launch_stop_close_without_an_ask_ends_the_tree_at_once() {
        let tree = Arc::new(Tree::default());
        let stop = BrowserStop::new(tree.clone(), LONG);
        stop.close();
        assert!(stop.closed() && stop.cancelled());
        assert_eq!(tree.ended.load(Ordering::SeqCst), 1);
        assert_eq!(tree.waited.load(Ordering::SeqCst), 0);
        // Only the first close does anything.
        stop.close();
        stop.clone().close();
        assert_eq!(tree.ended.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn launch_stop_close_asks_first_and_does_not_wait_out_the_grace_on_an_orderly_exit() {
        let tree = Arc::new(Tree::default());
        let stop = BrowserStop::new(tree.clone(), LONG);
        let asked = Arc::new(AtomicBool::new(false));
        let (seen, exits) = (asked.clone(), tree.clone());
        stop.set_orderly_close(move || {
            seen.store(true, Ordering::SeqCst);
            // The browser closes by itself.
            exits.empty.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        stop.close();
        assert!(stop.cancelled());
        assert!(stop.wait_closed(LONG));
        assert!(started.elapsed() < LONG / 2);
        assert!(asked.load(Ordering::SeqCst));
        // The tree is ended all the same: nothing may be left behind.
        assert_eq!(tree.ended.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn launch_stop_close_ends_the_tree_when_the_grace_is_over() {
        let tree = Arc::new(Tree::default());
        let grace = Duration::from_millis(30);
        let stop = BrowserStop::new(tree.clone(), grace);
        // The browser ignores the ask.
        stop.set_orderly_close(|| {});
        let started = Instant::now();
        stop.close();
        // `close` itself came back before the grace was over.
        assert!(!stop.closed() || started.elapsed() >= grace);
        assert!(stop.wait_closed(LONG));
        assert!(started.elapsed() >= grace);
        assert_eq!(tree.ended.load(Ordering::SeqCst), 1);
        assert_eq!(tree.waited.load(Ordering::SeqCst), 1);
        // An ask set after the close is never run.
        let late = Arc::new(AtomicBool::new(false));
        let seen = late.clone();
        stop.set_orderly_close(move || seen.store(true, Ordering::SeqCst));
        stop.close();
        assert!(!late.load(Ordering::SeqCst));
    }
}
