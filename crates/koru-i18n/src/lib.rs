// SPDX-License-Identifier: MPL-2.0

//! **The eight supported languages, and every string a person reads.**
//!
//! English, Spanish, Brazilian Portuguese, German, French, Arabic, Simplified
//! Chinese and Japanese were chosen before any of this was built
//! ([PRINCIPLES](../../../guide/contributing/PRINCIPLES.md)), and
//! `koru_store_core::fixtures::SAMPLES` already carries a title and a body in
//! each. That list is this crate's columns and the store's test fixture at
//! once, which is why neither can drift from the other by accident.
//!
//! ## A table, not a framework
//!
//! One compiled-in table: a [`Key`] per string, a [`Row`] per key, eight
//! `&'static str` in every row. No `fluent`, no `.po` files, no runtime
//! loading, no locale negotiation library.
//!
//! The completeness guarantee falls out of the type rather than out of
//! discipline: [`Row`] has eight fields, so **a row with a missing translation
//! does not compile**. There is no runtime fall-back to English inside the
//! table, and there must never be one — an English word appearing in the middle
//! of a Japanese sentence is the failure mode every string catalogue with a
//! fall-back eventually ships.
//!
//! The same idiom as `koru_workspace_host::cli`'s `FLAGS` and
//! `koru_store_api::api_catalog`'s `ROUTES`, and for the same reason: a
//! specification kept beside its implementation drifts, and the drift is
//! invisible until somebody hits it.
//!
//! ## What is stored is not what is shown
//!
//! **A profile records the person's locale; the UI draws the nearest supported
//! language.** Those are two facts, and collapsing them loses the one that
//! matters. A `ko-KR` machine stores `ko-KR` and reads English here; writing
//! `en` into the profile because there are no Korean strings yet would record
//! something untrue about a person and stay wrong on the day Korean is added.
//! [`Lang::for_system`] answers the second question only, and nothing in this
//! crate answers the first.
//!
//! ## What this does not do
//!
//! **Arabic renders, but no layout is mirrored.** parley resolves bidi within a
//! line, so the text itself is correct; the screen still reads left to right.
//! [`Lang::is_rtl`] is here so that a layout which *can* mirror knows to, and
//! so the gap is a value something could branch on rather than an omission.
//! Full mirroring is written up in `docs/deferred/`.
//!
//! **The translations have not been reviewed by native speakers.** They are
//! good-faith and consistent; the register of the Arabic and the Japanese in
//! particular wants a person before a release.

pub mod number;
pub mod socials;
pub mod store;
pub mod strings;
pub mod time;

pub use strings::{Key, ROWS, Row, t};

/// One of the eight.
///
/// A closed enum rather than a BCP 47 string, because "which strings do I draw"
/// has exactly eight answers and every one of them is compiled in. The tag is
/// available through [`Lang::tag`] for the places that need to speak BCP 47.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Lang {
    #[default]
    En,
    Es,
    PtBr,
    De,
    Fr,
    Ar,
    ZhHans,
    Ja,
}

impl Lang {
    /// Every language, in the order the picker lists them: English first
    /// because it is the fall-back, then the rest by the order they were
    /// decided in.
    pub const ALL: [Lang; 8] = [
        Lang::En,
        Lang::Es,
        Lang::PtBr,
        Lang::De,
        Lang::Fr,
        Lang::Ar,
        Lang::ZhHans,
        Lang::Ja,
    ];

