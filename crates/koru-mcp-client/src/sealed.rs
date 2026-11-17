// SPDX-License-Identifier: MPL-2.0

//! A value that does not print.

use std::fmt;

/// A value whose `Debug` output is its size and nothing else.
///
/// An answer from the hosted server can hold a document's content, a title, a
/// connection's name or a claim's purpose, and a request can hold the same.
/// Wrapping one here means that deriving `Debug` on anything that carries it,
/// printing an error, or a failed assertion cannot put that text in a log. A
/// caller that needs the value asks for it with [`Sealed::expose`], which is
/// easy to find in a review.
///
/// It is not encryption and it does not clear memory. It only keeps the value
/// out of formatted output.
#[derive(Clone, PartialEq, Eq)]
pub struct Sealed<T>(T);

impl<T> Sealed<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The value. The caller now owns where it goes.
    pub fn expose(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl fmt::Debug for Sealed<serde_json::Value> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The serialized length says whether there was an answer and roughly
        // how much, which is all a log needs.
        let bytes = serde_json::to_vec(&self.0).map_or(0, |bytes| bytes.len());
        write!(f, "Sealed({bytes} bytes)")
    }
}

impl fmt::Debug for Sealed<String> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sealed({} bytes)", self.0.len())
    }
}

impl fmt::Debug for Sealed<Vec<u8>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sealed({} bytes)", self.0.len())
    }
}
