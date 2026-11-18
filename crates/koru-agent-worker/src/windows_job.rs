// SPDX-License-Identifier: MPL-2.0

//! A kill-on-close Job Object per worker child, so the whole process tree —
//! the worker and anything it spawned — dies when the worker is killed, and
//! dies with the parent when the parent's handle table is torn down.
//!
//! The same shape `koru-session-runtime` uses for PTY trees. Windows allows
//! nested jobs since 8.1, so a sessions worker that puts its PTYs in jobs of
//! its own still sits inside this one; the integration test proves a
//! grandchild dies with the worker rather than assuming it.
//!
//! **An optional memory ceiling, and what Windows actually does at it.** With
//! a limit, the job carries `JOB_OBJECT_LIMIT_JOB_MEMORY`: the committed
//! memory of the child's whole tree is capped. Windows does **not** end a
//! process that reaches it — it refuses the commit that would cross it, and
//! posts `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` to the job's completion port. A
//! Rust child aborts on a refused allocation, but a child that handles the
//! refusal would carry on, so the port is watched ([`MemoryWatch`]) and the
//! supervisor ends the job itself when the message arrives. Without a limit
//! there is no port and the job is configured exactly as it always was.

use std::os::windows::io::AsRawHandle as _;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::INFINITE;

/// `winnt.h`'s message numbers for the two memory limits. Spelled here rather
/// than taken from `Win32_System_SystemServices`, a feature this crate would
/// enable for two integers.
const JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT: u32 = 9;
const JOB_OBJECT_MSG_JOB_MEMORY_LIMIT: u32 = 10;

/// The completion key the job's own notifications arrive under.
const JOB_KEY: usize = 1;
/// The key [`MemoryWatch::release`] posts to wake the watch for good.
const RELEASE_KEY: usize = 2;

pub struct Job {
    handle: HANDLE,
    /// Present only when the job carries a memory limit.
    port: Option<Arc<Port>>,
}

// A job handle is a kernel object reference; moving it between threads is
// exactly how the control thread outlives the spawner.
unsafe impl Send for Job {}

/// The job's completion port, shared by the job and its watcher thread and
/// closed when the last of them lets go.
struct Port(HANDLE);

// A completion port is built to be waited on from one thread and posted to
// from others.
unsafe impl Send for Port {}
unsafe impl Sync for Port {}

impl Drop for Port {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl Job {
    /// A kill-on-close job, capped at `memory_limit` bytes of committed memory
    /// for the whole tree when one is given.
    pub fn new(memory_limit: Option<u64>) -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "could not create a worker job object: {}",
                std::io::Error::last_os_error()
            ));
        }
        // Owned from here on, so every early return below closes it.
        let mut job = Self { handle, port: None };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(bytes) = memory_limit {
            limits.JobMemoryLimit = usize::try_from(bytes).map_err(|_| {
                format!("a memory limit of {bytes} bytes does not fit this platform")
            })?;
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            // **The port before the limit.** A job reports nothing to a port
            // associated after the fact, and the limit is only worth setting
            // if reaching it is heard.
            job.port = Some(Arc::new(job.associate_port()?));
        }
        let ok = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(format!(
                "could not configure the worker job object: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(job)
    }

    fn associate_port(&self) -> Result<Port, String> {
        let raw =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if raw.is_null() {
            return Err(format!(
                "could not create the worker job's completion port: {}",
                std::io::Error::last_os_error()
            ));
        }
        let port = Port(raw);
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: JOB_KEY as *mut core::ffi::c_void,
            CompletionPort: port.0,
        };
        let ok = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectAssociateCompletionPortInformation,
                &association as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        };
        if ok == 0 {
            return Err(format!(
                "could not attach a completion port to the worker job: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(port)
    }

    pub fn assign(&self, child: &std::process::Child) -> Result<(), String> {
        let ok = unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as HANDLE) };
        if ok == 0 {
            return Err(format!(
                "could not place the worker in its job object: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// The watch over this job's memory limit, if it has one.
    pub fn memory_watch(&self) -> Option<MemoryWatch> {
        self.port.as_ref().map(|port| MemoryWatch(Arc::clone(port)))
    }

    /// End everything in the job now. Idempotent; the exit still arrives
    /// through the waiter's `wait()`.
    pub fn kill(&self) {
        unsafe { TerminateJobObject(self.handle, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // A watch nobody released — a spawn that failed after the job was
        // made — must not block for ever on a job that is going away.
        if let Some(watch) = self.memory_watch() {
            watch.release();
        }
        // KILL_ON_JOB_CLOSE makes this close a kill for anything still
        // alive — which is the point: a parent that vanishes takes its
        // worker trees with it.
        unsafe { CloseHandle(self.handle) };
    }
}

/// One child's view of its job's completion port: *did the tree reach its
/// memory limit*.
///
/// **Delivery is the operating system's best effort.** Microsoft documents
/// that job notifications other than notification limits are not guaranteed
/// to arrive. A lost message leaves a child that ended itself on a refused
/// allocation reported as an ordinary exit — never the reverse.
///
/// A clone is the same port: one side waits, the other releases.
#[derive(Clone)]
pub struct MemoryWatch(Arc<Port>);

impl MemoryWatch {
    /// Block until the job reports its memory limit (`true`), or the watch is
    /// released or its port fails (`false`). Every other job message — a
    /// process joining or leaving — is passed over.
    pub fn wait(&self) -> bool {
        loop {
            let mut message = 0u32;
            let mut key = 0usize;
            let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    (self.0).0,
                    &mut message,
                    &mut key,
                    &mut overlapped,
                    INFINITE,
                )
            };
            if ok == 0 {
                return false;
            }
            match key {
                RELEASE_KEY => return false,
                JOB_KEY
                    if message == JOB_OBJECT_MSG_JOB_MEMORY_LIMIT
                        || message == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT =>
                {
                    return true;
                }
                _ => continue,
            }
        }
    }

    /// Wake [`MemoryWatch::wait`] with `false`, after anything the job has
    /// already queued: a completion port hands its packets out in order, so a
    /// limit reached before the release is still read first.
    pub fn release(&self) {
        unsafe { PostQueuedCompletionStatus((self.0).0, 0, RELEASE_KEY, std::ptr::null()) };
    }
}
