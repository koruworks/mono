// SPDX-License-Identifier: MPL-2.0

//! Running a role of our own executable — or a sibling program — as a
//! supervised child.
//!
//! **This crate can name no role.** It owns the mechanism a self-hosted
//! worker needs — the early-`main` dispatch that turns `koru-agent.exe
//! --worker <role>` into that role's entry point, the `current_exe()` spawn,
//! the one-line bootstrap over an inherited stdin pipe that doubles as the
//! parent-death signal *and the watcher that notices it*, the per-child
//! Windows Job Object so a worker's whole
//! process tree dies with it, readiness and health probing, a bounded
//! restart budget with backoff, and the hold/resume window an update needs —
//! and nothing about what any particular worker does. Role knowledge
//! (endpoints, authorities, bootstrap contents) stays with the role, passed
//! in as closures and an opaque payload.
//!
//! **The same mechanism runs a sibling program** (OCR roadmap Plan 1.4): a
//! supervised child may be a separately shipped executable resolved beside
//! `current_exe()` ([`supervisor::Program::Sibling`]) instead of a role of
//! this one, and any child may carry a ceiling on its tree's committed memory
//! ([`supervisor::WorkerSpec::memory_limit`], Windows only —
//! [`supervisor::MEMORY_LIMIT_ENFORCED`]). Which sibling, and why it needs a
//! ceiling, is the caller's knowledge; the crate names neither. A sibling
//! that links this crate reads its bootstrap and watches its parent with the
//! same [`read_bootstrap_line`] and [`ParentPipe`] a role does.
//!
//! **And a child can be kept out of the person's way** (OCR roadmap Plan
//! 3.1): [`supervisor::WorkerSpec::priority`] launches it below normal
//! priority, and the child completes that with [`follow_launch_priority`],
//! because background I/O is something only a process can ask for itself.
//!
//! Two sides, two entry points:
//!
//! - The **worker side** calls [`dispatch`] as the first thing in `main`.
//!   When `--worker <role>` is present the matching role's entry runs and
//!   never returns; an unknown role or malformed worker arguments are
//!   refused loudly (the token named, the roles listed, exit 2) before any
//!   parent machinery could start. With no `--worker` argument, `dispatch`
//!   returns and `main` continues as the parent.
//! - The **parent side** calls [`supervisor::start`] with a
//!   [`supervisor::WorkerSpec`], getting back a [`supervisor::WorkerHandle`]
//!   whose current-child accessor is generation-fenced: an operation minted
//!   against child A is refused once child B has taken over, never routed
//!   to B through their shared endpoint.
//!
//! The state machine itself is pure and lives in [`machine`], where it is
//! unit-tested without processes; the supervisor's control thread is the
//! only place timers, pipes and process handles meet it.

pub mod machine;
pub mod supervisor;

#[cfg(windows)]
mod windows_job;

use std::io::Read;

/// Set by the supervisor on a worker it spawned as a process-group leader.
/// Only such a worker may kill its own group on parent loss: a process that
/// merely calls [`ParentPipe::watch`] — a test runner, say — must never.
#[cfg(unix)]
pub(crate) const OWNS_GROUP_ENV: &str = "KORU_WORKER_OWNS_GROUP";

/// How long a worker whose parent died gets to shut down on its own terms
/// before its whole group is killed anyway.
///
/// **A backstop for a role that hangs, not a deadline for one that works.**
/// The session host's shutdown ends each session's PTY tree — its own
/// session, outside this group, so only the host can — one after another,
/// each bounded by ten seconds; a grace as short as one teardown would cut the
/// host down mid-shutdown and leave exactly the trees this exists to end.
#[cfg(unix)]
const PARENT_LOSS_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// SIGKILL a worker's process group, which it leads from spawn so its id is
/// the worker's pid. `ESRCH` means the group is empty — every member is
/// already gone — which is the outcome wanted, not an error to report: the
/// group was made by us for this child, so there is no second group it could
/// have been.
#[cfg(unix)]
pub(crate) fn kill_group(leader: u32) {
    let Ok(leader) = libc::pid_t::try_from(leader) else {
        return;
    };
    if leader <= 1 {
        return;
    }
    // SAFETY: a plain syscall with a negated pid naming one process group.
    unsafe {
        libc::kill(-leader, libc::SIGKILL);
    }
}

