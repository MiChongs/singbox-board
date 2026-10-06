//! The tray on Windows: an icon in the notification area.
//!
//! A thread of its own runs the Win32 message loop of a hidden window that
//! owns the icon. The async side changes the shared [`BoardTray`] and posts
//! the window a message to show the change; menu commands go back through
//! the tray's action channel.
//!
//! - Left click opens the dashboard, right click (or the menu key) shows
//!   the menu, built from the shared menu model with native radio and check
//!   marks. Menus follow the system's dark mode.
//! - Notifications are balloons, which Windows 10 and 11 show as toasts,
//!   with the cube in the state's colour; clicking one opens the dashboard.
//! - When Explorer restarts, the icon is added again.
//! - The login item is a value in `HKCU\...\CurrentVersion\Run`, which
//!   respects "disabled" in the Startup apps settings.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::oneshot;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::{CREATE_NEW_CONSOLE, CreateMutexW};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetSystemMetricsForDpi,
    SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON,
    NIIF_RESPECT_QUIET_TIME, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
    ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, HMENU,
    ICONINFO, InsertMenuItemW, MENUITEMINFOW, MFS_CHECKED, MFS_DISABLED, MFT_RADIOCHECK,
    MFT_SEPARATOR, MFT_STRING, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SM_CXICON,
    SM_CXSMICON, SM_MENUDROPALIGNMENT, SW_SHOWNORMAL, SetForegroundWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_NULL,
    WM_QUERYENDSESSION, WM_SETTINGCHANGE, WNDCLASSEXW, WS_OVERLAPPED,
};

use super::icon::{self, Tone};
use super::{Action, BoardTray, Command, Entry};
use crate::i18n::fl;
use crate::win::{Handle as Kernel, from_wide, quote_arg, wide};

/// The icon tells the window about clicks with this message.
const WM_TRAY: u32 = WM_APP + 1;
/// The shared tray changed: redraw the icon and tooltip.
const WM_REFRESH: u32 = WM_APP + 2;
/// A notification is waiting.
const WM_NOTIFY_USER: u32 = WM_APP + 3;
/// Remove the icon and end the message loop.
const WM_QUIT_TRAY: u32 = WM_APP + 4;
/// The menu key on a focused icon (NIN_SELECT | NINF_KEY).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const ICON_ID: u32 = 1;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const LOGIN_VALUE: &str = "singbox-board";
const LAUNCHER: &str = "singbox-board-tray.exe";

/// The tray in this process: the state both threads share and the window
/// that shows it.
struct Shared {
    tray: Mutex<BoardTray>,
    window: AtomicPtr<core::ffi::c_void>,
    closed: AtomicBool,
}

impl Shared {
    fn post(&self, message: u32) -> bool {
        let window = self.window.load(Ordering::Acquire);
        // SAFETY: posting to a window that may already be gone is harmless.
        !window.is_null() && unsafe { PostMessageW(window, message, 0, 0) } != 0
    }
}

static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
static NOTIFICATIONS: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

fn shared() -> Option<Arc<Shared>> {
    SHARED.lock().unwrap().clone()
}

/// A named mutex held while the tray runs, so that a second start in the
/// same session does not add a second icon.
pub struct Session {
    _mutex: Kernel,
}

impl Session {
    pub async fn open() -> Result<Self> {
        let name = wide(r"Local\singbox-board-tray");
        // SAFETY: default security, NUL-terminated name.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        // SAFETY: read right after the call it describes.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = Kernel::new(handle).context(fl!("win-tray-already-running"))?;
        if existed {
            bail!(fl!("win-tray-already-running"));
        }
        Ok(Self { _mutex: mutex })
    }

    pub fn notifier(&self) -> Notifier {
        Notifier
    }
}

struct Notification {
    tone: Tone,
    title: String,
    body: String,
}

/// Shows notifications as balloons of the tray icon.
#[derive(Clone)]
pub struct Notifier;

impl Notifier {
    pub async fn send(&self, tone: Tone, summary: &str, body: &str) {
        NOTIFICATIONS.lock().unwrap().push(Notification {
            tone,
            title: summary.to_owned(),
            body: body.to_owned(),
        });
        if let Some(shared) = shared() {
            shared.post(WM_NOTIFY_USER);
        }
    }
}

