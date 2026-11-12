// SPDX-License-Identifier: MPL-2.0

//! Where the daemon's output goes.
//!
//! A console daemon prints and retains the same records in a file. A tray
//! daemon has nowhere to print, so it writes only to that file — and the moment
//! there is a file there is a question about
//! how long it grows for, which is the question this module answers by **not
//! answering it**: `tracing-appender` rolls daily and keeps seven files, and
//! none of that logic is ours.
//!
//! ```text
//! <store root>/logs/koru-store-server.log.2026-08-09
//! ```
//!
//! Under the channel's root rather than beside the executable, which may be in
//! Program Files and unwritable — and per-channel, so a nightly store's log
//! cannot be mistaken for a stable one's.
//!
//! ## Why `tracing` and not a `log` facade
//!
//! Because of where this goes next. A cloud deployment adds an exporter —
//! `tracing-opentelemetry`, or a vendor's layer — as **one more layer in this
//! same initialiser**, behind a cargo feature, with no change at any call site.
//! `log4rs` and `flexi_logger` both rotate perfectly well and neither has that
//! shape. That layer and that feature are deliberately not added now.
//!
//! The process-level machinery lives in the leaf `koru-log` crate and is shared
//! with `koru-mcp`; `koru-store-core` still does not inherit it. A mobile build
//! embeds core, and a logging framework is not something an embedder should
//! inherit.
//!
//! ## Two process concerns
//!
//! **The shared panic hook** sends a panic through tracing before delegating to
//! Rust's previous hook. A `windows_subsystem = "windows"` process otherwise
//! disappears without a console record.
//!
//! **A message box on a fatal start-up failure**, because before the store is
//! serving there is no tray either. Without it, "I double-clicked it and nothing
//! happened" is the entire user experience of a bad database.

use std::path::Path;

/// Held for the daemon's life so the non-blocking writer keeps running.
pub use koru_log::Logging;

/// What is recorded when nothing says otherwise.
///
/// **`info` for us, `warn` for everything else.** Without the second half the
/// log fills with hyper's and tungstenite's `TRACE` — one line per frame on a
/// busy socket, which drowns the three lines that are actually about the store
/// and turns a seven-day log into a seven-hour one. Overridable through
/// `RUST_LOG` for the afternoon somebody needs exactly that.
///
/// `koru_network_iroh` is in the set because sync and blob hydration are
/// event-driven, not per-tick: its `info` lines are sync passes, hydration state
/// changes, and unusual reconnect bursts — news on exactly the incidents
/// somebody reads this file for. Routine peer lifecycle remains behind
/// `KORU_SYNC_DEBUG`. The mobile host already ships the same base level.
///
/// `koru_readings` is the target every reading line names (the coordinator,
/// the mirror, the reader's installer): it does not start with `koru_store`,
/// so without its own directive its `info` lines — the reader installed or
/// removed, a backlog finished, and the one line per job `KORU_READ_DEBUG`
/// asks for — were dropped, which OCR TECHNICAL §14 says are unconditional.
/// Every filter below names it; `the_reading_lines_reach_every_filter` holds
/// that.
const DEFAULT_FILTER: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_store=info,koru_store_api=info,koru_store_core=info,koru_replication=info,koru_network_iroh=info,koru_events=info,koru_metrics=info,koru_readings=info";
const DEFAULT_FILTER_SYNC_DEBUG: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_store=info,koru_store_api=info,koru_store_core=info,koru_replication=info,koru_network_iroh=info,koru_events=info,koru_metrics=info,koru_readings=info,koru_sync::peer=debug";

/// The same, one level louder for our own crates.
///
/// **`--console` means somebody is watching this particular process** — a
/// developer at a terminal, or a harness reading its output — and what they are
/// almost always watching for is a document: which snapshot went out, how big it
/// was, whose update came in. Those are `debug`, because on a busy store they
/// are one line per frame and have no business in a file that rotates for seven
/// days.
///
/// The access-log override keeps ordinary successful GET polling (logged at
/// DEBUG) out of this otherwise-debug console. Mutations, failures and protocol
/// upgrades retain their INFO/WARN levels, and `RUST_LOG` can still opt back in.
/// `noq-proto` reports an idle single-path close as `MultipathNotNegotiated` at
/// WARN; that is expected for our ordinary non-multipath connections, so its
/// connection module stays at ERROR unless a developer explicitly overrides it.
///
/// A tray launch keeps `info`. `RUST_LOG` still overrides both.
const CONSOLE_FILTER: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_store=debug,koru_store_api=debug,koru_store_api::access_log=info,koru_store_core=debug,koru_replication=info,koru_network_iroh=debug,koru_events=info,koru_metrics=info,koru_readings=info";
const CONSOLE_FILTER_SYNC_DEBUG: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_store=debug,koru_store_api=debug,koru_store_api::access_log=info,koru_store_core=debug,koru_replication=info,koru_network_iroh=debug,koru_events=info,koru_metrics=info,koru_readings=info,koru_sync::peer=debug";

