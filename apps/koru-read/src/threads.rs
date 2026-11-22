// SPDX-License-Identifier: MPL-2.0

//! **How many threads a recognition may use: `KORU_READ_THREADS`.**
//!
//! Four by default, Plan 0.1's measurement: above four there was little left
//! to gain, and the worker shares the machine with a person typing. The same
//! number bounds ONNX Runtime's pool and `oar-ocr`'s own, which would
//! otherwise use every core. A value that is not a whole number is named and
//! ignored rather than fatal, as `KORU_READ_IDLE_EXIT` is.

use koru_read_paddle::Settings;

/// The settings `KORU_READ_THREADS` asks for.
pub fn from_env() -> Settings {
    let raw = std::env::var(koru_env::name::KORU_READ_THREADS).ok();
    from_value(raw.as_deref())
}

/// The same, from the variable's text.
pub fn from_value(raw: Option<&str>) -> Settings {
    match Settings::from_setting(raw) {
        Ok(settings) => settings,
        Err(fallback) => {
            tracing::warn!(
                variable = koru_env::name::KORU_READ_THREADS,
                "not a whole number of threads; using the default"
            );
            fallback
        }
    }
}

#[cfg(test)]
mod tests;