/// The async side's grip on the tray.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
    thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl Handle {
    /// Changes the tray and shows the result; `None` once it is gone.
    pub async fn update<R>(&self, f: impl FnOnce(&mut BoardTray) -> R) -> Option<R> {
        if self.is_closed() {
            return None;
        }
        let result = f(&mut self.shared.tray.lock().unwrap());
        self.shared.post(WM_REFRESH);
        Some(result)
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    pub async fn shutdown(&self) {
        self.shared.post(WM_QUIT_TRAY);
        let thread = self.thread.lock().unwrap().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

pub async fn spawn(make: impl Fn() -> BoardTray) -> Result<Handle> {
    let shared = Arc::new(Shared {
        tray: Mutex::new(make()),
        window: AtomicPtr::new(std::ptr::null_mut()),
        closed: AtomicBool::new(false),
    });
    *SHARED.lock().unwrap() = Some(shared.clone());
    let (ready, started) = oneshot::channel();
    let thread_shared = shared.clone();
    let thread = std::thread::Builder::new()
        .name("tray".to_owned())
        .spawn(move || message_loop(thread_shared, ready))
        .context(fl!("tray-start-failed", error = "thread"))?;
    started
        .await
        .map_err(|_| anyhow!(fl!("tray-start-failed", error = "thread")))?
        .map_err(|err| anyhow!(fl!("tray-start-failed", error = err.to_string())))?;
    Ok(Handle {
        shared,
        thread: Arc::new(Mutex::new(Some(thread))),
    })
}

/// A detached tray has no console; one started from a terminal ends on
/// Ctrl-C. Signing out ends it through the window.
pub async fn terminated() {
    use tokio::signal::windows::{ctrl_c, ctrl_close};
    let (Ok(mut c), Ok(mut close)) = (ctrl_c(), ctrl_close()) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = c.recv() => {}
        _ = close.recv() => {}
    }
}

/// Opens the dashboard in a console window of its own, which Windows 11
/// shows in the default terminal application.
pub fn open_dashboard(command: &[OsString]) -> Result<()> {
    let (program, args) = command.split_first().context(fl!("tray-no-executable"))?;
    std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .with_context(|| fl!("err-spawn", path = program.to_string_lossy().into_owned()))?;
    Ok(())
}

/// `ShellExecuteW` on a thread of its own with COM initialised, as shell
/// extensions behind a verb may need it. `false` when it failed or the
/// user declined (UAC).
async fn shell_execute(
    verb: &'static str,
    file: String,
    parameters: Option<String>,
    show: i32,
) -> bool {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    tokio::task::spawn_blocking(move || {
        let (verb, file) = (wide(verb), wide(file));
        let parameters = parameters.map(wide);
        // SAFETY: NUL-terminated strings; COM is initialised for this call
        // on this thread only.
        unsafe {
            let initialised = CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            ) >= 0;
            let result = ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                parameters.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                std::ptr::null(),
                show,
            ) as isize;
            if initialised {
                CoUninitialize();
            }
            result > 32
        }
    })
    .await
    .unwrap_or(false)
}

/// Opens `url` in the default browser.
pub async fn open_url(url: &str) -> Result<()> {
    if !shell_execute("open", url.to_owned(), None, SW_SHOWNORMAL).await {
        bail!(fl!(
            "tray-open-failed",
            url = url,
            status = std::io::Error::last_os_error().to_string()
        ));
    }
    Ok(())
}

/// The login item starts the tray through `singbox-board-tray.exe`, which
/// has no console window to flash at sign-in.
pub fn login_command(mut command: Vec<OsString>) -> Vec<OsString> {
    let launcher = PathBuf::from(&command[0]).with_file_name(LAUNCHER);
    if launcher.is_file() {
        command[0] = launcher.into();
        if command.last().is_some_and(|arg| arg == "tray") {
            command.pop();
        }
    }
    command
}

fn registry_string(
    root: windows_sys::Win32::System::Registry::HKEY,
    key: &str,
    value: &str,
) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buffer = vec![0u16; 2048];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0).then(|| from_wide(&buffer).to_string_lossy().into_owned())
}

/// Whether the tray starts at sign-in: the Run value exists and the
/// Startup apps settings do not mark it disabled.
pub fn autostart_enabled() -> bool {
    if registry_string(HKEY_CURRENT_USER, RUN_KEY, LOGIN_VALUE).is_none() {
        return false;
    }
    let (key, value) = (wide(STARTUP_APPROVED), wide(LOGIN_VALUE));
    let mut data = [0u8; 12];
    let mut size = data.len() as u32;
    // SAFETY: the buffer holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    // An odd first byte (3, 7) means switched off in the settings.
    status != 0 || data[0] & 1 == 0
}