/// The product gateway owns the AI worker as well as the embedded Store.
///
/// Keep ordinary lifecycle records from both domains in the retained log. A
/// console launch opts the AI timing summaries into DEBUG alongside the Store's
/// document diagnostics; neither filter admits provider payloads or user text.
const GATEWAY_FILTER: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_ai=info,koru_ai_server=info,koru_ai_sqlite=info,koru_store=info,koru_store_api=info,koru_store_core=info,koru_replication=info,koru_network_iroh=info,koru_events=info,koru_metrics=info,koru_readings=info";
const GATEWAY_CONSOLE_FILTER: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_ai=debug,koru_ai_server=debug,koru_ai_sqlite=debug,koru_store=debug,koru_store_api=debug,koru_store_api::access_log=info,koru_store_core=debug,koru_replication=info,koru_network_iroh=debug,koru_events=info,koru_metrics=info,koru_readings=info";
const GATEWAY_FILTER_SYNC_DEBUG: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_ai=info,koru_ai_server=info,koru_ai_sqlite=info,koru_store=info,koru_store_api=info,koru_store_core=info,koru_replication=info,koru_network_iroh=info,koru_events=info,koru_metrics=info,koru_readings=info,koru_sync::peer=debug";
const GATEWAY_CONSOLE_FILTER_SYNC_DEBUG: &str = "warn,noq_proto::connection=error,koru_api_gateway=info,koru_api_gateway_core=info,koru_ai=debug,koru_ai_server=debug,koru_ai_sqlite=debug,koru_store=debug,koru_store_api=debug,koru_store_api::access_log=info,koru_store_core=debug,koru_replication=info,koru_network_iroh=debug,koru_events=info,koru_metrics=info,koru_readings=info,koru_sync::peer=debug";

fn sync_debug_filter(ordinary: &'static str, debug: &'static str) -> &'static str {
    if koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
        debug
    } else {
        ordinary
    }
}

/// Start logging.
///
/// `console` adds stdout beside the rotating file. Both modes retain a log: the
/// console is for a failure while it happens, and the file is for diagnosing it
/// after the terminal or process has gone.
pub fn init(root: &Path, console: bool) -> Logging {
    if console {
        // One event, two destinations; neither copy has a private logging path.
        return koru_log::init_file_and_stdout(
            root,
            "koru-store-server.log",
            sync_debug_filter(CONSOLE_FILTER, CONSOLE_FILTER_SYNC_DEBUG),
        );
    }
    koru_log::init_file(
        root,
        "koru-store-server.log",
        sync_debug_filter(DEFAULT_FILTER, DEFAULT_FILTER_SYNC_DEBUG),
    )
}

/// Start gateway-owned process logging. Embedded Store events share the same
/// global tracing subscriber but use a gateway-specific file name so a
/// standalone diagnostic Store launch is never confused with the product.
pub fn init_gateway(root: &Path, console: bool) -> Logging {
    if console {
        return koru_log::init_file_and_stdout(
            root,
            "koru-api-gateway.log",
            sync_debug_filter(GATEWAY_CONSOLE_FILTER, GATEWAY_CONSOLE_FILTER_SYNC_DEBUG),
        );
    }
    koru_log::init_file(
        root,
        "koru-api-gateway.log",
        sync_debug_filter(GATEWAY_FILTER, GATEWAY_FILTER_SYNC_DEBUG),
    )
}

/// The one way to say nothing when a store cannot start.
///
/// **For test harnesses, and set by nothing that ships.** A store that cannot
/// open its databases is a real emergency — somebody's local storage is gone or
/// unreadable — and a person who launched one deserves to be told, which on a
/// tray launch means a modal dialog. A *suite* that starts thirty daemons on
/// throwaway roots is a different thing entirely: the dialog it raises answers
/// nobody, and `MessageBoxW` blocks until it is dismissed, so the process never
/// exits and goes on holding its databases and its own binary — a build that
/// then fails with "Access is denied", minutes away from the cause.
///
/// An environment variable rather than a flag because it has to cross a process
/// boundary the harness does not control: the standalone test host runs in a
/// daemon and passes nothing but the verb and a root, and a child inherits the
/// environment for free. The same reason `KORU_FRAME_STATS` is one.
pub const NO_DIALOG: &str = "KORU_STORE_NO_DIALOG";

/// Say that the store could not start, on a machine where nothing else can.
///
/// Only for a failure **before** the store is serving. After that there is a
/// tray icon and a log file, and a modal dialog would be an interruption rather
/// than the only way to find out.
pub fn report_fatal(message: &str) {
    tracing::error!("{message}");
    if koru_env::switch(NO_DIALOG) {
        // Said out loud, so a suppressed dialog is never a silent one.
        tracing::error!("{NO_DIALOG} is set; not showing the dialog");
        return;
    }
    platform::message_box(message);
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    pub fn message_box(message: &str) {
        let text: Vec<u16> = OsStr::new(message).encode_wide().chain(Some(0)).collect();
        let title: Vec<u16> = OsStr::new("Koru Store")
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: both strings are NUL-terminated and outlive the call; a null
        // owner window is a valid, ownerless dialog.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            )
        };
    }
}

#[cfg(not(windows))]
mod platform {
    /// No dialog. A process started from a terminal still has stderr, which is
    /// where the error already went.
    pub fn message_box(_message: &str) {}
}

#[cfg(test)]
mod tests;
