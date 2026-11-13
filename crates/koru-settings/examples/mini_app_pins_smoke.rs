// SPDX-License-Identifier: MPL-2.0
//! Explicit opt-in disk/process proof. Never called by the unit suite.
use koru_settings::{
    SettingsPaths,
    pins::{self, Authority, Mutation, Pin, PinId, Placement, Request, Scope},
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn scope(n: u128) -> Scope {
    Scope {
        profile: format!("11111111-1111-4111-8111-{n:012x}"),
        workspace: "22222222-2222-4222-8222-222222222222".into(),
    }
}
fn pin(n: u128) -> Pin {
    Pin {
        id: PinId(n),
        family: "33333333-3333-4333-8333-333333333333".into(),
        version: format!("44444444-4444-4444-8444-{n:012x}"),
        title: format!("Synthetic {n}"),
    }
}
fn apply(a: &Authority, s: &Scope, m: Mutation) -> Result<Vec<Pin>> {
    Ok(a.mutate(&Request {
        scope: s.clone(),
        mutation: m,
    })?)
}
struct Running(Child);
impl std::ops::Deref for Running {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for Running {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
struct ReaderStop(Arc<AtomicBool>);
impl Drop for ReaderStop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn child(root: &Path, mode: &str, n: u128) -> Result<Running> {
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(Running(
        command
            .args(["--settings-root"])
            .arg(root)
            .args(["--child", mode, &n.to_string()])
            .spawn()?,
    ))
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 && args.len() != 5 {
        return Err("usage: mini_app_pins_smoke --settings-root ABSOLUTE_FRESH_DIRECTORY".into());
    }
    if args[0] != "--settings-root" {
        return Err("explicit settings root required".into());
    }
    let root = PathBuf::from(&args[1]);
    if !root.is_absolute() {
        return Err("settings root must be absolute".into());
    }
    let paths = SettingsPaths::under_root(&root);
    let authority = Authority::new(paths.clone());
    let s = scope(1);
    if args.len() == 5 {
        if args[2] != "--child" || !root.join(".mini-app-pins-smoke").is_file() {
            return Err("child requires the parent's fresh smoke root".into());
        }
        let n = args[4].parse::<u128>()?;
        match args[3].as_str() {
            "write" => {
                for i in 0..16 {
                    let id = n * 100 + i + 1;
                    apply(&authority, &s, Mutation::Add { pin: pin(id) })?;
                    if i % 2 == 0 {
                        apply(&authority, &s, Mutation::Remove { id: PinId(id) })?;
                    }
                }
                apply(
                    &authority,
                    &s,
                    Mutation::Move {
                        id: PinId(n * 100 + 16),
                        to: Placement::First,
                    },
                )?;
            }
            "hold" => {
                let lock = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(paths.mini_app_pins_file().with_extension("json.lock"))?;
                lock.lock()?;
                std::fs::write(root.join("lock-ready"), b"ready")?;
                std::thread::sleep(pins::LOCK_WAIT + Duration::from_millis(700));
            }
            _ => return Err("unknown child mode".into()),
        }
        return Ok(());
    }
    // Exclusive directory creation is the safety boundary. A caller cannot
    // point this at pre-existing settings, even if their files look synthetic.
    std::fs::create_dir(&root)?;
    std::fs::write(
        root.join(".mini-app-pins-smoke"),
        b"disposable synthetic test root",
    )?;
    assert!(authority.list(&s)?.is_empty());
    apply(&authority, &scope(2), Mutation::Add { pin: pin(999) })?;
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = stop.clone();
    let file = paths.mini_app_pins_file();
    let _reader_guard = ReaderStop(stop.clone());
    let reader = std::thread::spawn(move || -> std::result::Result<usize, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut reads = 0;
        while !reader_stop.load(Ordering::Acquire) && Instant::now() < deadline {
            let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
            pins::File::decode(&bytes).map_err(|e| e.to_string())?;
            reads += 1;
            std::thread::yield_now();
        }
        Ok(reads)
    });
    let mut a = child(&root, "write", 1)?;
    let mut b = child(&root, "write", 2)?;
    assert!(a.wait()?.success());
    assert!(b.wait()?.success());
    stop.store(true, Ordering::Release);
    assert!(reader.join().map_err(|_| "reader panicked")?? > 0);
    assert_eq!(authority.list(&s)?.len(), 16);
    let actual = authority
        .list(&s)?
        .iter()
        .map(|p| p.id)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = (1..=2)
        .flat_map(|n| (2..=16).step_by(2).map(move |i| PinId(n * 100 + i)))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected);
    assert_eq!(authority.list(&scope(2))?.len(), 1);
    let id = PinId(116);
    let placement = Placement::Before {
        neighbor: PinId(216),
    };
    let first = apply(
        &authority,
        &s,
        Mutation::Move {
            id,
            to: placement.clone(),
        },
    )?;
    assert_eq!(
        first,
        apply(&authority, &s, Mutation::Move { id, to: placement })?
    );
    let mut holder = child(&root, "hold", 0)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("lock-ready").exists() {
        if Instant::now() > deadline {
            return Err("lock child did not become ready".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    assert!(authority.list(&s).is_err());
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(holder.wait()?.success());
    let before = std::fs::read(paths.mini_app_pins_file())?;
    koru_settings::SettingsAuthority::new(paths.clone())
        .patch_global(koru_settings::GlobalPatch {
            theme: Some(koru_settings::ThemeChoice::Dark),
            ..Default::default()
        })
        .map_err(|error| error.message().to_owned())?;
    assert_eq!(before, std::fs::read(paths.mini_app_pins_file())?);
    let second = root.join("second-machine");
    std::fs::create_dir(&second)?;
    assert!(
        Authority::new(SettingsPaths::under_root(&second))
            .list(&s)?
            .is_empty()
    );
    for (name, bytes) in [
        ("corrupt", b"broken".as_slice()),
        ("newer", br#"{"version":2,"scopes":[]}"#.as_slice()),
        (
            "unknown",
            br#"{"version":1,"scopes":[],"future":true}"#.as_slice(),
        ),
    ] {
        let fixture = root.join(name);
        std::fs::create_dir(&fixture)?;
        let paths = SettingsPaths::under_root(&fixture);
        std::fs::write(paths.mini_app_pins_file(), bytes)?;
        let a = Authority::new(paths.clone());
        assert!(apply(&a, &s, Mutation::Add { pin: pin(1) }).is_err());
        assert_eq!(bytes, std::fs::read(paths.mini_app_pins_file())?);
    }
    #[cfg(windows)]
    {
        let destination = paths.mini_app_pins_file();
        let permissions = std::fs::metadata(&destination)?.permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&destination, readonly)?;
        let result = apply(&authority, &s, Mutation::Add { pin: pin(888) });
        std::fs::set_permissions(&destination, permissions)?;
        assert!(
            result.is_err(),
            "Windows read-only destination must visibly refuse atomic replacement"
        );
        assert_eq!(before, std::fs::read(destination)?);
    }
    println!(
        "PASS: concurrent mutations, atomic readers, bounded process lock, strict preservation, settings-root/scope isolation, scalar settings writer{}",
        if cfg!(windows) {
            ", read-only failure"
        } else {
            ""
        }
    );
    println!("Disposable evidence retained under {}", root.display());
    Ok(())
}
