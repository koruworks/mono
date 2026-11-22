// SPDX-License-Identifier: MPL-2.0

//! **One run over a corpus**, and what it adds up to.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use koru_read::{Event, FailureCode, JobId, Source, Want};
use koru_read_worker::budget::{ABANDONED_CEILING, Abandoned, MAX_ABANDONED_ALIVE};
use koru_read_worker::job::{self, Engine, ReadJob};
use koru_read_worker::recognition::Recognition;

use crate::cli::Options;
use crate::corpus::{self, Kind};
use crate::memory;
use crate::metrics::{self, Errors};

/// Where a page's time is counted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    TextLayer,
    Recognized,
    Mixed,
    Picture,
    Plain,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::TextLayer => "text layer",
            Category::Recognized => "scanned, recognized",
            Category::Mixed => "text and pictures (mixed)",
            Category::Picture => "pictures",
            Category::Plain => "plain text",
        }
    }

    /// A page of a file of `kind` whose words came from `source`.
    pub fn of(kind: Kind, source: Source) -> Category {
        match (kind, source) {
            (Kind::Picture, _) => Category::Picture,
            (_, Source::Plain) => Category::Plain,
            (_, Source::Recognized) => Category::Recognized,
            (_, Source::Mixed) => Category::Mixed,
            (_, Source::TextLayer) => Category::TextLayer,
        }
    }
}

/// Every page's time, by category, and every file's end.
#[derive(Default, Debug)]
pub struct Totals {
    pub millis: BTreeMap<Category, Vec<f64>>,
    pub ends: BTreeMap<String, usize>,
    pub files: BTreeMap<Kind, usize>,
}

impl Totals {
    pub fn page(&mut self, category: Category, millis: f64) {
        self.millis.entry(category).or_default().push(millis);
    }

    pub fn end(&mut self, kind: Kind, end: &Event) {
        *self.files.entry(kind).or_default() += 1;
        *self.ends.entry(end_label(end)).or_default() += 1;
    }
}

/// `read`, `partial/page-cap`, `failed/password`, `local: unsupported`.
pub fn end_label(end: &Event) -> String {
    match end {
        Event::Done { status, reason, .. } => match reason {
            Some(reason) => format!("{}/{}", status.as_str(), reason.as_str()),
            None => status.as_str().to_string(),
        },
        Event::Failed { code, .. } => format!("local: {}", code.as_str()),
        Event::Page { .. } | Event::Status(_) => "unexpected".into(),
    }
}

/// `(p50, p99, max)` of `values`, which need not be sorted.
pub fn spread(values: &[f64]) -> Option<(f64, f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    Some((at(0.5), at(0.99), sorted[sorted.len() - 1]))
}

struct PageRead {
    number: u32,
    text: String,
}

fn read_truth(path: &Path) -> Result<HashMap<String, Vec<String>>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("the truth file cannot be read: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("the truth file is not {{\"name\": [\"page\", ...]}}: {error}"))
}