/// Adds or removes the login item that runs `command`.
pub fn set_autostart(enable: bool, command: &[OsString]) -> Result<()> {
    let (run, approved, value) = (wide(RUN_KEY), wide(STARTUP_APPROVED), wide(LOGIN_VALUE));
    // A fresh choice overrides an earlier "disabled" in the settings.
    // SAFETY: NUL-terminated strings.
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, approved.as_ptr(), value.as_ptr()) };
    let status = if enable {
        let line = command
            .iter()
            .map(|arg| quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let data = wide(line);
        // SAFETY: `data` holds the NUL-terminated string of the size given.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                run.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        }
    } else {
        // SAFETY: NUL-terminated strings.
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), value.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => 0,
            status => status,
        }
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .with_context(|| format!(r"HKEY_CURRENT_USER\{RUN_KEY}"));
    }
    Ok(())
}

/// Starts the daemon's service; asks for administrator rights when the
/// account may not start it by itself.
pub async fn start_service() -> Result<()> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let name = crate::win::service::NAME;
    let direct = tokio::task::spawn_blocking(move || -> windows_service::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(name, ServiceAccess::START)?;
        service.start::<&str>(&[])
    })
    .await?;
    let code = match &direct {
        Ok(()) => return Ok(()),
        Err(windows_service::Error::Winapi(err)) => err.raw_os_error(),
        Err(_) => None,
    };
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
    match code {
        Some(ERROR_SERVICE_ALREADY_RUNNING) => Ok(()),
        Some(ERROR_ACCESS_DENIED) => {
            let (verb, program, args) =
                (wide("runas"), wide("sc.exe"), wide(format!("start {name}")));
            // SAFETY: NUL-terminated strings; shows the UAC prompt.
            let result = unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    program.as_ptr(),
                    args.as_ptr(),
                    std::ptr::null(),
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
                )
            } as isize;
            if result <= 32 {
                bail!(fl!("win-service-elevation-declined", name = name));
            }
            // sc.exe returns at once; give the daemon a moment to listen.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            Ok(())
        }
        _ => Err(anyhow!(fl!(
            "win-service-start-failed",
            name = name,
            error = direct.err().map(|e| e.to_string()).unwrap_or_default()
        ))),
    }
}

// ----- the window thread ----------------------------------------------------

/// What the window thread keeps: icons by tone at the current size.
struct Window {
    hwnd: HWND,
    shared: Arc<Shared>,
    taskbar_created: u32,
    added: bool,
    icons: HashMap<Tone, Icon>,
    large: HashMap<Tone, Icon>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// An icon handle, destroyed on drop.
struct Icon(HICON);

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: created by CreateIconIndirect and not shared.
        unsafe { DestroyIcon(self.0) };
    }
}

fn message_loop(shared: Arc<Shared>, ready: oneshot::Sender<std::io::Result<()>>) {
    // SAFETY: plain Win32 calls on this thread's own window.
    unsafe {
        // Crisp icons on high-DPI screens.
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        allow_dark_menus();
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("singbox-board-tray");
        let window_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&window_class);
        // A top-level window, never shown: message-only windows do not get
        // the TaskbarCreated broadcast.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            let _ = ready.send(Err(std::io::Error::last_os_error()));
            shared.closed.store(true, Ordering::Release);
            return;
        }
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        WINDOW.with_borrow_mut(|window| {
            *window = Some(Window {
                hwnd,
                shared: shared.clone(),
                taskbar_created,
                added: false,
                icons: HashMap::new(),
                large: HashMap::new(),
            });
        });
        shared.window.store(hwnd, Ordering::Release);
        // Explorer may not run yet at sign-in; TaskbarCreated adds the icon
        // once it does.
        with_window(Window::add);
        let _ = ready.send(Ok(()));
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    shared.window.store(std::ptr::null_mut(), Ordering::Release);
    shared.closed.store(true, Ordering::Release);
    WINDOW.with_borrow_mut(|window| *window = None);
}

