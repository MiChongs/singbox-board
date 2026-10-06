//! Running the daemon as a Windows service.
//!
//! `singbox-board daemon --service` is the command line the service is
//! registered with. The service control manager's stop and pre-shutdown
//! requests take the place of SIGTERM, "paramchange" (`sc control
//! singbox-board paramchange`) the place of SIGHUP. A service has no
//! stderr, so the daemon's output goes to a log file that is rotated as it
//! grows.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::io::IntoRawHandle;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::Notify;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

use crate::i18n::fl;

/// Name the service is registered under.
pub const NAME: &str = "singbox-board";
/// The log is rotated once it grows past this size.
const LOG_LIMIT: u64 = 8 * 1024 * 1024;

/// What the service control manager asked for.
pub struct Events {
    pub stop: Notify,
    pub reload: Notify,
}

pub fn events() -> &'static Events {
    static EVENTS: OnceLock<Events> = OnceLock::new();
    EVENTS.get_or_init(|| Events {
        stop: Notify::new(),
        reload: Notify::new(),
    })
}

static STATUS: OnceLock<ServiceStatusHandle> = OnceLock::new();

type Main = Box<dyn FnOnce() -> Result<()> + Send>;
static MAIN: Mutex<Option<Main>> = Mutex::new(None);
static RESULT: Mutex<Option<Result<()>>> = Mutex::new(None);

define_windows_service!(ffi_service_main, service_main);

/// Hands the process to the service control manager, which then runs
/// `main` as the service. Returns once the service stopped.
pub fn dispatch(main: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
    *MAIN.lock().unwrap() = Some(Box::new(main));
    service_dispatcher::start(NAME, ffi_service_main)
        .map_err(|err| anyhow!(fl!("win-service-dispatch", error = err.to_string())))?;
    RESULT.lock().unwrap().take().unwrap_or(Ok(()))
}

fn status(state: ServiceState, exit_code: u32, wait_hint: Duration) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::PRESHUTDOWN
                | ServiceControlAccept::PARAM_CHANGE
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(exit_code),
        checkpoint: 0,
        wait_hint,
        process_id: None,
    }
}

fn service_main(_arguments: Vec<OsString>) {
    let result = run_service();
    *RESULT.lock().unwrap() = Some(result);
}

fn run_service() -> Result<()> {
    let handle = service_control_handler::register(NAME, |control| match control {
        ServiceControl::Stop | ServiceControl::Preshutdown | ServiceControl::Shutdown => {
            events().stop.notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::ParamChange => {
            events().reload.notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let _ = STATUS.set(handle);
    handle.set_service_status(status(ServiceState::Running, 0, Duration::ZERO))?;
    let main = MAIN.lock().unwrap().take();
    let result = main.map_or(Ok(()), |main| main());
    let code = if result.is_ok() { 0 } else { 1 };
    let _ = handle.set_service_status(status(ServiceState::Stopped, code, Duration::ZERO));
    result
}

/// Tells the service control manager that the daemon is shutting down and
/// may take a while (sing-box and the components get `stop_timeout_secs`).
pub fn stopping(wait: Duration) {
    if let Some(handle) = STATUS.get() {
        let _ = handle.set_service_status(status(ServiceState::StopPending, 0, wait));
    }
}

/// Sends standard output and error, and with them the daemon's log and the
/// output it forwards from sing-box, to `file`.
pub fn redirect_output(file: &Path) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut log = RotatingLog::open(file.to_path_buf())?;
    let (mut reader, writer) = std::io::pipe()?;
    let writer = writer.into_raw_handle();
    // SAFETY: the pipe's write end stays open for the life of the process.
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, writer);
        SetStdHandle(STD_ERROR_HANDLE, writer);
    }
    std::thread::Builder::new()
        .name("log-file".to_owned())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                log.write(&buffer[..read]);
            }
        })?;
    Ok(())
}

struct RotatingLog {
    path: PathBuf,
    file: std::fs::File,
    size: u64,
}

impl RotatingLog {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let size = file.metadata()?.len();
        let mut log = Self { path, file, size };
        if log.size > LOG_LIMIT {
            log.rotate();
        }
        Ok(log)
    }

    fn write(&mut self, data: &[u8]) {
        if self.file.write_all(data).is_ok() {
            self.size += data.len() as u64;
        }
        if self.size > LOG_LIMIT {
            self.rotate();
        }
    }

    /// `daemon.log` becomes `daemon.log.1`, replacing the previous one.
    fn rotate(&mut self) {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        let _ = std::fs::rename(&self.path, &old);
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.file = file;
            self.size = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_rotate() {
        let dir = std::env::temp_dir().join(format!("sbb-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.log");
        let mut log = RotatingLog::open(path.clone()).unwrap();
        log.write(&vec![b'x'; LOG_LIMIT as usize + 1]);
        log.write(b"fresh\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"fresh\n");
        assert!(dir.join("daemon.log.1").is_file());
        drop(log);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