/// Whether this process is a supervised worker leading its own group — the
/// only case in which killing "my group" cannot reach anything but my tree.
#[cfg(unix)]
fn owns_its_group() -> bool {
    // SAFETY: two argument-free syscalls.
    let (group, pid) = unsafe { (libc::getpgrp(), libc::getpid()) };
    group == pid && std::env::var_os(OWNS_GROUP_ENV).is_some()
}

/// On parent loss, a worker's own descendants must not outlive it — the
/// Windows Job Object guarantees that when the parent's handle closes, and
/// unix has nothing that does. So: kill the group when the worker exits, and
/// kill it anyway after a grace period if the role's shutdown hangs. The role's
/// graceful path still runs first, because it may own trees in *other* groups
/// (a PTY session is its own session) that only it knows how to end.
#[cfg(unix)]
fn contain_group_after_parent_loss() {
    extern "C" fn kill_own_group() {
        // SAFETY: `kill(0, …)` names the caller's own process group.
        unsafe {
            libc::kill(0, libc::SIGKILL);
        }
    }
    if !owns_its_group() {
        return;
    }
    // SAFETY: registering a plain `extern "C"` function with no captures.
    unsafe {
        libc::atexit(kill_own_group);
    }
    let _ = std::thread::Builder::new()
        .name("parent-loss grace".into())
        .stack_size(64 * 1024)
        .spawn(|| {
            std::thread::sleep(PARENT_LOSS_GRACE);
            kill_own_group();
        });
}

/// A worker role the dispatching binary understands.
///
/// The entry takes the arguments that followed `--worker <name>` and never
/// returns — a worker process is its role for its whole life.
pub struct Role {
    pub name: &'static str,
    pub run: fn(Vec<String>) -> !,
}

/// Turn `<exe> --worker <role> [args…]` into that role, or return to let the
/// parent continue.
///
/// Call this before anything else in `main` — before logging, before CLI
/// parsing, before any single-instance claim — so a worker process can never
/// half-become an Agent. The strict-CLI rule applies: `--worker` anywhere
/// but first, a missing role name, or an unknown role each name the token,
/// print the usage, and exit 2.
pub fn dispatch(args: impl IntoIterator<Item = String>, roles: &[Role]) {
    let mut words = args.into_iter();
    let exe = words.next().unwrap_or_else(|| "koru-agent".into());
    let rest: Vec<String> = words.collect();
    let Some(position) = rest.iter().position(|word| word == "--worker") else {
        return;
    };
    if position != 0 {
        refuse(&exe, roles, "`--worker` must be the first argument");
    }
    let Some(name) = rest.get(1) else {
        refuse(&exe, roles, "`--worker` needs a role name");
    };
    let Some(role) = roles.iter().find(|role| role.name == name) else {
        refuse(&exe, roles, &format!("unknown worker role `{name}`"));
    };
    (role.run)(rest[2..].to_vec());
}

fn refuse(exe: &str, roles: &[Role], why: &str) -> ! {
    let known: Vec<&str> = roles.iter().map(|role| role.name).collect();
    eprintln!("{exe}: {why}");
    eprintln!("usage: {exe} --worker <{}> …", known.join("|"));
    std::process::exit(2)
}

/// What is left of the bootstrap pipe once its line has been read: the
/// cross-platform parent-death signal.
///
/// **It lives beside [`read_bootstrap_line`] because the two are one
/// mechanism.** The parent writes one line and then holds the write end open
/// for the child's whole life, so the same descriptor answers two questions in
/// sequence — *what am I* and then *is my parent still there*. Both roles that
/// read the line also have to watch it, and a private copy of the watcher in
/// each of them is a place the next platform fix would not be applied.
///
/// It carries no session, gateway or bootstrap content, and it never looks at
/// a byte it reads: only *end of stream* means anything.
pub struct ParentPipe(Box<dyn Read + Send>);