/// Runs `f` on the window state; nested calls (from the menu's modal loop)
/// find it busy and do nothing.
fn with_window<R: Default>(f: impl FnOnce(&mut Window) -> R) -> R {
    WINDOW.with(|cell| match cell.try_borrow_mut() {
        Ok(mut window) => window.as_mut().map(f).unwrap_or_default(),
        Err(_) => R::default(),
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: the event in the low word, the anchor
            // point in wparam.
            match (lparam & 0xffff) as u32 {
                NIN_SELECT | NIN_KEYSELECT | NIN_BALLOONUSERCLICK => {
                    send(Action::Dashboard);
                }
                WM_CONTEXTMENU => {
                    let x = (wparam & 0xffff) as u16 as i16 as i32;
                    let y = ((wparam >> 16) & 0xffff) as u16 as i16 as i32;
                    show_menu(hwnd, x, y);
                }
                _ => {}
            }
            0
        }
        WM_REFRESH => {
            with_window(|window| window.refresh());
            0
        }
        WM_NOTIFY_USER => {
            with_window(|window| window.notify());
            0
        }
        WM_QUIT_TRAY => {
            with_window(|window| window.remove());
            // SAFETY: this thread's own window.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                with_window(|window| window.remove());
                send(Action::Quit);
            }
            0
        }
        WM_SETTINGCHANGE => {
            // The light/dark choice changed: menus pick it up.
            flush_menu_themes();
            // SAFETY: forwarding the message unchanged.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => {
            if with_window(|window| message == window.taskbar_created) {
                // Explorer (re)started: icons of the old one are gone, and
                // the screen's DPI may have changed with it.
                with_window(|window| {
                    window.added = false;
                    window.icons.clear();
                    window.large.clear();
                    window.add();
                });
                return 0;
            }
            // SAFETY: the default handling of everything else.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

fn send(action: Action) {
    if let Some(shared) = shared() {
        shared.tray.lock().unwrap().send(action);
    }
}

impl Window {
    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        }
    }

    fn icon(&mut self, tone: Tone, large: bool) -> HICON {
        let metric = if large { SM_CXICON } else { SM_CXSMICON };
        let cache = if large {
            &mut self.large
        } else {
            &mut self.icons
        };
        cache
            .entry(tone)
            .or_insert_with(|| {
                // SAFETY: plain queries.
                let size = unsafe { GetSystemMetricsForDpi(metric, GetDpiForSystem()) };
                create_icon(tone, size.clamp(16, 256) as u32)
            })
            .0
    }

    /// Icon and tooltip from the shared tray.
    fn fill(&mut self, data: &mut NOTIFYICONDATAW) {
        let (tone, tip) = {
            let tray = self.shared.tray.lock().unwrap();
            let mut lines = vec![tray.headline()];
            lines.extend(tray.details());
            (tray.tone(), lines.join("\n"))
        };
        data.uFlags |= NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.hIcon = self.icon(tone, false);
        copy_text(&mut data.szTip, &tip);
    }

    fn add(&mut self) {
        let mut data = self.data();
        data.uFlags = NIF_MESSAGE;
        data.uCallbackMessage = WM_TRAY;
        self.fill(&mut data);
        // SAFETY: the data describes this window's icon.
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
                return;
            }
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
        self.added = true;
    }

    fn refresh(&mut self) {
        if !self.added {
            return self.add();
        }
        let mut data = self.data();
        self.fill(&mut data);
        // SAFETY: the data describes this window's icon.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    /// Shows the newest waiting notification; a burst shows only its end.
    fn notify(&mut self) {
        let Some(notification) = NOTIFICATIONS.lock().unwrap().drain(..).next_back() else {
            return;
        };
        if !self.added {
            self.add();
        }
        let mut data = self.data();
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME;
        data.hBalloonIcon = self.icon(notification.tone, true);
        copy_text(&mut data.szInfoTitle, &notification.title);
        // An empty text would remove the balloon instead of showing it.
        let body = if notification.body.trim().is_empty() {
            " "
        } else {
            &notification.body
        };
        copy_text(&mut data.szInfo, body);
        // SAFETY: the data describes this window's icon.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
            eprintln!(
                "{}",
                fl!(
                    "tray-notify-failed",
                    error = std::io::Error::last_os_error().to_string()
                )
            );
        }
    }

    fn remove(&mut self) {
        if self.added {
            let data = self.data();
            // SAFETY: the data describes this window's icon.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            self.added = false;
        }
    }
}

