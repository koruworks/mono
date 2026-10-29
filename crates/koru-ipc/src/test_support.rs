// SPDX-License-Identifier: MPL-2.0

//! Shared by the suites that touch a real endpoint.
//!
//! ## Why this exists, which the first version of these tests got wrong
//!
//! An endpoint is **exclusive by design**, and cargo runs a crate's tests in
//! parallel threads of one process. So a suite binding a channel and a suite
//! asserting that nothing is listening on one are not independent: the second
//! was reading the first's listener and failing, in a way that passed whenever
//! the two happened not to overlap.
//!
//! Two answers, and the order matters:
//!
//! 1. **Prefer not to depend on ambient state at all.** A test that needs "an
//!    endpoint nobody holds" can have one for certain by scoping it to a root
//!    that does not exist, rather than by hoping a channel is quiet. That is
//!    [`nowhere`], and it needs no lock.
//! 2. **Serialise the rest.** A test that genuinely has to use a *channel* —
//!    because it is the channel rule under test — takes [`exclusive`].

use std::sync::{Mutex, MutexGuard, OnceLock};

/// The lock every test that binds or probes a bare channel endpoint must hold.
///
/// Poisoning is ignored deliberately: one failing test must not cascade into
/// every other test in the file reporting a poisoned mutex instead of its own
/// result, which turns one red line into twenty and hides the cause.
pub fn exclusive() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An endpoint that certainly has nothing behind it.
///
/// Scoped to a root that does not exist, so it is unique to this call and
/// nothing in this process or any other has bound it. This is what makes the
/// "nothing is listening" tests deterministic rather than dependent on which
/// other test happens to be running.
pub fn nowhere(label: &str) -> crate::Endpoint {
    let root = std::env::temp_dir()
        .join("koru-ipc-tests-no-such-service")
        .join(label);
    crate::Endpoint::for_service_root(crate::STORE, koru_channel::segment(), &root)
        .expect("this build's own channel is always a valid segment")
}

/// A **bindable** endpoint nobody else in this process or on this machine
/// holds: a temp root of its own, so two tests never meet.
pub fn scratch(service: &str, label: &str) -> crate::Endpoint {
    let root = std::env::temp_dir()
        .join("koru-ipc-tests-scratch")
        .join(label);
    crate::Endpoint::for_service_root(service, koru_channel::segment(), &root)
        .expect("this build's own channel is always a valid segment")
}