impl ParentPipe {
    pub fn new(reader: impl Read + Send + 'static) -> Self {
        Self(Box::new(reader))
    }

    /// Spawn the named thread that blocks on the pipe and calls
    /// `on_parent_loss` when the parent is gone.
    ///
    /// **The watcher runs on a thread of its own, and nothing joins it.** A
    /// guard runs off the thread it guards: a blocking read on the thread that
    /// is meant to notice the block would be the bug, and joining a thread
    /// parked in `read` is how a shutdown wedges. The handler is called once,
    /// on that thread, and should be a wake rather than the shutdown itself.
    ///
    /// **Only EOF or a read error counts.** A stray byte from a confused
    /// parent is skipped rather than treated as a death — the signal is that
    /// the pipe *ended*, and reading one byte and stopping would make any
    /// second line a shutdown request nobody sent.
    ///
    /// An `Err` means the thread could not be created, which is the one case
    /// in which the signal would silently never fire; the caller decides
    /// whether to serve without it.
    pub fn watch(
        self,
        thread_name: &str,
        on_parent_loss: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<()> {
        let ParentPipe(mut pipe) = self;
        std::thread::Builder::new()
            .name(thread_name.to_owned())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut byte = [0; 1];
                loop {
                    match pipe.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                #[cfg(unix)]
                contain_group_after_parent_loss();
                on_parent_loss();
            })
            .map(|_| ())
    }
}

/// **The child's half of [`supervisor::Priority::Background`]**: a child that
/// finds itself launched below normal priority lowers its own I/O too.
/// Returns whether it did.
///
/// On Windows a process can enter background mode
/// (`PROCESS_MODE_BACKGROUND_BEGIN`: very low I/O priority and low memory
/// priority) only for itself, so the parent's launch cannot do it; the
/// priority class the launch gave is the signal, which needs no new argument
/// or variable and leaves a child started by hand at normal priority alone.
///
/// **Entering background mode resets the priority class to normal**
/// (measured on Windows 11 build 26300: a below-normal process reads
/// `NORMAL_PRIORITY_CLASS`, base priority 8, straight afterwards, and is
/// still in background mode — a second `BEGIN` fails with
/// `ERROR_PROCESS_MODE_ALREADY_BACKGROUND`). So the class the launch gave is
/// put back after it, and the child ends below normal with background I/O.
///
/// Elsewhere it does nothing and returns `false`
/// ([`supervisor::PRIORITY_LOWERED`] says what the catch-up needs).
pub fn follow_launch_priority() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, GetPriorityClass, IDLE_PRIORITY_CLASS,
            PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass,
        };
        // SAFETY: the pseudo-handle of this process, and two calls that take
        // it and a plain integer.
        unsafe {
            let me = GetCurrentProcess();
            let launched = GetPriorityClass(me);
            if launched != BELOW_NORMAL_PRIORITY_CLASS && launched != IDLE_PRIORITY_CLASS {
                return false;
            }
            if SetPriorityClass(me, PROCESS_MODE_BACKGROUND_BEGIN) == 0 {
                return false;
            }
            SetPriorityClass(me, launched);
            true
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The bootstrap a parent wrote is one newline-terminated line on stdin,
/// bounded so a confused writer cannot make the child buffer forever.
///
/// The reader is left open on purpose: the same pipe is the cross-platform
/// parent-death signal, and a role that reads EOF from it knows its parent
/// is gone. [`ParentPipe`] is what watches for that.
pub fn read_bootstrap_line(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    const MAX_BOOTSTRAP: usize = 64 * 1024;
    let mut bytes = Vec::with_capacity(1_024);
    let mut byte = [0; 1];
    while bytes.len() < MAX_BOOTSTRAP {
        reader.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            return Ok(bytes);
        }
        bytes.push(byte[0]);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "worker bootstrap exceeds 64 KiB",
    ))
}

#[cfg(test)]
mod tests;
