// SPDX-License-Identifier: MPL-2.0

//! **The UI Lab**: Koru's own components, shown one at a time over synthetic
//! state so they can be tried without a workspace.
//!
//! It is this binary launched as `koru --ui`, in its own process. Nothing in
//! here opens a profile, a Store, the Agent or a settings file, and nothing is
//! saved when the window closes.
//!
//! [`registry`] is the single source for what the Lab lists: each entry names
//! the production symbol that draws it and the options that symbol really
//! takes. The components themselves stay with their owners; the Lab holds
//! stories and fixtures only.

pub(crate) mod registry;
