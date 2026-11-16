// SPDX-License-Identifier: MPL-2.0

//! **`read-model convert`: PaddlePaddle's official weights in, our ONNX files
//! out, and every byte checked against the table.**
//!
//! This is OCR Plan 0.1's `scripts/dev/read-model/convert.sh`, absorbed. The
//! script held the weights' pins in a bash array; they are now
//! [`super::table::MODEL`]'s, so there is one copy of each pin:
//!
//! - the six upstream files — `table.rs`, fetched and verified here in Rust,
//!   through the same streamer `voice-models` downloads with;
//! - the conversion's tools — `crates/koru-release/read-model/requirements.lock`,
//!   installed with `pip install --require-hashes --no-deps` into a throwaway
//!   virtual environment under the work directory;
//! - the three outputs — `table.rs`, checked after the conversion, and an
//!   output that differs is **refused**.
//!
//! What stays Python is what has to: paddle2onnx is a Python package, and the
//! recognizer's alphabet sits inside a YAML file that `extract_dict.py` reads
//! with the PyYAML the lock already installs (reading it in Rust would add a
//! YAML crate to the workspace for one list).
//!
//! ## Windows x64 only, and why that is kept
//!
//! paddle2onnx 2.1.0's Windows extension imports about 110 `pir::` symbols that
//! no released PaddlePaddle exports; one nightly (3.0.0.dev20250426, from
//! PaddlePaddle's own wheel host) does, and the lock pins exactly that wheel,
//! which exists only for Windows x64 (OCR QUESTIONS Q10). Converting on Linux
//! was never tried and would be a different, unmeasured toolchain. So on any
//! other platform this verb says so and stops, before it creates anything.
//! Nothing else here needs it: `package` and `publish` work wherever
//! `voice-models` does, from files already converted and pinned.
//!
//! ## Determinism
//!
//! Two runs from `--clean` produce byte-identical files (measured in Plan 0.1
//! and again in Plan 2.2). That is what lets the table pin the outputs at all:
//! if a run ever disagrees with the pins, the refusal below is the evidence,
//! and the published archive — whose digest is also pinned — is still the
//! model every build installs.

use super::archive::verify_output;
use super::table::{ReadModel, is_digest};
use crate::voice::{download_and_hash, repack};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The lock and the dictionary script, beside this crate's sources.
const LOCK: &str = "crates/koru-release/read-model/requirements.lock";
const EXTRACT_DICT: &str = "crates/koru-release/read-model/extract_dict.py";

/// paddle2onnx's defaults, written out, exactly as Plan 0.1 ran them: opset 9
/// raised automatically to the lowest the graph needs (11 for both models),
/// the ONNX checker on, and polygraphy's constant folding.
const PADDLE2ONNX_FLAGS: &[&str] = &[
    "--model_filename",
    "inference.json",
    "--params_filename",
    "inference.pdiparams",
    "--opset_version",
    "9",
    "--enable_auto_update_opset",
    "True",
    "--enable_onnx_checker",
    "True",
    "--optimize_tool",
    "polygraphy",
];

