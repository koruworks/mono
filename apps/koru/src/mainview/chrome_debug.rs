// SPDX-License-Identifier: MPL-2.0

//! `KORU_CHROME_DEBUG`: one line per thing the sidebar's chrome does — a
//! pane toggle, an app switch, a popover or the help menu opening — on
//! stderr, behind the switch, so a question about the chrome is one run
//! rather than one afternoon (`guide/contributing/PRINCIPLES.md`,
//! *instrument before guessing*). Routine, so never unconditional; never per
//! frame. Ids, counts and names of controls — never a document's text.

use std::sync::OnceLock;

pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| koru_env::switch(koru_env::name::KORU_CHROME_DEBUG))
}

pub(super) fn log(args: std::fmt::Arguments<'_>) {
    if on() {
        eprintln!("[Koru][chrome] {args}");
    }
}

macro_rules! chrome_debug {
    ($($arg:tt)*) => {
        $crate::mainview::chrome_debug::log(format_args!($($arg)*))
    };
}
pub(super) use chrome_debug;
