// SPDX-License-Identifier: MPL-2.0

//! What a [`Kind::Toggle`](crate::Kind::Toggle) means to a reader.
//!
//! A toggle is a line with a body: its text is the title, its children are
//! the body. The model stores **nothing** about whether it is open. That is a
//! reader's private view preference, kept by the Store beside the document in
//! a separate Loro document of its own (`document_private_crdts`) and never
//! replicated, so folding never changes another collaborator's view and never
//! makes a shared edit. Missing state means open.
//!
//! This module holds the one stable key that preference is stored under, so
//! the Store, the API and the editor name it the same way.

/// The private attribute that records a toggle the reader closed. Boolean;
/// absent and `false` both mean open.
pub const COLLAPSED: &str = "koru.collapsed";
