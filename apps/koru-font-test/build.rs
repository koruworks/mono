// SPDX-License-Identifier: MPL-2.0

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = package.parent().unwrap().parent().unwrap().to_path_buf();
    let upstream = root.join("third-party/parley-reference");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("reference");
    fs::create_dir_all(&output).unwrap();
    for name in ["main.rs", "text.rs", "access_ids.rs"] {
        let source = upstream.join(name);
        println!("cargo:rerun-if-changed={}", source.display());
        fs::copy(&source, output.join(name)).unwrap_or_else(|error| {
            panic!(
                "{}: {error}. Run koru.bat setup (or scripts/setup/fetch-third-party.sh) first.",
                source.display()
            )
        });
    }
    let patch = root.join("third-party/patches/parley-font-test.patch");
    println!("cargo:rerun-if-changed={}", patch.display());
    let result = Command::new("git")
        .current_dir(&root)
        .args([
            "apply",
            "--unsafe-paths",
            "--whitespace=nowarn",
            "--directory",
        ])
        .arg(&output)
        .arg(&patch)
        .output()
        .expect("git is required to apply the font reference patch");
    assert!(
        result.status.success(),
        "font reference patch: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    for name in [
        "coverage.rs",
        "coverage/tests.rs",
        "fonts.rs",
        "quiet.rs",
        "quiet/tests.rs",
        "zoom.rs",
        "zoom/tests.rs",
        "zoom.wgsl",
    ] {
        let source = package.join("src").join(name);
        let destination = output.join(name);
        println!("cargo:rerun-if-changed={}", source.display());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
}
