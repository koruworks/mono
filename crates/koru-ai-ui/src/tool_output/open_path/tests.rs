// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **Text and source open; anything a double-click runs never does** (D62):
/// the scripts and programs an agent writes on Windows, a web page, an
/// unknown extension and a file with none are all shown in their folder.
#[test]
fn only_text_and_source_open_in_an_editor() {
    for path in [
        "src/lib.rs",
        "README.md",
        "notes.txt",
        "Cargo.toml",
        "package.json",
        "ci.yaml",
        "ci.yml",
        "app.ts",
        "view.tsx",
        "main.go",
        "MAIN.RS",
        "Notes.Md",
    ] {
        assert!(opens_in_editor(Path::new(path)), "{path} opens");
    }
    for path in [
        "build.bat",
        "build.cmd",
        "setup.ps1",
        "tool.js",
        "tool.jse",
        "run.vbs",
        "run.vbe",
        "job.wsf",
        "job.wsh",
        "koru.exe",
        "old.com",
        "setup.msi",
        "Koru.lnk",
        "app.hta",
        "saver.scr",
        "script.py",
        "script.pyw",
        "script.sh",
        "app.jar",
        "keys.reg",
        "panel.cpl",
        "page.html",
        "logo.svg",
        "Cargo.lock",
        "Makefile",
        ".gitignore",
        "TOOL.BAT",
    ] {
        assert!(!opens_in_editor(Path::new(path)), "{path} is only revealed");
    }
}

/// A relative path is the agent's: under the chat's working folder, else
/// the home folder; an absolute one is kept as it is.
#[test]
fn a_relative_path_is_read_against_the_chats_folder() {
    let home = Path::new("/home/someone");
    assert_eq!(
        resolve("src/a.rs", Some("/work/koru"), Some(home)),
        Some(PathBuf::from("/work/koru/src/a.rs"))
    );
    assert_eq!(
        resolve("src/a.rs", None, Some(home)),
        Some(PathBuf::from("/home/someone/src/a.rs")),
        "no folder chosen: the home folder"
    );
    assert_eq!(resolve("src/a.rs", None, None), None);
    let absolute = if cfg!(windows) {
        r"C:\work\koru\src\a.rs"
    } else {
        "/work/koru/src/a.rs"
    };
    assert_eq!(
        resolve(absolute, Some("/elsewhere"), Some(home)),
        Some(PathBuf::from(absolute))
    );
}

/// An existing source file opens; an existing script is shown in its
/// folder; a missing file shows the nearest folder that exists; nothing at
/// all on the disk does nothing.
#[test]
fn a_press_opens_reveals_or_shows_the_nearest_folder() {
    let on_disk = [
        PathBuf::from("/work/koru"),
        PathBuf::from("/work/koru/src/a.rs"),
        PathBuf::from("/work/koru/build.bat"),
    ];
    let exists = |path: &Path| on_disk.iter().any(|known| known == path);
    assert_eq!(
        action(Path::new("/work/koru/src/a.rs"), exists),
        PathAction::Open(PathBuf::from("/work/koru/src/a.rs"))
    );
    assert_eq!(
        action(Path::new("/work/koru/build.bat"), exists),
        PathAction::Reveal(PathBuf::from("/work/koru/build.bat")),
        "a script is selected in its folder, never run"
    );
    assert_eq!(
        action(Path::new("/work/koru/gone/b.rs"), exists),
        PathAction::Reveal(PathBuf::from("/work/koru")),
        "removed since: the nearest folder that is there"
    );
    assert_eq!(
        action(Path::new("/nowhere/b.rs"), |_: &Path| false),
        PathAction::Nothing
    );
}
