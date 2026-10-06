//! `singbox-board-tray.exe`: starts `singbox-board.exe tray` without a
//! console window.
//!
//! singbox-board is a console program, so Windows gives it a console
//! window when it is started from the Start menu or at sign-in. This
//! launcher is a GUI program that starts the tray detached from any
//! console instead; arguments are passed on before `tray`, e.g.
//! `singbox-board-tray.exe --lang zh-CN`.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let started = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe.with_file_name("singbox-board.exe"))
            .args(std::env::args_os().skip(1))
            .arg("tray")
            .creation_flags(DETACHED_PROCESS)
            .spawn()
    });
    match started {
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
            let (title, text) = (
                wide("singbox-board"),
                wide(&format!("singbox-board.exe: {err}")),
            );
            // SAFETY: both strings are NUL-terminated.
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    text.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONERROR,
                )
            };
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("singbox-board-tray only exists on Windows; run `singbox-board tray`");
    std::process::ExitCode::from(2)
}
