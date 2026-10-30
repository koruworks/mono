// SPDX-License-Identifier: MPL-2.0

//! What machine is this, and how is it set up — collected once per run.
//!
//! **Split by who can answer.** The window facts ([`WindowFacts`]) come from
//! the event loop, because only it holds a `Window` and its monitors — and
//! because one platform call, the keyboard layout, may only be made from the
//! main thread, which the event loop is. The rest is platform state read on
//! a worker thread, because several of these calls touch the registry, the
//! filesystem and the network stack, and *a thread that answers a person
//! never waits on the world*. The split is the whole reason [`collect`]
//! takes the first half as an argument rather than finding it itself.
//!
//! **Once, after the first frame** — never on a timer, and never during
//! startup's critical path: what it measures is a machine, and a machine
//! does not change between two frames.
//!
//! Everything here is a dimension a product or rendering decision is read
//! against. No name, no path, no serial, no address; tokens are mapped to
//! the registry's closed vocabularies *here*, so an unrecognised value
//! becomes `other` on this machine rather than travelling as itself.

use koru_events::AppEnvironment;

/// The regional calendar convention exposed to product features. Unknown
/// platform answers remain absent rather than being mislabeled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstDayOfWeek {
    Monday,
    Saturday,
    Sunday,
}

/// The person's operating-system clock convention, for local presentation.
/// This is sampled by maintenance callers, never by draw or input handlers.
pub fn preferred_hour_cycle() -> Option<koru_i18n::time::HourCycle> {
    match platform::clock()? {
        "h12" => Some(koru_i18n::time::HourCycle::H12),
        "h24" => Some(koru_i18n::time::HourCycle::H24),
        _ => None,
    }
}

/// The person's operating-system first-day-of-week convention. This reuses
/// the same platform probe as environment telemetry instead of inventing a
/// second locale reader in reminder code.
pub fn preferred_first_day_of_week() -> Option<FirstDayOfWeek> {
    match platform::first_day_of_week()? {
        "monday" => Some(FirstDayOfWeek::Monday),
        "saturday" => Some(FirstDayOfWeek::Saturday),
        "sunday" => Some(FirstDayOfWeek::Sunday),
        _ => None,
    }
}

/// What only the event loop can answer, gathered on it and handed over.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowFacts {
    pub monitor_count: Option<u64>,
    pub display_width: Option<u64>,
    pub display_height: Option<u64>,
    pub display_scale_percent: Option<u64>,
    pub window_width: Option<u64>,
    pub window_height: Option<u64>,
    pub dark: bool,
    /// From `keyboard_layout`, which has to run on the main thread.
    pub keyboard_layout: Option<&'static str>,
}

/// The machine, as one event's worth of dimensions. Runs on a worker, so
/// nothing it reads may need the main thread: a reader that does travels in
/// [`WindowFacts`] instead.
pub fn collect(window: WindowFacts) -> AppEnvironment {
    let system_tag = koru_window::system_locale::system_locale();
    AppEnvironment {
        os: os_token(),
        os_version: os_version(),
        app_build: koru_channel::build_info().build_number,
        // The person's own tag never leaves the machine: it is matched to a
        // supported language here, and only the answer is carried.
        system_language: language_token(system_tag.as_deref()),
        app_language: language_token(system_tag.as_deref()),
        utc_offset_minutes: utc_offset_minutes(),
        display_width: window.display_width,
        display_height: window.display_height,
        display_scale_percent: window.display_scale_percent,
        monitor_count: window.monitor_count,
        window_width: window.window_width,
        window_height: window.window_height,
        ram_gb_bucket: ram_gb().map(koru_events::bucket),
        disk_free_gb_bucket: disk_free_gb().map(koru_events::bucket),
        keyboard_layout: window.keyboard_layout,
        cpu_arch: cpu_arch_token(),
        cpu_count: std::thread::available_parallelism()
            .ok()
            .map(|count| count.get() as u64),
        gpu_vendor: gpu().map(|(vendor, _)| vendor),
        gpu_backend: gpu().map(|(_, backend)| backend),
        touch: touch(),
        theme: Some(if window.dark { "dark" } else { "light" }),
        reduced_motion: reduced_motion(),
        high_contrast: high_contrast(),
        text_scale_percent: text_scale_percent(),
        first_day_of_week: first_day_of_week(),
        clock: clock(),
        network: network(),
    }
}

