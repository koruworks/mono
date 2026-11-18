// SPDX-License-Identifier: MPL-2.0

//! Locale-aware formatting for user-visible whole numbers.
//!
//! Token counts are integers, so this deliberately covers only decimal digits
//! and thousands grouping. It does not grow the dependency-free string table
//! into a general-purpose locale framework.

use crate::strings::{Key, t};
use crate::{Lang, fill};

/// Format an unsigned decimal integer for one of Koru's supported languages.
///
/// Arabic uses Arabic-Indic digits and the Arabic thousands separator. French
/// uses a narrow no-break space; the remaining locales use their conventional
/// comma or full-stop grouping separator.
pub fn decimal(lang: Lang, value: u64) -> String {
    let ascii = value.to_string();
    let separator = match lang {
        Lang::En | Lang::ZhHans | Lang::Ja => ',',
        Lang::Es | Lang::PtBr | Lang::De => '.',
        Lang::Fr => '\u{202f}',
        Lang::Ar => '\u{066c}',
    };

    let mut out = String::with_capacity(ascii.len() + ascii.len() / 3);
    for (index, digit) in ascii.bytes().enumerate() {
        if index != 0 && (ascii.len() - index).is_multiple_of(3) {
            out.push(separator);
        }
        out.push(localize_digit(lang, digit));
    }
    out
}

/// A byte count, as a person reads one — **rough on purpose**.
///
/// *4.2 MB* answers the question a size column is asked; a digit-exact
/// count answers a different one. Binary steps (1024), because that is what
/// a filesystem reports and what the register path rule read.
///
/// The unit word comes from the table (so Arabic and French are not
/// English), the digits from [`localize_digit`], and the decimal separator
/// from the locale — a comma where a comma is the decimal mark, and never
/// the same character as the grouping separator in the same language.
pub fn bytes(lang: Lang, value: u64) -> String {
    rough(lang, value, 1024)
}

/// Cloud storage, as a plan names it: **steps of 1000**, so a 100 GB plan
/// reads *100 GB* and not *93 GB*. Rounded as [`bytes`] rounds; only the
/// step differs, because an allowance is sold in decimal gigabytes while a
/// file's size is what the filesystem reports.
pub fn storage(lang: Lang, value: u64) -> String {
    rough(lang, value, 1000)
}

fn rough(lang: Lang, value: u64, step: u64) -> String {
    const UNITS: [Key; 3] = [Key::SizeKilobytes, Key::SizeMegabytes, Key::SizeGigabytes];
    let unit_step = step as f64;
    if value < step {
        return fill(t(lang, Key::SizeBytes), &[&decimal(lang, value)]);
    }
    let mut scaled = value as f64 / unit_step;
    let mut unit = 0;
    while scaled >= unit_step && unit + 1 < UNITS.len() {
        scaled /= unit_step;
        unit += 1;
    }
    // One decimal below ten, none above: the second digit of *4.2* carries
    // information and the second digit of *420* does not.
    let number = if scaled < 10.0 {
        let tenths = (scaled * 10.0).round() as u64;
        let mut spelled = decimal(lang, tenths / 10);
        spelled.push(decimal_separator(lang));
        spelled.push(localize_digit(lang, b'0' + (tenths % 10) as u8));
        spelled
    } else {
        decimal(lang, scaled.round() as u64)
    };
    fill(t(lang, UNITS[unit]), &[&number])
}

/// The decimal mark, which is **not** the grouping separator: a language
/// that groups with `.` marks decimals with `,` and the other way round,
/// and one function using the other's character would spell *1.234* as a
/// thousand.
fn decimal_separator(lang: Lang) -> char {
    match lang {
        Lang::En | Lang::ZhHans | Lang::Ja => '.',
        Lang::Es | Lang::PtBr | Lang::De | Lang::Fr => ',',
        Lang::Ar => '\u{066b}',
    }
}

fn localize_digit(lang: Lang, ascii: u8) -> char {
    const ARABIC_INDIC: [char; 10] = ['٠', '١', '٢', '٣', '٤', '٥', '٦', '٧', '٨', '٩'];
    match lang {
        Lang::Ar => ARABIC_INDIC[usize::from(ascii - b'0')],
        _ => char::from(ascii),
    }
}

#[cfg(test)]
mod tests {
    use super::{bytes, decimal, storage};
    use crate::Lang;

