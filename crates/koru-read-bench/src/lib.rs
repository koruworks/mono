// SPDX-License-Identifier: MPL-2.0

//! **The reading benchmark** (OCR TECHNICAL §14 "What did that cost?").
//!
//! ```text
//! koru-read-bench <corpus> [--model <dir>] [--truth <truth.json>] [--threads N]
//!                          [--max-read-pages N] [--json <file>]
//! ```
//!
//! It walks `<corpus>`, reads every PDF, picture and plain-text file in it
//! through the read worker's own job pipeline (`koru_read_worker::job::run`:
//! the same acceptance, rendering, decoding, recognition and caps as the
//! shipped worker, minus its socket), and reports:
//!
//! - **pages per second**, by where a page's words came from (text layer,
//!   recognized, mixed, plain) and for pictures, from the time between one
//!   page's event and the next;
//! - **peak memory**, the operating system's own lifetime peak for this one
//!   process — private committed bytes on Windows, which is what Plan 0.1
//!   measured and what the worker's Job Object limit counts;
//! - with `--truth`, the **character error rate** and **word recall** of each
//!   file against the expected text, whitespace ignored, as Plan 0.1 reported
//!   them;
//! - with `--model`, the **model's load time**.
//!
//! With `--model` it asks for recognition (`Want::Recognize`), which needs the
//! `recognition` feature; without, the text layer only.
//!
//! ## The corpus is private
//!
//! **It downloads nothing and commits no corpus**; the corpus is a path. A
//! person's folder can be read, so without `--truth` no file is named: files
//! are numbered in an order the report does not print, and only counts, sizes
//! and timings appear. With `--truth` (a set of synthetic fixtures whose text
//! is known) each file is named, because that is the point.

pub mod cli;
pub mod corpus;
pub mod memory;
pub mod metrics;
pub mod run;

/// The whole of `main`.
pub fn main() -> std::process::ExitCode {
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Parsed::Run(options)) => options,
        Ok(cli::Parsed::Help) => {
            print!("{}", cli::HELP);
            return std::process::ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("koru-read-bench: {message}\n\n{}", cli::HELP);
            return std::process::ExitCode::from(2);
        }
    };
    match run::run(&options) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("koru-read-bench: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}