fn os_token() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        _ => "other",
    }
}

fn cpu_arch_token() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => "other",
    }
}

/// A BCP-47 tag matched to a supported language, or `other`. The mapping is
/// `koru-i18n`'s, so this cannot drift from what the app actually draws in.
fn language_token(tag: Option<&str>) -> &'static str {
    use koru_i18n::Lang;
    match tag.and_then(Lang::matching) {
        Some(Lang::En) => "en",
        Some(Lang::Es) => "es",
        Some(Lang::PtBr) => "pt_br",
        Some(Lang::De) => "de",
        Some(Lang::Fr) => "fr",
        Some(Lang::Ar) => "ar",
        Some(Lang::ZhHans) => "zh_hans",
        Some(Lang::Ja) => "ja",
        None => "other",
    }
}

/// The offset the machine is at *now*, in minutes — chrono's, so daylight
/// saving is accounted for rather than assumed.
fn utc_offset_minutes() -> Option<i64> {
    use chrono::Offset as _;
    Some(i64::from(
        chrono::Local::now().offset().fix().local_minus_utc() / 60,
    ))
}

/// The vendor and backend of the adapter that is actually drawing, recorded
/// by `koru-window` when the device was created — asked rather than
/// re-derived, because creating a second adapter to ask would be absurd.
fn gpu() -> Option<(&'static str, &'static str)> {
    let (name, backend) = koru_window::renderer::adapter_description()?;
    let name = name.to_ascii_lowercase();
    let vendor = if name.contains("nvidia") {
        "nvidia"
    } else if name.contains("amd") || name.contains("radeon") {
        "amd"
    } else if name.contains("intel") {
        "intel"
    } else if name.contains("apple") {
        "apple"
    } else if name.contains("microsoft") || name.contains("basic render") {
        "microsoft"
    } else {
        "other"
    };
    let backend = match backend.to_ascii_lowercase().as_str() {
        "vulkan" => "vulkan",
        "dx12" => "dx12",
        "metal" => "metal",
        "gl" => "gl",
        "browserwebgpu" => "browser_webgpu",
        _ => "other",
    };
    Some((vendor, backend))
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::MAX_PATH;
    use windows_sys::Win32::Globalization::GetLocaleInfoEx;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::System::SystemInformation::{
        GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SM_DIGITIZER, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SystemParametersInfoW,
    };

    /// `RtlGetVersion` rather than `GetVersionEx`: the latter lies about
    /// anything past Windows 8 unless the executable carries a manifest
    /// claiming otherwise, and a lie is worse than an absence in a metric.
    pub fn os_version() -> Option<String> {
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
        }
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        if unsafe { RtlGetVersion(&mut info) } != 0 {
            return None;
        }
        Some(format!(
            "{}.{}.{}",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        ))
    }

    pub fn ram_gb() -> Option<u64> {
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            return None;
        }
        Some(status.ullTotalPhys / (1024 * 1024 * 1024))
    }

    /// Free space on the volume holding this channel's data — the one an
    /// index, an update or a snapshot actually needs.
    pub fn disk_free_gb(path: &std::path::Path) -> Option<u64> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then(|| free / (1024 * 1024 * 1024))
    }

    /// The layout *family*, from the active layout's primary language — what
    /// a shortcut decision needs. The identifier itself is not carried.
    ///
    /// `GetKeyboardLayout(0)` answers for the *calling* thread, and an input
    /// locale is per thread: the UI thread's follows what the person switches
    /// to, and a worker never sees that switch. So this is asked from the
    /// event loop (`App::window_facts`), the thread the window types on.
    pub fn keyboard_layout() -> Option<&'static str> {
        let layout = unsafe { GetKeyboardLayout(0) } as usize as u32;
        let primary = (layout & 0x3ff) as u16;
        Some(match primary {
            // French, Belgian.
            0x0c => "azerty",
            // German, and the Central-European group that shares its shape.
            0x07 | 0x05 | 0x0e | 0x18 | 0x1a | 0x24 => "qwertz",
            0x09 | 0x0a | 0x10 | 0x13 | 0x16 | 0x1d | 0x11 | 0x12 | 0x04 => "qwerty",
            _ => "other",
        })
    }

    pub fn touch() -> Option<bool> {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
        // The digitizer bits: any of them means a touch or pen device is
        // present, which is what the question is asking.
        Some(unsafe { GetSystemMetrics(SM_DIGITIZER) } != 0)
    }

    pub fn reduced_motion() -> Option<bool> {
        let mut animations: i32 = 1;
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animations as *mut i32).cast(),
                0,
            )
        };
        // The OS reports whether animation is *on*; reduced motion is its
        // negation, which is the way round a caller wants to read it.
        (ok != 0).then_some(animations == 0)
    }

    pub fn high_contrast() -> Option<bool> {
        #[repr(C)]
        struct HighContrast {
            size: u32,
            flags: u32,
            scheme: *mut u16,
        }
        const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;
        let mut contrast = HighContrast {
            size: std::mem::size_of::<HighContrast>() as u32,
            flags: 0,
            scheme: std::ptr::null_mut(),
        };
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                contrast.size,
                (&mut contrast as *mut HighContrast).cast(),
                0,
            )
        };
        (ok != 0).then_some(contrast.flags & HCF_HIGHCONTRASTON != 0)
    }

    /// The accessibility text scale, from the value the OS writes for it.
    /// Absent rather than 100 when nothing is set: absence says "not
    /// determined", and a default would claim it was measured.
    pub fn text_scale_percent() -> Option<u64> {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW, RegOpenKeyExW,
        };
        let path: Vec<u16> = "Software\\Microsoft\\Accessibility\0"
            .encode_utf16()
            .collect();
        let name: Vec<u16> = "TextScaleFactor\0".encode_utf16().collect();
        let mut key = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
            return None;
        }
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let read = unsafe {
            RegGetValueW(
                key,
                std::ptr::null(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        };
        unsafe { RegCloseKey(key) };
        (read == 0).then_some(u64::from(value))
    }

    fn locale_number(what: u32) -> Option<u32> {
        let mut buffer = [0u16; 16];
        let read = unsafe {
            GetLocaleInfoEx(
                std::ptr::null(),
                what,
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        };
        if read <= 0 {
            return None;
        }
        String::from_utf16_lossy(&buffer[..(read as usize - 1)])
            .trim()
            .parse()
            .ok()
    }

    pub fn first_day_of_week() -> Option<&'static str> {
        const LOCALE_IFIRSTDAYOFWEEK: u32 = 0x0000_100C;
        // Windows counts from Monday as 0; the tokens name the day.
        Some(match locale_number(LOCALE_IFIRSTDAYOFWEEK)? {
            0 => "monday",
            5 => "saturday",
            6 => "sunday",
            _ => "other",
        })
    }

    pub fn clock() -> Option<&'static str> {
        const LOCALE_ITIME: u32 = 0x0000_0023;
        Some(match locale_number(LOCALE_ITIME)? {
            1 => "h24",
            _ => "h12",
        })
    }

    /// Whether the machine believed it had connectivity. `wininet`'s answer
    /// is a belief rather than a probe, which is exactly right here: nothing
    /// may go near the network to record that the network was there.
    pub fn network() -> Option<&'static str> {
        #[link(name = "wininet")]
        unsafe extern "system" {
            fn InternetGetConnectedState(flags: *mut u32, reserved: u32) -> i32;
        }
        let mut flags = 0u32;
        let connected = unsafe { InternetGetConnectedState(&mut flags, 0) };
        Some(if connected != 0 { "online" } else { "offline" })
    }

    use std::os::windows::ffi::OsStrExt as _;
    const _: () = {
        // `MAX_PATH` is imported for the reader's sake: every path here is a
        // store root, which is well under it.
        let _ = MAX_PATH;
    };
}

