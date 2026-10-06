//! Child processes of the daemon on Windows.
//!
//! - Every child is put in a job object that kills its members when the
//!   daemon's handle to it closes, i.e. when the daemon dies: the
//!   counterpart of `PR_SET_PDEATHSIG`. Grandchildren (mihomo started by
//!   http-meta) join it too.
//! - Children run in their own process group on the daemon's console, so a
//!   Ctrl-C meant for a daemon in a terminal does not reach them, and a
//!   CTRL_BREAK sent to the group asks them to stop: sing-box (Go) treats it
//!   like SIGINT and shuts down cleanly. A service has no console, so the
//!   daemon allocates a hidden one first.

use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::MAX_PATH;
use windows_sys::Win32::System::Console::{
    AllocConsole, CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleWindow,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

use super::Handle;

/// Creation flags for a supervised child.
pub const CHILD_FLAGS: u32 = CREATE_NEW_PROCESS_GROUP;

/// The job every child joins; `None` when Windows refused to create it.
fn job() -> Option<&'static Handle> {
    static JOB: OnceLock<Option<Handle>> = OnceLock::new();
    JOB.get_or_init(|| {
        // SAFETY: no name, default security.
        let job =
            Handle::new(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) }).ok()?;
        // SAFETY: zero is a valid bit pattern for this plain C struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the struct matches the information class.
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        (set != 0).then_some(job)
    })
    .as_ref()
}

/// Ties `child` to the daemon's lifetime.
pub fn adopt(child: &tokio::process::Child) {
    if let (Some(job), Some(process)) = (job(), child.raw_handle()) {
        // SAFETY: both handles are open.
        unsafe { AssignProcessToJobObject(job.0, process) };
    }
}

/// Gives a process without a console (a service) a hidden one for its
/// children to share, so that they can be sent CTRL_BREAK.
pub fn ensure_console() {
    // SAFETY: no arguments; the window, if any, belongs to this process.
    unsafe {
        if GetConsoleWindow().is_null() && AllocConsole() != 0 {
            let window = GetConsoleWindow();
            if !window.is_null() {
                ShowWindow(window, SW_HIDE);
            }
        }
    }
}

/// Asks the process group led by `pid` to stop. `false` when the event
/// could not be delivered and the caller should terminate the process.
pub fn request_stop(pid: u32) -> bool {
    // SAFETY: plain values.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
}

/// The executable of process `pid`.
fn image_path(process: &Handle) -> Option<PathBuf> {
    let mut buffer = vec![0u16; MAX_PATH as usize * 4];
    let mut size = buffer.len() as u32;
    // SAFETY: the buffer holds `size` characters.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            buffer.as_mut_ptr(),
            &mut size,
        )
    };
    (ok != 0).then(|| PathBuf::from(std::ffi::OsString::from_wide(&buffer[..size as usize])))
}

/// Terminates every process whose executable is `exe`; returns their ids.
pub fn kill_by_executable(exe: &Path) -> Vec<u32> {
    let wanted = exe.to_string_lossy().to_lowercase();
    let mut killed = Vec::new();
    // SAFETY: the snapshot handle is owned and closed on drop.
    let Ok(snapshot) = Handle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })
    else {
        return killed;
    };
    // SAFETY: zero is a valid bit pattern; dwSize is set as required.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: the entry is initialised as the API requires.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        let pid = entry.th32ProcessID;
        if pid != 0 && pid != std::process::id() {
            // SAFETY: a plain open by id; failures are skipped.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    pid,
                )
            };
            if let Ok(process) = Handle::new(handle)
                && image_path(&process)
                    .is_some_and(|p| p.to_string_lossy().to_lowercase() == wanted)
            {
                // SAFETY: the handle has PROCESS_TERMINATE.
                if unsafe { TerminateProcess(process.0, 1) } != 0 {
                    killed.push(pid);
                }
            }
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    killed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_are_found_by_executable() {
        let missing = std::env::temp_dir().join("sbb-no-such-program.exe");
        assert!(kill_by_executable(&missing).is_empty());
        let me = image_path(
            &Handle::new(unsafe {
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, std::process::id())
            })
            .unwrap(),
        );
        assert!(me.is_some_and(|p| p.is_absolute()));
    }
}