/// **Run the bench.** Reads the disk; prints the report.
pub fn run(options: &Options) -> Result<(), String> {
    let files = corpus::walk(&options.corpus)
        .map_err(|error| format!("the corpus cannot be read: {error}"))?;
    let truth = options.truth.as_deref().map(read_truth).transpose()?;
    let named = truth.is_some();
    let settings = koru_read_paddle::Settings::with_threads(options.threads);
    let want = if options.model.is_some() {
        if !koru_read_paddle::RECOGNITION_BUILT {
            return Err(
                "this bench was built without recognition; build it with `--features recognition` \
                 and ORT_LIB_PATH set (scripts/setup/fetch-voice-runtime)"
                    .into(),
            );
        }
        Want::Recognize
    } else {
        Want::Text
    };
    let recognition = Recognition::product(options.model.clone(), settings);
    if want == Want::Recognize && !recognition.available() {
        return Err("the model directory does not hold a usable model".into());
    }
    let abandoned = Abandoned::new(MAX_ABANDONED_ALIVE, ABANDONED_CEILING, |why| {
        eprintln!("koru-read-bench: abandoned work is still running ({why:?})");
    });
    let engine = Engine::product(abandoned).with_recognition(recognition);

    println!(
        "koru-read-bench: {} files, recognition {}, {} threads, reading cap {}",
        files.len(),
        if want == Want::Recognize { "on" } else { "off" },
        settings.threads(),
        options
            .max_read_pages
            .map_or("none".to_string(), |cap| cap.to_string())
    );
    let mut json = serde_json::Map::new();
    if want == Want::Recognize {
        let before = memory::read();
        let started = Instant::now();
        let reader = engine
            .recognition
            .take()
            .map_err(|code| format!("the model did not load ({})", code.as_str()))?;
        let load_ms = started.elapsed().as_secs_f64() * 1000.0;
        engine.recognition.give_back(reader);
        let after = memory::read();
        let added = match (before, after) {
            (Some((was, _, measure)), Some((now, _, _)))
                if measure == memory::Measure::PrivateCommitted =>
            {
                format!(
                    ", {} {} added",
                    memory::mib(now.saturating_sub(was)),
                    measure.label()
                )
            }
            _ => String::new(),
        };
        println!("model load: {load_ms:.0} ms{added}");
        json.insert("model_load_ms".into(), load_ms.into());
    }

    let cancel = AtomicBool::new(false);
    let mut totals = Totals::default();
    let mut errors_total = Errors::default();
    let mut recall_total = (0usize, 0usize);
    let mut file_rows = Vec::new();
    let run_started = Instant::now();
    for (index, path) in files.iter().enumerate() {
        let Some((kind, mime)) = corpus::kind_of(path) else {
            continue;
        };
        let request = ReadJob {
            job: JobId(index as u64),
            path: path.clone(),
            mime: mime.to_string(),
            from_page: 1,
            max_text_pages: Some(koru_read::TEXT_PAGE_CAP),
            max_read_pages: options.max_read_pages,
            want,
        };
        let mut pages: Vec<PageRead> = Vec::new();
        let mut last = Instant::now();
        let end = job::run(&engine, &request, &cancel, &mut |event| {
            if let Event::Page {
                page, text, source, ..
            } = event
            {
                let now = Instant::now();
                totals.page(
                    Category::of(kind, source),
                    (now - last).as_secs_f64() * 1000.0,
                );
                last = now;
                pages.push(PageRead {
                    number: page,
                    text: text.into_string(),
                });
            }
            true
        });
        totals.end(kind, &end);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(expected) = truth.as_ref().and_then(|truth| truth.get(&name)) {
            let mut errors = Errors::default();
            let mut recall = (0, 0);
            for (at, want_text) in expected.iter().enumerate() {
                let got = pages
                    .iter()
                    .find(|page| page.number as usize == at + 1)
                    .map_or("", |page| page.text.as_str());
                errors = errors.plus(metrics::errors(want_text, got));
                let (found, of) = metrics::recall(want_text, got);
                recall = (recall.0 + found, recall.1 + of);
            }
            errors_total = errors_total.plus(errors);
            recall_total = (recall_total.0 + recall.0, recall_total.1 + recall.1);
            file_rows.push((name, pages.len(), errors, recall, end_label(&end)));
        } else if named {
            // A named run lists every file, with nothing to compare where the
            // truth expects nothing.
            file_rows.push((
                name,
                pages.len(),
                Errors::default(),
                (0, 0),
                end_label(&end),
            ));
        }
    }
    let run_seconds = run_started.elapsed().as_secs_f64();

    println!("\npages per second (time from one page's answer to the next):");
    let mut categories = serde_json::Map::new();
    for (category, millis) in &totals.millis {
        let seconds = millis.iter().sum::<f64>() / 1000.0;
        let (p50, p99, max) = spread(millis).unwrap_or_default();
        println!(
            "  {:<28} {:>6} pages  {:>8.2} s  {:>8} pages/s   p50 {p50:.0} / p99 {p99:.0} / max {max:.0} ms",
            category.label(),
            millis.len(),
            seconds,
            metrics::per_second(millis.len(), seconds)
        );
        categories.insert(
            category.label().into(),
            serde_json::json!({
                "pages": millis.len(),
                "seconds": seconds,
                "p50_ms": p50, "p99_ms": p99, "max_ms": max,
            }),
        );
    }
    json.insert("categories".into(), categories.into());
    println!(
        "  end to end: {} files in {run_seconds:.2} s",
        totals.files.values().sum::<usize>()
    );
    println!("\noutcomes:");
    for (label, count) in &totals.ends {
        println!("  {label:<22} {count}");
    }
    json.insert(
        "outcomes".into(),
        serde_json::to_value(&totals.ends).unwrap_or_default(),
    );
    if let Some((_, peak, measure)) = memory::read() {
        println!("\npeak memory ({}): {}", measure.label(), memory::mib(peak));
        json.insert("peak_bytes".into(), peak.into());
        json.insert("peak_measure".into(), measure.label().into());
    }
    if named {
        println!("\nagainst the truth (white space ignored):");
        println!(
            "  {:<40} {:>5} {:>7} {:>8}  end",
            "file", "pages", "CER", "recall"
        );
        let mut rows = Vec::new();
        for (name, pages, errors, (found, of), end) in &file_rows {
            let recall = if *of > 0 {
                format!("{:.2}", *found as f64 / *of as f64)
            } else {
                "-".into()
            };
            println!(
                "  {name:<40} {pages:>5} {:>7} {recall:>8}  {end}",
                metrics::rate_text(errors.rate())
            );
            rows.push(serde_json::json!({
                "file": name, "pages": pages, "cer": errors.rate(),
                "recall": if *of > 0 { Some(*found as f64 / *of as f64) } else { None },
                "end": end,
            }));
        }
        println!(
            "  {:<40} {:>5} {:>7} {:>8}",
            "all files with expected text",
            "",
            metrics::rate_text(errors_total.rate()),
            if recall_total.1 > 0 {
                format!("{:.2}", recall_total.0 as f64 / recall_total.1 as f64)
            } else {
                "-".into()
            }
        );
        json.insert("files".into(), rows.into());
    }
    if let Some(path) = &options.json {
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(json))
            .map_err(|error| error.to_string())?;
        std::fs::write(path, text)
            .map_err(|error| format!("the report cannot be written: {error}"))?;
    }
    // A local failure is worth a line of its own: nothing about it is written
    // into a reading, and the worker's coordinator retries it.
    let local = totals
        .ends
        .iter()
        .filter(|(label, _)| label.starts_with("local: "))
        .map(|(_, count)| count)
        .sum::<usize>();
    if local > 0 {
        println!(
            "\n{local} files ended with a local failure, which no reading records \
             (`{}` is a reader that is not installed)",
            FailureCode::NeedsReader.as_str()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