#[cfg(target_os = "macos")]
mod platform {
    //! The same eleven questions, asked the way macOS answers them.
    //!
    //! **Foundation and AppKit where there is an object, C where there is
    //! not.** `NSProcessInfo`, `NSWorkspace` and `NSCalendar` are ordinary
    //! typed calls through `objc2`; the free space, the keyboard layout and
    //! the network belief are `extern` declarations against a framework,
    //! which is the same shape the Windows arm above uses for `ntdll` and
    //! `wininet` rather than a new habit.
    //!
    //! **Ten of the eleven answer.** The one that does not is
    //! `text_scale_percent`, and it says why at its own function; `touch`
    //! answers `Some(false)`, which is a different thing and also says why.
    //! Absence means "not determined" — never zero, and never a plausible
    //! default.
    //!
    //! Nothing here reaches the network, spawns a process, or names the
    //! machine, the person or a path.

    use objc2_app_kit::NSWorkspace;
    use objc2_core_foundation::CFString;
    use objc2_foundation::{NSCalendar, NSDateFormatter, NSLocale, NSProcessInfo, NSString};
    use std::ffi::c_void;

    /// The dotted product version — `26.0.1`. Read through
    /// [`koru_window::system_version`], which is also where `koru-agent`
    /// reads it: one `sysctl`, two callers (ADR-173).
    pub fn os_version() -> Option<String> {
        koru_window::system_version::system_version().map(|version| version.product)
    }