/// Copies `text` into a fixed UTF-16 field, cut with an ellipsis to fit.
fn copy_text(field: &mut [u16], text: &str) {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    if units.len() >= field.len() {
        units.truncate(field.len() - 2);
        // Do not leave half of a surrogate pair.
        if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            units.pop();
        }
        units.push('…' as u16);
    }
    field[..units.len()].copy_from_slice(&units);
    field[units.len()] = 0;
}

/// The cube in `tone` as an icon of `size` pixels.
fn create_icon(tone: Tone, size: u32) -> Icon {
    let rgba = icon::rgba(tone, size);
    // SAFETY: the DIB section is sized for `size`² 32-bit pixels and filled
    // before the icon copies it; both bitmaps are deleted afterwards.
    unsafe {
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative: rows from the top, like the RGBA data.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if !color.is_null() && !bits.is_null() {
            let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len());
            let targets = pixels.as_chunks_mut::<4>().0.iter_mut();
            for (target, [r, g, b, a]) in targets.zip(rgba.as_chunks::<4>().0) {
                *target = [*b, *g, *r, *a];
            }
        }
        // An all-zero mask: the colour bitmap's alpha decides.
        let mask_bytes = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, mask_bytes.as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(mask);
        DeleteObject(color);
        Icon(icon)
    }
}

/// Builds the native menu for `entries`; `commands[id - 1]` is what item
/// `id` does.
fn build_menu(entries: &[Entry], commands: &mut Vec<Command>) -> HMENU {
    // SAFETY: a fresh menu, filled with items whose strings outlive the
    // calls that copy them.
    unsafe {
        let menu = CreatePopupMenu();
        for (position, entry) in entries.iter().enumerate() {
            let mut item = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID,
                fType: MFT_STRING,
                ..std::mem::zeroed()
            };
            let mut label = Vec::new();
            let mut command_of = |command: &Option<Command>| match command {
                Some(command) => {
                    commands.push(command.clone());
                    commands.len() as u32
                }
                None => 0,
            };
            match entry {
                Entry::Separator => item.fType = MFT_SEPARATOR,
                Entry::Item {
                    label: text,
                    enabled,
                    command,
                    ..
                } => {
                    label = menu_text(text);
                    item.wID = command_of(command);
                    if !enabled || command.is_none() {
                        item.fState |= MFS_DISABLED;
                    }
                }
                Entry::Check {
                    label: text,
                    checked,
                    enabled,
                    command,
                } => {
                    label = menu_text(text);
                    item.wID = command_of(&Some(command.clone()));
                    item.fState |= if *checked { MFS_CHECKED } else { 0 };
                    item.fState |= if *enabled { 0 } else { MFS_DISABLED };
                }
                Entry::Radio {
                    label: text,
                    checked,
                    enabled,
                    command,
                } => {
                    label = menu_text(text);
                    item.fType |= MFT_RADIOCHECK;
                    item.wID = command_of(command);
                    item.fState |= if *checked { MFS_CHECKED } else { 0 };
                    item.fState |= if *enabled { 0 } else { MFS_DISABLED };
                }
                Entry::Sub {
                    label: text, items, ..
                } => {
                    label = menu_text(text);
                    item.fMask |= MIIM_SUBMENU;
                    item.hSubMenu = build_menu(items, commands);
                }
            }
            if !label.is_empty() {
                item.fMask |= MIIM_STRING;
                item.dwTypeData = label.as_mut_ptr();
            }
            InsertMenuItemW(menu, position as u32, 1, &item);
        }
        menu
    }
}

