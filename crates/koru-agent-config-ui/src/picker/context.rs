// SPDX-License-Identifier: MPL-2.0

//! **The context pill's words** (koru-ai D10, D76): a model with a longer
//! context window shows a *1M context* pill in the picker's footer; on is
//! the longer window, off the model's own.

use koru_i18n::{Key, Lang, fill, number, t};

/// A window's size as the pill names it: *1M*, or *200k*.
pub fn window_label(lang: Lang, tokens: u32) -> String {
    let tokens = u64::from(tokens);
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        fill(
            t(lang, Key::AiModelContextMillions),
            &[&number::decimal(lang, tokens / 1_000_000)],
        )
    } else {
        fill(
            t(lang, Key::AiModelContextThousands),
            &[&number::decimal(lang, tokens / 1_000)],
        )
    }
}

/// The pill's words: *1M context*.
pub fn pill_label(lang: Lang, tokens: u32) -> String {
    fill(
        t(lang, Key::AiModelContextPill),
        &[&window_label(lang, tokens)],
    )
}