    pub fn ram_gb() -> Option<u64> {
        Some(NSProcessInfo::processInfo().physicalMemory() / (1024 * 1024 * 1024))
    }

    /// Free space on the volume holding this channel's data, through the
    /// POSIX counterpart of the Windows arm's `GetDiskFreeSpaceExW`.
    ///
    /// `f_bavail` rather than `f_bfree`: the blocks *this* process may use,
    /// which is the number an index or an update is actually limited by.
    pub fn disk_free_gb(path: &std::path::Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: a valid C path and a zeroed `statvfs` the call fills in.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
            return None;
        }
        let free = (stats.f_bavail as u64).checked_mul(stats.f_frsize as u64)?;
        Some(free / (1024 * 1024 * 1024))
    }

    /// The layout *family* of the physical keyboard, from Text Input
    /// Services.
    ///
    /// **`TISCopyCurrentKeyboardLayoutInputSource`, not
    /// `TISCopyCurrentKeyboardInputSource`.** The second answers with the
    /// active *input source*, which for anyone typing Japanese or Chinese is
    /// an input method rather than a layout; the first answers with the
    /// layout underneath it, which is the question — where the letters are on
    /// the keys.
    ///
    /// A name the table does not know is `other`. That is deliberate and is
    /// not a gap to be filled by guessing: `Dvorak`, `Colemak` and every
    /// non-Latin layout are genuinely none of these three families.
    ///
    /// **Main thread only.** On macOS 26, `TISGetInputSourceProperty`
    /// validates the source through `islGetInputSourceListWithAdditions`,
    /// which `dispatch_assert_queue`s the main queue — anywhere else that is
    /// a `SIGTRAP`, not an error return. From the events worker it took the
    /// app down at the end of every first run: onboarding's last Enter opens
    /// the workspace, its first frame starts `collect`, and the worker's call
    /// landed while the main thread was in `-[NSWindow becomeKeyWindow]` for
    /// that new window. **Not every launch.** A relaunch straight into a
    /// workspace survived the same worker call, even brought to the front at
    /// launch, and so did a worker in a bare process — no unit test catches
    /// it, and walking onboarding on a fresh root reproduces it reliably. It
    /// is asked from `App::window_facts` now; the other readers in this module
    /// were run from the worker across a launch on 26.5 and still answer there.
    ///
    /// **Off the main thread it answers `None` without making the call.** The
    /// TIS functions are plain `extern "C"`, so nothing in their types stops
    /// a worker from calling them; the check here is what does, and absence
    /// is what this module already means by "not determined".
    pub fn keyboard_layout() -> Option<&'static str> {
        #[link(name = "Carbon", kind = "framework")]
        unsafe extern "C" {
            static kTISPropertyInputSourceID: *const c_void;
            fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
            fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *mut c_void;
        }
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFRelease(cf: *const c_void);
        }

        objc2_foundation::MainThreadMarker::new()?;
        // SAFETY: the Copy call returns a +1 reference or null, and the Get
        // call borrows from it — so the string is read before the release
        // below, and nothing escapes.
        let source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
        if source.is_null() {
            return None;
        }
        let identifier = unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceID) };
        let identifier = (!identifier.is_null())
            .then(|| unsafe { (*identifier.cast::<CFString>()).to_string() });
        unsafe { CFRelease(source) };

        Some(layout_family(identifier.as_deref()?))
    }

    /// `com.apple.keylayout.SwissGerman` becomes `qwertz`.
    ///
    /// Split out so the table is testable without a keyboard: the reader
    /// above cannot be made to answer anything in particular on the machine
    /// running the suite.
    pub(super) fn layout_family(identifier: &str) -> &'static str {
        let Some(name) = identifier.strip_prefix("com.apple.keylayout.") else {
            return "other";
        };
        // Apple ships QWERTY variants of several QWERTZ layouts under the
        // same stem, so the suffix is checked before the stem or `Czech` and
        // `Czech-QWERTY` land in the same bucket.
        if name.ends_with("-QWERTY") {
            return "qwerty";
        }
        const AZERTY: &[&str] = &["French", "French-numerical", "French-PC", "Belgian"];
        const QWERTZ: &[&str] = &[
            "German",
            "Austrian",
            "SwissGerman",
            "SwissFrench",
            "Czech",
            "Slovak",
            "Hungarian",
            "Croatian",
            "Slovenian",
            "Serbian-Latin",
            "Bosnian",
            "Albanian",
        ];
        const QWERTY: &[&str] = &[
            "ABC",
            "US",
            "USExtended",
            "US-International-PC",
            "British",
            "British-PC",
            "Irish",
            "IrishExtended",
            "Australian",
            "Canadian",
            "Canadian-CSA",
            "Spanish",
            "Spanish-ISO",
            "LatinAmerican",
            "Italian",
            "Italian-Pro",
            "Portuguese",
            "Brazilian",
            "Dutch",
            "Norwegian",
            "NorwegianExtended",
            "Danish",
            "Swedish",
            "Swedish-Pro",
            "Finnish",
            "FinnishExtended",
            "Icelandic",
            "Polish",
            "PolishPro",
            "Romanian",
            "RomanianStandard",
            "Estonian",
            "Latvian",
            "Lithuanian",
            "Maltese",
            "Turkish-QWERTY-PC",
            "Vietnamese",
        ];
        if AZERTY.contains(&name) {
            "azerty"
        } else if QWERTZ.contains(&name) {
            "qwertz"
        } else if QWERTY.contains(&name) {
            "qwerty"
        } else {
            "other"
        }
    }

    /// No Mac has a touch digitizer, and a trackpad is not one: the question
    /// is whether the *screen* takes touch, which is what the Windows arm's
    /// `SM_DIGITIZER` reports.
    ///
    /// So `Some(false)` rather than `None`. This is a fact about the
    /// platform, not an unanswered question — and reporting it as absent
    /// would make macOS unreadable in the one cohort split the attr exists
    /// for.
    pub fn touch() -> Option<bool> {
        Some(false)
    }

    pub fn reduced_motion() -> Option<bool> {
        Some(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion())
    }

    /// *Increase contrast* in Accessibility → Display. macOS has no
    /// high-contrast *scheme* the way Windows does; this is the setting a
    /// person turns on for the same reason, and it is what AppKit asks its
    /// own controls.
    pub fn high_contrast() -> Option<bool> {
        Some(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast())
    }

    /// **`None`, and it will stay `None`.** macOS has no global text scale:
    /// Dynamic Type is iOS's, and the Mac's Accessibility pane offers
    /// per-application text sizes and a display zoom rather than one factor
    /// an app can read. Returning 100 would claim a measurement nobody made.
    pub fn text_scale_percent() -> Option<u64> {
        None
    }

    /// `NSCalendar` counts weekdays from Sunday as 1, which is the opposite
    /// end from the Windows arm's zero-is-Monday.
    pub fn first_day_of_week() -> Option<&'static str> {
        Some(match NSCalendar::currentCalendar().firstWeekday() {
            1 => "sunday",
            2 => "monday",
            7 => "saturday",
            _ => "other",
        })
    }

    /// The `j` template is Unicode's "whichever hour field this locale
    /// prefers", so the answer comes back as a pattern — `h a` or `HH` — and
    /// the presence of the period field is what tells them apart. Asking
    /// this way rather than reading a preference means it follows the same
    /// rule `NSDateFormatter` itself would.
    pub fn clock() -> Option<&'static str> {
        let template = NSString::from_str("j");
        let pattern = NSDateFormatter::dateFormatFromTemplate_options_locale(
            &template,
            0,
            Some(&NSLocale::currentLocale()),
        )?
        .to_string();
        Some(if pattern.contains('a') { "h12" } else { "h24" })
    }

    /// Whether the machine believed it had a route, the way the Windows arm
    /// believes `wininet`. **Nothing here touches the network**: a
    /// reachability target built from the wildcard address is answered out of
    /// the routing table, and no packet is sent.
    ///
    /// `SCNetworkReachability` is deprecated in favour of `NWPathMonitor`,
    /// and is still the right call here: the replacement delivers its first
    /// answer asynchronously on a dispatch queue, and a collector that had to
    /// wait for it would be waiting on the world to record that the world was
    /// there.
    pub fn network() -> Option<&'static str> {
        #[link(name = "SystemConfiguration", kind = "framework")]
        unsafe extern "C" {
            fn SCNetworkReachabilityCreateWithAddress(
                allocator: *const c_void,
                address: *const libc::sockaddr,
            ) -> *mut c_void;
            fn SCNetworkReachabilityGetFlags(target: *mut c_void, flags: *mut u32) -> u8;
        }
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFRelease(cf: *const c_void);
        }
        // `kSCNetworkReachabilityFlagsReachable`, and the one that qualifies
        // it: a target that is "reachable" but needs a connection brought up
        // first is not connectivity a person has.
        const REACHABLE: u32 = 1 << 1;
        const CONNECTION_REQUIRED: u32 = 1 << 2;

        // SAFETY: a zeroed `sockaddr_in` with its length and family set is
        // the documented wildcard target; the create call copies it, and the
        // reference is released below.
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        address.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        address.sin_family = libc::AF_INET as u8;
        let target = unsafe {
            SCNetworkReachabilityCreateWithAddress(
                std::ptr::null(),
                (&raw const address).cast::<libc::sockaddr>(),
            )
        };
        if target.is_null() {
            return None;
        }
        let mut flags = 0u32;
        let answered = unsafe { SCNetworkReachabilityGetFlags(target, &mut flags) } != 0;
        unsafe { CFRelease(target) };
        if !answered {
            // The vocabulary's third token, for exactly this: the question
            // was asked of something valid and came back without an answer.
            return Some("unknown");
        }
        Some(
            if flags & REACHABLE != 0 && flags & CONNECTION_REQUIRED == 0 {
                "online"
            } else {
                "offline"
            },
        )
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    //! **Linux, and nothing else.** Every answer is "not determined" until
    //! this platform's own reader is written — absence over a guess, which is
    //! what the optional attrs are for. Windows and macOS each have an arm
    //! above; what is left here is one platform Koru does not yet ship a
    //! build for, so writing eleven readers against it would be eleven
    //! things nobody can run.
    pub fn os_version() -> Option<String> {
        None
    }
    pub fn ram_gb() -> Option<u64> {
        None
    }
    pub fn disk_free_gb(_path: &std::path::Path) -> Option<u64> {
        None
    }
    pub fn keyboard_layout() -> Option<&'static str> {
        None
    }
    pub fn touch() -> Option<bool> {
        None
    }
    pub fn reduced_motion() -> Option<bool> {
        None
    }
    pub fn high_contrast() -> Option<bool> {
        None
    }
    pub fn text_scale_percent() -> Option<u64> {
        None
    }
    pub fn first_day_of_week() -> Option<&'static str> {
        None
    }
    pub fn clock() -> Option<&'static str> {
        None
    }
    pub fn network() -> Option<&'static str> {
        None
    }
}

