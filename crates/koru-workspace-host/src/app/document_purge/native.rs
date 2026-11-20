// SPDX-License-Identifier: MPL-2.0
//! Custom choice labels without imposing a v6 loader import on every host/test.
use rfd::MessageDialogResult;
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW, TDF_RTL_LAYOUT,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

type TaskDialog =
    unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> i32;

pub(super) fn show(
    parent: Option<&Window>,
    title: &str,
    body: &str,
    choices: [&str; 3],
    rtl: bool,
) -> Result<MessageDialogResult, ()> {
    // The application manifest activates Common Controls v6. Tests and other
    // consumers may not have it: match the updater's dynamic lookup so those
    // processes still start, and retain the source if no dialog can be shown.
    let library = wide("comctl32.dll");
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    if module.is_null() {
        return Err(());
    }
    let result = (|| {
        let procedure =
            unsafe { GetProcAddress(module, b"TaskDialogIndirect\0".as_ptr()) }.ok_or(())?;
        // SAFETY: this named Windows API has the declared system ABI; module
        // and every string/button/configuration below live through the call.
        let dialog: TaskDialog = unsafe { std::mem::transmute(procedure) };
        let title = wide(title);
        let body = wide(body);
        let labels = choices.map(wide);
        let buttons = [
            TASKDIALOG_BUTTON {
                nButtonID: 1001,
                pszButtonText: labels[0].as_ptr(),
            },
            TASKDIALOG_BUTTON {
                nButtonID: 1002,
                pszButtonText: labels[1].as_ptr(),
            },
            TASKDIALOG_BUTTON {
                nButtonID: 2,
                pszButtonText: labels[2].as_ptr(),
            },
        ];
        let parent = parent
            .and_then(|window| window.window_handle().ok())
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as _),
                _ => None,
            })
            .unwrap_or(std::ptr::null_mut());
        let config = TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            hwndParent: parent,
            dwFlags: TDF_ALLOW_DIALOG_CANCELLATION
                | TDF_POSITION_RELATIVE_TO_WINDOW
                | if rtl { TDF_RTL_LAYOUT } else { 0 },
            pszWindowTitle: title.as_ptr(),
            pszContent: body.as_ptr(),
            cButtons: buttons.len() as u32,
            pButtons: buttons.as_ptr(),
            // Enter defaults to preserving the changed copy, never deleting it.
            nDefaultButton: 1001,
            // SAFETY: all remaining fields are optional pointers or integer flags.
            ..unsafe { std::mem::zeroed() }
        };
        let mut selected = 0;
        let status = unsafe {
            dialog(
                &config,
                &mut selected,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status < 0 {
            return Err(());
        }
        Ok(match selected {
            1001 => MessageDialogResult::Yes,
            1002 => MessageDialogResult::No,
            _ => MessageDialogResult::Cancel,
        })
    })();
    unsafe { FreeLibrary(module) };
    result
}

fn wide(value: &str) -> Vec<u16> {
    value
        .replace('\0', "\u{fffd}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}