/// **Whether this machine can run the conversion**, as a sentence when not.
///
/// Compile-time, because `koru-release` is built for the machine it runs on
/// and the answer is a property of the lock's wheels, not of the moment.
pub fn supported_here() -> Result<(), String> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        return Ok(());
    }
    Err(format!(
        "`read-model convert` runs only on Windows x64, and this is {}-{}. The \
         conversion's lock (crates/koru-release/read-model/requirements.lock) pins \
         paddle2onnx 2.1.0 against the one PaddlePaddle nightly whose Windows build it \
         loads with, and that wheel exists only for Windows x64 (OCR QUESTIONS Q10). \
         Nothing was created. Packaging and publishing do not need it: \
         `read-model package` and `read-model publish` work here from files already \
         converted and pinned.",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

/// Where each piece of a conversion lives under the work directory.
pub struct Layout {
    pub venv: PathBuf,
    pub weights: PathBuf,
    pub models: PathBuf,
}

impl Layout {
    pub fn under(work: &Path) -> Layout {
        Layout {
            venv: work.join("venv"),
            weights: work.join("weights"),
            models: work.join("models"),
        }
    }
}

/// How the base Python is started: `py -3.12` unless `--python` named one.
pub struct Python {
    pub program: String,
    pub args: Vec<String>,
}

impl Python {
    pub fn launcher() -> Python {
        Python {
            program: "py".into(),
            args: vec!["-3.12".into()],
        }
    }
}

/// **Run the conversion and check what it wrote.**
pub fn run(
    root: &Path,
    model: &ReadModel,
    work: &Path,
    clean: bool,
    python: &Python,
) -> Result<(), String> {
    supported_here()?;
    let lock = root.join(LOCK);
    let extract_dict = root.join(EXTRACT_DICT);
    for path in [&lock, &extract_dict] {
        if !path.is_file() {
            return Err(format!("{} is missing from this checkout", path.display()));
        }
    }
    let layout = Layout::under(work);
    println!("[Koru] text-recognition model — convert");
    println!("       work directory: {}", work.display());
    if clean {
        println!("       --clean: removing the environment, the weights and the outputs");
        for dir in [&layout.venv, &layout.weights, &layout.models] {
            if dir.exists() {
                std::fs::remove_dir_all(dir)
                    .map_err(|error| format!("removing {}: {error}", dir.display()))?;
            }
        }
    }
    std::fs::create_dir_all(&layout.models).map_err(|error| error.to_string())?;

    // 1. The environment: a venv from the lock and nothing else.
    let venv_python = layout.venv.join("Scripts").join("python.exe");
    if !venv_python.is_file() {
        println!(
            "\n  creating {} with {}",
            layout.venv.display(),
            python.program
        );
        run_checked(
            Command::new(&python.program)
                .args(&python.args)
                .args(["-m", "venv"])
                .arg(&layout.venv),
            "creating the conversion's Python environment (Python 3.12 is needed; \
             pass --python <python.exe> if `py -3.12` does not find it)",
        )?;
    }
    println!("\n  installing the pinned tools (requirements.lock, hashes required)");
    run_checked(
        Command::new(&venv_python)
            .args(["-I", "-m", "pip", "install", "--quiet", "--require-hashes"])
            .args(["--no-deps", "--only-binary", ":all:", "-r"])
            .arg(&lock)
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
        "installing requirements.lock",
    )?;
    run_checked(
        Command::new(&venv_python).args(["-I", "-m", "pip", "check"]),
        "checking the installed tools are consistent",
    )?;

    // 2. The official weights, verified into place.
    println!("\n  the official weights");
    for weight in model.weights {
        let target = layout.weights.join(weight.part).join(weight.file);
        ensure_weight(&weight.url(), weight.size, weight.sha256, &target)?;
        println!(
            "    verified {}/{}  {}",
            weight.part, weight.file, weight.sha256
        );
    }

    // 3. The conversion. Outputs are removed first, so a file left by an
    // earlier run can never be mistaken for this run's.
    for output in model.outputs {
        let path = layout.models.join(output.name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        }
    }
    for part in ["det", "rec"] {
        println!("\n  converting {part}");
        run_checked(
            Command::new(&venv_python)
                .args(["-I", "-m", "paddle2onnx.command", "--model_dir"])
                .arg(layout.weights.join(part))
                .args(PADDLE2ONNX_FLAGS)
                .arg("--save_file")
                .arg(layout.models.join(format!("{part}.onnx")))
                // polygraphy pip-installs any module it imports and cannot
                // find, and paddle2onnx turns that on inside its own process
                // (it sets POLYGRAPHY_AUTOINSTALL_DEPS=1 before folding), so
                // the variable alone cannot stop it. The lock pins every
                // module the fold imports, so nothing is missing; and with no
                // index, a future attempt to install one fails instead of
                // fetching whatever is newest. A failed fold writes an
                // unfolded graph, which the table's digests then refuse.
                // Neither is a PYTHON* variable, so `-I` does not hide them.
                .env("POLYGRAPHY_AUTOINSTALL_DEPS", "0")
                .env("PIP_NO_INDEX", "1"),
            &format!("converting {part} with paddle2onnx"),
        )?;
    }
    println!("\n  writing the recognizer's character list");
    run_checked(
        Command::new(&venv_python)
            .arg("-I")
            .arg(&extract_dict)
            .arg(layout.weights.join("rec").join("inference.yml"))
            .arg(layout.models.join("rec_dict.txt")),
        "extracting rec_dict.txt",
    )?;

    // 4. Nothing outside the lock took part. Checked after the conversion,
    // because the conversion is what used to install things.
    let installed = Command::new(&venv_python)
        .args(["-I", "-m", "pip", "list", "--format=freeze"])
        .output()
        .map_err(|error| format!("listing the environment: {error}"))?;
    if !installed.status.success() {
        return Err(format!(
            "listing the environment failed ({})",
            installed.status
        ));
    }
    let lock_text = std::fs::read_to_string(&lock).map_err(|error| error.to_string())?;
    let strays = unlocked(&lock_text, &String::from_utf8_lossy(&installed.stdout));
    if !strays.is_empty() {
        return Err(format!(
            "the conversion's environment holds packages requirements.lock does not pin: {}. \
             Something installed them during the run; pin them in the lock (with every \
             file's hash) and convert again from --clean.",
            strays.join(", ")
        ));
    }
    println!("\n  the environment holds exactly the locked packages");

    // 5. Record, and refuse what does not match.
    record(model, &layout.models)
}

/// The packages in `pip list --format=freeze` output that the lock does not
/// name, by normalised name (PEP 503: case-folded, runs of `-`, `_`, `.` as
/// one `-`). `pip` itself is the venv's own and is not in the lock.
pub fn unlocked(lock: &str, freeze: &str) -> Vec<String> {
    let names = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| line.starts_with(|c: char| c.is_ascii_alphanumeric()))
            .filter_map(|line| line.split(['=', ' ', '@']).next())
            .map(normalise)
            .collect()
    };
    let locked = names(lock);
    names(freeze)
        .into_iter()
        .filter(|name| name != "pip" && !locked.contains(name))
        .collect()
}