fn os_version() -> Option<String> {
    platform::os_version()
}
fn ram_gb() -> Option<u64> {
    platform::ram_gb()
}
/// **The nearest existing ancestor, not the root itself.** The store root is
/// a path this build *will* use, and before a first run nothing along it
/// exists yet — both platforms' calls take a path that has to be there, so
/// asking for the root directly answered `None` on exactly the machines where
/// free space is most worth knowing. Walking up changes no answer where the
/// directory does exist: every ancestor is on the same volume, which is the
/// only thing being measured.
fn disk_free_gb() -> Option<u64> {
    let root = koru_channel::store_root()?;
    let existing = root.ancestors().find(|path| path.exists())?;
    platform::disk_free_gb(existing)
}
/// **Main thread only, so it travels in [`WindowFacts`] rather than being
/// read by [`collect`].** On macOS 26 Text Input Services asserts it is on
/// the main queue and traps the process otherwise — the events worker's call
/// took the app down as onboarding opened the first workspace; the macOS
/// reader says when, and answers `None` off the main thread instead.
/// On Windows `GetKeyboardLayout(0)` answers for the calling thread, so the
/// event loop is the right thread to ask there too.
pub(crate) fn keyboard_layout() -> Option<&'static str> {
    platform::keyboard_layout()
}
fn touch() -> Option<bool> {
    platform::touch()
}
/// Whether the OS asks for reduced motion. `None` when it could not say,
/// which a caller reads as motion on. The window reads it once at
/// construction (`docs/features/workspace-foundation/TECHNICAL.final.md`
/// §5.8), so a change mid-session takes effect at the next launch.
pub fn reduced_motion() -> Option<bool> {
    platform::reduced_motion()
}
pub fn high_contrast() -> Option<bool> {
    platform::high_contrast()
}
fn text_scale_percent() -> Option<u64> {
    platform::text_scale_percent()
}
fn first_day_of_week() -> Option<&'static str> {
    platform::first_day_of_week()
}
fn clock() -> Option<&'static str> {
    platform::clock()
}
fn network() -> Option<&'static str> {
    platform::network()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever this machine answers, the result is publishable: every token
    /// is in its declared vocabulary and every version is dotted digits.
    /// Running it *here* is the point — the collector's own machine is the
    /// one case a fixture cannot invent.
    ///
    /// **And on a Mac it asserts what was answered, not only that it was
    /// valid.** For a year this test passed on macOS while every one of the
    /// eleven platform readers returned `None`, because a blank environment
    /// is a perfectly valid one — every attr it fills is optional. Validity
    /// was never the question a collector needed asked of it.
    #[test]
    fn what_this_machine_reports_validates() {
        let environment = collect(WindowFacts {
            monitor_count: Some(2),
            display_width: Some(3840),
            display_height: Some(2160),
            display_scale_percent: Some(150),
            window_width: Some(1280),
            window_height: Some(800),
            dark: true,
            // Read on the main thread, which a test is not: see
            // `keyboard_layout`.
            keyboard_layout: Some("qwerty"),
        });
        // Carried from the window facts and never re-read, on every platform.
        assert_eq!(
            environment.keyboard_layout,
            Some("qwerty"),
            "keyboard_layout"
        );
        // Nine of the ten the worker reads, on any Mac. The one absence is
        // named as an equality rather than left out, so writing a reader for
        // it means changing this line on purpose. The eleventh reader, the
        // keyboard layout, is the one asserted above: it is not this thread's
        // to call.
        #[cfg(target_os = "macos")]
        {
            assert!(environment.os_version.is_some(), "os_version");
            assert!(environment.ram_gb_bucket.is_some(), "ram_gb_bucket");
            assert!(
                environment.disk_free_gb_bucket.is_some(),
                "disk_free_gb_bucket"
            );
            assert_eq!(environment.touch, Some(false), "no Mac has a digitizer");
            assert!(environment.reduced_motion.is_some(), "reduced_motion");
            assert!(environment.high_contrast.is_some(), "high_contrast");
            assert!(environment.first_day_of_week.is_some(), "first_day_of_week");
            assert!(environment.clock.is_some(), "clock");
            assert!(environment.network.is_some(), "network");
            // macOS has no global text scale to read; the reader says why.
            assert_eq!(environment.text_scale_percent, None, "text_scale_percent");
        }
        let envelope = koru_events::Draft::from(environment)
            .into_envelope(&koru_events::EmitContext::user_session());
        koru_events::validate(&envelope).unwrap_or_else(|violation| {
            panic!("this machine's environment is unpublishable: {violation}")
        });
        // `--nocapture` prints what this machine actually answered, which is
        // the only way to eyeball a collector whose inputs are the machine.
        println!(
            "collected: {}",
            serde_json::to_string_pretty(&envelope.attrs).unwrap()
        );
    }

    /// Off the main thread the macOS reader is absence, not a trap. The
    /// trap itself cannot be reproduced here (see the reader), so what is
    /// pinned is the check that makes it unreachable: without it this call
    /// answers the machine's layout and the assertion fails. Spawned rather
    /// than run on the test's own thread, so the thread is not the main one
    /// however the harness is configured.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_keyboard_layout_is_absent_off_the_main_thread() {
        let answer = std::thread::spawn(keyboard_layout).join().unwrap();
        assert_eq!(answer, None);
    }

    /// The keyboard table, without a keyboard: the reader itself answers
    /// whatever the machine running the suite is set to, so the mapping is
    /// the only part of it a test can pin.
    ///
    /// The `-QWERTY` line is the one worth having. Apple ships QWERTY
    /// variants of several QWERTZ layouts under the same stem, so a table
    /// matching on the stem puts `Czech-QWERTY` in the QWERTZ bucket and
    /// nothing ever says so.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_layout_identifier_becomes_a_family() {
        use platform::layout_family;
        assert_eq!(layout_family("com.apple.keylayout.US"), "qwerty");
        assert_eq!(layout_family("com.apple.keylayout.French"), "azerty");
        assert_eq!(layout_family("com.apple.keylayout.German"), "qwertz");
        assert_eq!(layout_family("com.apple.keylayout.Czech"), "qwertz");
        assert_eq!(layout_family("com.apple.keylayout.Czech-QWERTY"), "qwerty");
        assert_eq!(layout_family("com.apple.keylayout.Dvorak"), "other");
        assert_eq!(layout_family("com.apple.keylayout.Russian"), "other");
        // An input method rather than a layout, which is what
        // `TISCopyCurrentKeyboardLayoutInputSource` exists to avoid handing
        // back — and is still not something to guess a family from.
        assert_eq!(
            layout_family("com.apple.inputmethod.Kotoeri.Japanese"),
            "other"
        );
    }

    #[test]
    fn an_unknown_locale_becomes_other_here() {
        assert_eq!(language_token(Some("en-GB")), "en");
        assert_eq!(language_token(Some("pt-BR")), "pt_br");
        assert_eq!(language_token(Some("is-IS")), "other");
        assert_eq!(language_token(None), "other");
    }
}
