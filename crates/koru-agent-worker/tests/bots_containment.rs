// SPDX-License-Identifier: MPL-2.0

//! P0.3: real nested supervisor jobs plus the session runtime's provider job.
//! No real agent, Store, account, window or disk fixture. The same binary is
//! each process in the chain; readiness is reported over a private loopback
//! socket only after the child has been created. All waits have deadlines.

#[cfg(windows)]
#[path = "bots_containment/windows.rs"]
mod platform;

fn main() {
    #[cfg(windows)]
    platform::run();
    #[cfg(not(windows))]
    println!("SKIP bots containment: Windows proof only");
}