fn normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// **Print every output's measurement, write `SHA256SUMS`, and refuse an
/// output that differs from its pin.**
///
/// Every output is measured and printed before any is refused, so one run
/// shows the whole picture; an unpinned row prints the line to paste.
pub fn record(model: &ReadModel, models: &Path) -> Result<(), String> {
    println!("\n[Koru] outputs (sha256, bytes, file):");
    let mut sums = String::new();
    let mut refused = Vec::new();
    let mut unpinned = Vec::new();
    for output in model.outputs {
        let path = models.join(output.name);
        let (size, digest) = repack::hash_file(&path)?;
        sums.push_str(&format!("{digest}  {}\n", output.name));
        let verdict = if !is_digest(output.sha256) {
            unpinned.push((output.name, size, digest.clone()));
            "UNPINNED"
        } else if verify_output(output.name, output.size, output.sha256, &path).is_ok() {
            "matches the table"
        } else {
            refused.push(output.name);
            "DIFFERS FROM THE TABLE"
        };
        println!("  {digest}  {size:>10}  {}  {verdict}", output.name);
    }
    std::fs::write(models.join("SHA256SUMS"), sums).map_err(|error| error.to_string())?;
    if !unpinned.is_empty() {
        println!("\n[Koru] paste into read_model/table.rs, after a second clean run agrees:");
        for (name, size, digest) in &unpinned {
            println!(
                "    ConvertedFile {{ name: \"{name}\", size: {size}, sha256: \"{digest}\" }},"
            );
        }
    }
    if !refused.is_empty() {
        return Err(format!(
            "{} differ from the digests pinned in read_model/table.rs, and are refused. \
             The weights were verified, so the converter or its environment produced \
             different bytes; a changed model needs a new version in the table, not a \
             new digest under the old one.",
            refused.join(", ")
        ));
    }
    if unpinned.is_empty() {
        println!("\n[Koru] every output matches its pin. Next: `koru-release read-model package`.");
    }
    Ok(())
}

/// The verified weight at `target`, downloading it only if it is not already
/// there. A file already there that does not match is refused and left
/// alone, voice's rule: somebody put it there, and this tool is not the thing
/// that decides they were wrong.
pub fn ensure_weight(url: &str, size: u64, sha256: &str, target: &Path) -> Result<(), String> {
    if target.is_file() {
        let (have_size, have) = repack::hash_file(target)?;
        if have_size == size && have == sha256 {
            return Ok(());
        }
        return Err(format!(
            "{} is already here but is {have_size} bytes, sha256 {have}, not the pinned \
             {size} bytes, sha256 {sha256}. Move it aside (or run with --clean) rather than \
             letting this overwrite it.",
            target.display()
        ));
    }
    // Verified into place, never in place: the bytes land under a name the
    // next run does not trust, and take the real name only once they match.
    let name = target
        .file_name()
        .ok_or_else(|| format!("{} has no file name", target.display()))?
        .to_string_lossy()
        .into_owned();
    let unverified = target.with_file_name(format!("{name}.unverified"));
    let (have_size, have) = download_and_hash(url, &unverified, true)?;
    if have_size != size || have != sha256 {
        let _ = std::fs::remove_file(&unverified);
        return Err(format!(
            "{url} downloaded as {have_size} bytes, sha256 {have}, but the table pins {size} \
             bytes, sha256 {sha256}. Nothing was kept."
        ));
    }
    std::fs::rename(&unverified, target).map_err(|error| error.to_string())
}

/// Run a child with inherited output, and turn a failure into a sentence
/// naming the step.
fn run_checked(command: &mut Command, step: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("{step}: could not start: {error}"))?;
    if !status.success() {
        return Err(format!("{step} failed ({status})"));
    }
    Ok(())
}