    #[test]
    fn decimal_goldens_cover_every_supported_language() {
        let cases = [
            (Lang::En, "1,234,567"),
            (Lang::Es, "1.234.567"),
            (Lang::PtBr, "1.234.567"),
            (Lang::De, "1.234.567"),
            (Lang::Fr, "1\u{202f}234\u{202f}567"),
            (Lang::Ar, "١٬٢٣٤٬٥٦٧"),
            (Lang::ZhHans, "1,234,567"),
            (Lang::Ja, "1,234,567"),
        ];

        assert_eq!(cases.len(), Lang::ALL.len());
        for (lang, expected) in cases {
            assert_eq!(decimal(lang, 1_234_567), expected, "{}", lang.tag());
        }
    }

    #[test]
    fn decimal_handles_grouping_boundaries_and_zero() {
        assert_eq!(decimal(Lang::En, 0), "0");
        assert_eq!(decimal(Lang::En, 999), "999");
        assert_eq!(decimal(Lang::En, 1_000), "1,000");
        assert_eq!(decimal(Lang::Ar, 0), "٠");
        assert_eq!(decimal(Lang::Ar, 999), "٩٩٩");
        assert_eq!(decimal(Lang::Ar, 1_000), "١٬٠٠٠");
    }

    /// The unit is the language's own word, the digits are the language's
    /// own digits, and the decimal mark is never the grouping separator.
    #[test]
    fn a_size_reads_in_every_language() {
        assert_eq!(bytes(Lang::En, 0), "0 B");
        assert_eq!(bytes(Lang::En, 1_023), "1,023 B");
        assert_eq!(bytes(Lang::En, 1_024), "1.0 KB");
        assert_eq!(bytes(Lang::En, 4_404_019), "4.2 MB");
        assert_eq!(bytes(Lang::En, 1024 * 1024 * 100), "100 MB");
        assert_eq!(bytes(Lang::En, 1024u64.pow(3) * 3), "3.0 GB");
        // Above the last step it keeps scaling in that unit rather than
        // inventing one the table has no word for.
        assert_eq!(bytes(Lang::En, 1024u64.pow(4) * 2), "2,048 GB");

        assert_eq!(bytes(Lang::De, 4_404_019), "4,2 MB");
        assert_eq!(bytes(Lang::Fr, 4_404_019), "4,2 Mo");
        assert_eq!(bytes(Lang::Fr, 512), "512 o");
        assert_eq!(
            bytes(Lang::Ar, 512),
            "\u{665}\u{661}\u{662} \u{628}\u{627}\u{64a}\u{62a}"
        );
        assert!(bytes(Lang::Ar, 4_404_019).starts_with("\u{664}\u{66b}\u{662}"));
        assert_eq!(bytes(Lang::Ja, 4_404_019), "4.2 MB");
        // Every language answers something, and none of them answers with
        // a stray placeholder.
        for lang in Lang::ALL {
            for value in [0, 1, 1_023, 1_024, 4_404_019, u64::MAX] {
                let spelled = bytes(lang, value);
                assert!(!spelled.contains("{}"), "{} {spelled}", lang.tag());
                assert!(!spelled.trim().is_empty());
            }
        }
    }

    #[test]
    fn decimal_handles_u64_max() {
        assert_eq!(decimal(Lang::En, u64::MAX), "18,446,744,073,709,551,615");
        assert_eq!(decimal(Lang::Ar, u64::MAX), "١٨٬٤٤٦٬٧٤٤٬٠٧٣٬٧٠٩٬٥٥١٬٦١٥");
    }

    /// A plan's allowance reads as the plan is sold: decimal steps, so
    /// 100,000,000,000 bytes is *100 GB*, not the *93 GB* `bytes` would say.
    #[test]
    fn storage_counts_in_thousands_and_rounds_like_file_sizes() {
        assert_eq!(storage(Lang::En, 100_000_000_000), "100 GB");
        assert_eq!(storage(Lang::En, 1_000_000_000), "1.0 GB");
        assert_eq!(storage(Lang::En, 99_470_869_421), "99 GB");
        assert_eq!(storage(Lang::En, 529_130_579), "529 MB");
        assert_eq!(storage(Lang::En, 4_200_000), "4.2 MB");
        assert_eq!(storage(Lang::En, 999), "999 B");
        assert_eq!(storage(Lang::De, 4_200_000), "4,2 MB");
        assert_ne!(
            storage(Lang::En, 100_000_000_000),
            bytes(Lang::En, 100_000_000_000)
        );
    }
}
