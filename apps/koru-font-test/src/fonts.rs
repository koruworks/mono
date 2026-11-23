// SPDX-License-Identifier: MPL-2.0

//! Koru's FontChoice list and exact bundled font files, loaded once at startup.
use anyhow::{Context, Result};
use parley::{FontContext, fontique::FontInfoOverride};
use std::{path::Path, sync::Arc};
use vello::peniko::Blob;

pub struct Choice {
    pub label: &'static str,
    pub stack: &'static str,
    files: &'static [&'static str],
}

macro_rules! choice {
    ($label:literal, $stack:literal $(, $file:literal)*) => {
        Choice { label: $label, stack: $stack, files: &[$($file),*] }
    };
}

// Labels/order follow Koru's main picker, with its featured Windows face first.
// No dependency on Koru: this remains an upstream Parley/Vello comparison.
pub const CHOICES: &[Choice] = &[
    choice!("Segoe UI", "Segoe UI, system-ui, sans-serif"),
    choice!(
        "Inter",
        "Inter, system-ui, sans-serif",
        "Inter.ttf",
        "Inter-Italic.ttf"
    ),
    choice!(
        "Source Sans",
        "Source Sans 3, system-ui, sans-serif",
        "SourceSans3.ttf",
        "SourceSans3-Italic.ttf"
    ),
    choice!(
        "Jakarta",
        "Plus Jakarta Sans, system-ui, sans-serif",
        "PlusJakartaSans.ttf",
        "PlusJakartaSans-Italic.ttf"
    ),
    choice!(
        "Plex",
        "IBM Plex Sans, system-ui, sans-serif",
        "IBMPlexSans.ttf",
        "IBMPlexSans-Italic.ttf"
    ),
    choice!(
        "Poppins",
        "Poppins, system-ui, sans-serif",
        "Poppins-Regular.ttf",
        "Poppins-Italic.ttf",
        "Poppins-Medium.ttf",
        "Poppins-MediumItalic.ttf",
        "Poppins-SemiBold.ttf",
        "Poppins-SemiBoldItalic.ttf",
        "Poppins-Bold.ttf",
        "Poppins-BoldItalic.ttf"
    ),
    choice!("Manrope", "Manrope, system-ui, sans-serif", "Manrope.ttf"),
    choice!(
        "Source Serif",
        "Source Serif 4, Georgia, serif",
        "SourceSerif4.ttf",
        "SourceSerif4-Italic.ttf"
    ),
    choice!(
        "Lora",
        "Lora, Georgia, serif",
        "Lora.ttf",
        "Lora-Italic.ttf"
    ),
    choice!(
        "Nunito",
        "Nunito, system-ui, sans-serif",
        "Nunito.ttf",
        "Nunito-Italic.ttf"
    ),
    choice!(
        "JetBrains Mono",
        "JetBrains Mono, ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace",
        "JetBrainsMono.ttf",
        "JetBrainsMono-Italic.ttf"
    ),
    choice!(
        "Source Code",
        "Source Code Pro, ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace",
        "SourceCodePro.ttf",
        "SourceCodePro-Italic.ttf"
    ),
    choice!(
        "System Mono",
        "ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace"
    ),
    choice!("System", "system-ui, sans-serif"),
];

pub fn find(name: &str) -> Result<usize> {
    CHOICES
        .iter()
        .position(|font| {
            font.label.eq_ignore_ascii_case(name)
                || font
                    .stack
                    .split(',')
                    .next()
                    .unwrap()
                    .eq_ignore_ascii_case(name)
        })
        .with_context(|| format!("Unknown font {name:?}. Use --list-fonts to see choices."))
}

pub fn load(directory: &Path) -> Result<FontContext> {
    let mut context = FontContext::new();
    for font in CHOICES {
        let family = font.stack.split(',').next().unwrap();
        for file in font.files {
            let path = directory.join(file);
            let data = std::fs::read(&path).with_context(|| {
                format!(
                    "Reading {}. Supply Koru's assets/fonts directory with --fonts-dir=PATH.",
                    path.display()
                )
            })?;
            let registered = context.collection.register_fonts(
                Blob::new(Arc::new(data)),
                Some(FontInfoOverride {
                    family_name: Some(family),
                    width: None,
                    style: None,
                    weight: None,
                    axes: None,
                }),
            );
            anyhow::ensure!(
                !registered.is_empty(),
                "Could not register {}",
                path.display()
            );
        }
    }
    Ok(context)
}