/// `&` marks an access key in a menu; `&&` is a literal one.
fn menu_text(text: &str) -> Vec<u16> {
    wide(text.replace('&', "&&"))
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let Some(shared) = shared() else {
        return;
    };
    let entries = {
        let tray = shared.tray.lock().unwrap();
        tray.send(Action::Refresh);
        tray.menu()
    };
    let mut commands = Vec::new();
    let menu = build_menu(&entries, &mut commands);
    // SAFETY: the menu and window belong to this thread; the window is
    // brought to the front so that the menu closes when clicking elsewhere.
    let chosen = unsafe {
        let mut point = POINT { x, y };
        if x == 0 && y == 0 {
            GetCursorPos(&mut point);
        }
        let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 {
            TPM_RIGHTALIGN
        } else {
            TPM_LEFTALIGN
        };
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenuEx(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | align,
            point.x,
            point.y,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen
    };
    if let Some(command) = usize::try_from(chosen)
        .ok()
        .and_then(|id| id.checked_sub(1))
        .and_then(|index| commands.get(index))
    {
        shared.tray.lock().unwrap().invoke(command.clone());
        with_window(|window| window.refresh());
    }
}

/// Windows build number, e.g. 22631.
fn build_number() -> u32 {
    registry_string(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .and_then(|build| build.trim().parse().ok())
    .unwrap_or(0)
}

/// uxtheme's unnamed exports that let popup menus follow the dark mode:
/// SetPreferredAppMode (135, AllowDarkModeForApp on 1809) and
/// FlushMenuThemes (136). Every Windows 10 1809+ and 11 has them; they are
/// looked up once.
fn uxtheme(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    type Export = Option<unsafe extern "system" fn() -> isize>;
    static EXPORTS: std::sync::OnceLock<(Export, Export)> = std::sync::OnceLock::new();
    let (set_mode, flush) = *EXPORTS.get_or_init(|| {
        if build_number() < 17763 {
            return (None, None);
        }
        // SAFETY: loading a system library and looking up exports by ordinal.
        unsafe {
            let library = LoadLibraryW(wide("uxtheme.dll").as_ptr());
            if library.is_null() {
                return (None, None);
            }
            (
                GetProcAddress(library, 135 as *const u8),
                GetProcAddress(library, 136 as *const u8),
            )
        }
    });
    match ordinal {
        135 => set_mode,
        136 => flush,
        _ => None,
    }
}

fn allow_dark_menus() {
    if let Some(set_preferred_app_mode) = uxtheme(135) {
        // SAFETY: both versions of the export take one int (1: allow dark).
        unsafe {
            let set: unsafe extern "system" fn(i32) -> i32 =
                std::mem::transmute(set_preferred_app_mode);
            set(1);
        }
    }
    flush_menu_themes();
}

fn flush_menu_themes() {
    if let Some(flush) = uxtheme(136) {
        // SAFETY: FlushMenuThemes takes no arguments.
        unsafe { flush() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_to_fit() {
        let mut field = [0u16; 8];
        copy_text(&mut field, "short");
        assert_eq!(String::from_utf16_lossy(&field[..5]), "short");
        assert_eq!(field[5], 0);
        copy_text(&mut field, "much too long");
        assert_eq!(String::from_utf16_lossy(&field[..7]), "much t…");
        assert_eq!(field[7], 0);
    }

    #[test]
    fn ampersands_are_not_access_keys() {
        assert_eq!(menu_text("R&D"), wide("R&&D"));
    }

    #[test]
    fn login_item_uses_the_launcher_when_present() {
        let dir = std::env::temp_dir().join(format!("sbb-launcher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe: OsString = dir.join("singbox-board.exe").into();
        let command = vec![exe.clone(), "--lang".into(), "en".into(), "tray".into()];
        assert_eq!(login_command(command.clone()), command);
        std::fs::write(dir.join(LAUNCHER), b"MZ").unwrap();
        let launcher: OsString = dir.join(LAUNCHER).into();
        assert_eq!(
            login_command(command),
            vec![launcher, "--lang".into(), "en".into()]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn icons_are_created_at_any_size() {
        for size in [16, 20, 24, 32, 48] {
            assert!(!create_icon(Tone::Running, size).0.is_null());
        }
    }

    #[test]
    fn menus_are_built() {
        let entries = vec![
            Entry::label("headline & more".into()),
            Entry::Separator,
            Entry::Sub {
                label: "Mode".into(),
                icon: "",
                items: vec![
                    Entry::Radio {
                        label: "rule".into(),
                        checked: true,
                        enabled: true,
                        command: None,
                    },
                    Entry::Radio {
                        label: "global".into(),
                        checked: false,
                        enabled: true,
                        command: Some(Command::Quit),
                    },
                ],
            },
            Entry::item("Quit".into(), true, "", Command::Quit),
        ];
        let mut commands = Vec::new();
        let menu = build_menu(&entries, &mut commands);
        assert!(!menu.is_null());
        assert_eq!(commands, [Command::Quit, Command::Quit]);
        unsafe { DestroyMenu(menu) };
    }
}
