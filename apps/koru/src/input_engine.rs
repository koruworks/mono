// SPDX-License-Identifier: MPL-2.0

//! **The text engine an element tree is laid out with when no paint is
//! running** — a press, a key, a hover or the accessibility tree.
//!
//! Paint is handed the window's engine; input arrives without one, and an
//! element frame cannot be laid out without an engine to shape with. One
//! per UI thread, made on first use and shared by every surface that answers
//! input from an element frame (the switcher's pages, the Tasks page's rows),
//! so a second window, a reopened pane or a second surface does not pay for
//! a font context again. It was the switcher's private helper until the
//! Tasks rows needed the same thing.

use std::cell::RefCell;

use koru_draw::text::TextEngine;

thread_local! {
    static ENGINE: RefCell<Option<TextEngine>> = const { RefCell::new(None) };
}

/// `f` with the thread's input-layout engine.
pub(crate) fn with<R>(f: impl FnOnce(&mut TextEngine) -> R) -> R {
    ENGINE.with_borrow_mut(|engine| f(engine.get_or_insert_with(TextEngine::new)))
}
