// SPDX-License-Identifier: MPL-2.0
//! Explicit Windows process/disk proof: --run --root <new absolute directory>.
#[cfg(windows)]
#[path = "bot_process/windows.rs"]
mod windows;
fn main() {
    #[cfg(windows)]
    windows::run();
    #[cfg(not(windows))]
    println!("SKIP: Windows containment is the supported launcher");
}
