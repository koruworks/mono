// SPDX-License-Identifier: MPL-2.0

//! **The command line.** Every flag is in [`HELP`], and an unknown one is
//! refused rather than ignored: a mistyped `--model` would quietly measure the
//! text layer and report it as recognition.

use std::path::PathBuf;

pub const HELP: &str = "\
Usage: koru-read-bench <corpus> [options]

Reads every PDF, picture and plain-text file under <corpus> through the read
worker's own job pipeline and reports pages per second, peak memory and, with
--truth, the character error rate. Downloads nothing.

Options:
  --model <dir>          recognize, with the model in <dir> (det.onnx, rec.onnx,
                         rec_dict.txt); needs the `recognition` feature
  --truth <truth.json>   expected text, {\"<file name>\": [\"page 1\", ...]};
                         names files in the report (synthetic fixtures only)
  --threads <n>          recognition threads (default 4, as KORU_READ_THREADS)
  --max-read-pages <n>   recognize at most n pages of a PDF (default 200;
                         0 for no cap)
  --json <file>          also write the report as JSON
  --help                 print this message
";

/// What a run is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub corpus: PathBuf,
    pub model: Option<PathBuf>,
    pub truth: Option<PathBuf>,
    pub threads: usize,
    /// `None` for no cap.
    pub max_read_pages: Option<u32>,
    pub json: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Options),
    Help,
}

/// Parse the arguments after the program's name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut corpus = None;
    let mut model = None;
    let mut truth = None;
    let mut threads = koru_read_paddle::settings::DEFAULT_THREADS;
    let mut max_read_pages = Some(koru_read::READ_PAGE_CAP);
    let mut json = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("`{name}` needs a value"))
        };
        match arg.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--model" => model = Some(PathBuf::from(value("--model")?)),
            "--truth" => truth = Some(PathBuf::from(value("--truth")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--threads" => {
                threads = value("--threads")?
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("`--threads` takes a whole number above 0")?;
            }
            "--max-read-pages" => {
                let n = value("--max-read-pages")?
                    .parse::<u32>()
                    .map_err(|_| "`--max-read-pages` takes a whole number")?;
                max_read_pages = (n > 0).then_some(n);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown argument `{flag}`")),
            path => {
                if corpus.replace(PathBuf::from(path)).is_some() {
                    return Err("give one corpus".into());
                }
            }
        }
    }
    let corpus = corpus.ok_or("give the corpus to read, as a path")?;
    Ok(Parsed::Run(Options {
        corpus,
        model,
        truth,
        threads,
        max_read_pages,
        json,
    }))
}

#[cfg(test)]
mod tests;