    /// The BCP 47 tag, exactly as `koru_store_core::fixtures` spells it.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
            Lang::PtBr => "pt-BR",
            Lang::De => "de",
            Lang::Fr => "fr",
            Lang::Ar => "ar",
            Lang::ZhHans => "zh-Hans",
            Lang::Ja => "ja",
        }
    }

    /// The language's name **in itself**.
    ///
    /// A picker that offered "Japanese" to somebody who reads only Japanese
    /// would be a list they cannot use to leave the language they are stuck in.
    pub fn endonym(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Es => "Español",
            Lang::PtBr => "Português (Brasil)",
            Lang::De => "Deutsch",
            Lang::Fr => "Français",
            Lang::Ar => "العربية",
            Lang::ZhHans => "简体中文",
            Lang::Ja => "日本語",
        }
    }

    /// Whether this language is written right to left.
    ///
    /// Nothing mirrors a layout yet; see the module note. This exists so the
    /// day something does, it asks rather than hard-codes Arabic.
    pub fn is_rtl(self) -> bool {
        matches!(self, Lang::Ar)
    }

    /// The language for a BCP 47 tag, or `None` when none of the eight fits.
    ///
    /// Three rules, in order, and the third is the one worth knowing:
    ///
    /// 1. the whole tag, case-insensitively — `pt-br` is `pt-BR`;
    /// 2. **a script subtag, when present, must match.** `zh-Hant` is
    ///    Traditional Chinese and there are no Traditional strings, so it does
    ///    *not* quietly become Simplified — two scripts are not two dialects.
    ///    `zh-CN` names no script, so rule 3 reaches it;
    /// 3. the primary subtag — so `en-GB` is English and `pt-PT` gets the
    ///    Brazilian strings, which is closer than English by a long way.
    pub fn matching(tag: &str) -> Option<Lang> {
        let (language, script) = split(tag);
        if language.is_empty() {
            return None;
        }
        Lang::ALL.into_iter().find(|candidate| {
            let (their_language, their_script) = split(candidate.tag());
            if their_language != language {
                return false;
            }
            // An absent script on either side matches anything; two present
            // scripts must agree.
            match (script.as_deref(), their_script.as_deref()) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
        })
    }

    /// What to **draw** for a system that reports `tag`.
    ///
    /// English for an unmatched or absent tag. That is a rendering decision and
    /// says nothing about the person: what gets stored is their own tag, which
    /// this function never sees and never returns.
    pub fn for_system(tag: Option<&str>) -> Lang {
        tag.and_then(Lang::matching).unwrap_or(Lang::En)
    }

    /// This language's column, for [`strings::t`].
    fn column(self) -> usize {
        self as usize
    }
}

/// A tag's language and script subtags, lowercased.
///
/// Deliberately not a BCP 47 parser: everything after the script — region,
/// variants, extensions — is irrelevant to which of eight columns to read, and
/// a real parser would be a dependency this crate exists not to have.
/// Underscores are accepted because POSIX `$LANG` uses them (`ja_JP.UTF-8`),
/// and everything from the first `.` or `@` is dropped for the same reason.
fn split(tag: &str) -> (String, Option<String>) {
    let cleaned = tag
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-")
        .to_ascii_lowercase();
    let mut parts = cleaned.split('-').filter(|p| !p.is_empty());
    let language = parts.next().unwrap_or("").to_string();
    // A script subtag is four letters — `Hans`, `Latn`, `Cyrl`. A two-letter or
    // three-digit part in the same position is a region and is not one.
    let script = parts
        .next()
        .filter(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string);
    (language, script)
}

/// Fill `{}` placeholders in order.
///
/// **Placeholders, never concatenation.** "Good to see you, Steven." puts the
/// name last in English and Japanese puts it first; a string built by joining
/// pieces can only be right in the language it was written for.
///
/// Extra placeholders are left as they are and extra values are dropped, so a
/// mismatched call renders something legible rather than panicking in front of
/// a person. The test that every row's placeholder count matches English is
/// what actually catches it.
pub fn fill(template: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for value in values {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(value);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// The process's one display language, as an index into [`Lang::ALL`].
static CURRENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Say which of the eight this process draws in. Resolved once at startup
/// from the OS locale (ADR-87: the shown language is the nearest supported
/// one) by whichever binary owns the window.
///
/// A process global rather than a parameter, for the reason the theme's font
/// roles are one: threading a `Copy` value through every draw call that says a
/// word would make the language a parameter of functions that never branch on
/// it. It lives **here** rather than in one app because a leaf surface — the
/// editor's slash menu, say — draws words too and cannot see an app's private
/// static. One owner, so two callers cannot answer differently.
pub fn set_current(lang: Lang) {
    let index = Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0);
    CURRENT.store(index as u8, std::sync::atomic::Ordering::Relaxed);
}

/// The language [`set_current`] last named — English until something does.
///
/// English is the default rather than a fall-back: a process that has not yet
/// resolved a locale has not yet been told anything about a person.
pub fn current() -> Lang {
    if let Some(lang) = SCOPED.get() {
        return lang;
    }
    Lang::ALL
        .get(CURRENT.load(std::sync::atomic::Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or(Lang::En)
}

thread_local! {
    static SCOPED: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with [`current`] answering `lang` **on this thread only**.
///
/// For a test that draws a surface in another language: flipping the process
/// global would change the words every other test in the binary is drawing at
/// that moment, because cargo runs them concurrently. Nested calls restore in
/// order, and an unwind restores too.
pub fn with_current<T>(lang: Lang, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Lang>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPED.set(self.0);
        }
    }
    let _restore = Restore(SCOPED.replace(Some(lang)));
    f()
}

#[cfg(test)]
mod tests;
