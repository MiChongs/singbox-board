//! The Windows side of the platform: wide strings, handles, tokens, paths.
//!
//! What Linux gets from the kernel and nix (credentials of socket peers,
//! process groups, PR_SET_PDEATHSIG, file modes) is built here on Win32:
//! named pipes with a DACL, impersonation tokens, job objects and console
//! control events.

pub mod clipboard;
pub mod fs;
pub mod net;
pub mod pipe;
pub mod process;
pub mod service;

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, DuplicateToken, PSID, SECURITY_MAX_SID_SIZE,
    SecurityIdentification, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `text` as a NUL-terminated UTF-16 string.
pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// A NUL-terminated UTF-16 buffer as a string.
pub fn from_wide(buffer: &[u16]) -> OsString {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..end])
}

/// A kernel handle closed on drop.
#[derive(Debug)]
pub struct Handle(pub HANDLE);

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Wraps the result of a call that returns null or
    /// `INVALID_HANDLE_VALUE` on failure.
    pub fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and still open.
        unsafe { CloseHandle(self.0) };
    }
}

/// `Err(last error)` when a BOOL-returning call failed.
pub fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `C:\ProgramData`, where machine-wide configuration and data live.
pub fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// A security identifier: a user, a group or a well-known account such as
/// LocalSystem.
#[derive(Clone, Debug)]
pub struct Sid(Vec<u8>);

impl Sid {
    pub fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Self> {
        let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = buffer.len() as u32;
        // SAFETY: the buffer holds `size` bytes.
        check(unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        })?;
        buffer.truncate(size as usize);
        Ok(Self(buffer))
    }

    /// Copies the SID `sid` points to.
    ///
    /// # Safety
    /// `sid` must point to a valid SID.
    pub unsafe fn copy(sid: PSID) -> Self {
        use windows_sys::Win32::Security::GetLengthSid;
        // SAFETY: guaranteed by the caller.
        let length = unsafe { GetLengthSid(sid) } as usize;
        // SAFETY: a valid SID is `length` bytes long.
        Self(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) }.to_vec())
    }

    pub fn as_psid(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    /// `S-1-5-18` and the like.
    pub fn to_string_sid(&self) -> String {
        let mut text = std::ptr::null_mut();
        // SAFETY: the SID is valid; the string is freed with LocalFree.
        unsafe {
            if ConvertSidToStringSidW(self.as_psid(), &mut text) == 0 {
                return String::new();
            }
            let length = (0..).take_while(|&i| *text.add(i) != 0).count();
            let result = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            result
        }
    }
}

impl PartialEq for Sid {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// Who a token belongs to and what it may do.
pub struct Identity {
    pub user: Sid,
    /// LocalSystem, or an elevated member of BUILTIN\Administrators: the
    /// Windows counterpart of root.
    pub admin: bool,
    token: Handle,
}

impl Identity {
    /// Reads an access or impersonation token opened for `TOKEN_QUERY`
    /// (and `TOKEN_DUPLICATE` for a primary token).
    pub fn from_token(token: Handle) -> io::Result<Self> {
        let identity = Self {
            user: token_user(&token)?,
            admin: false,
            token,
        };
        let admin = identity.user == Sid::well_known(WinLocalSystemSid)?
            || identity.member_of(&Sid::well_known(WinBuiltinAdministratorsSid)?);
        Ok(Self { admin, ..identity })
    }

    /// The identity of this process.
    pub fn current() -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: plain out parameter.
        check(unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &mut token,
            )
        })?;
        Self::from_token(Handle(token))
    }

    /// Whether the token is an enabled member of `group`. CheckTokenMembership
    /// wants an impersonation token, so a primary token is duplicated first.
    pub fn member_of(&self, group: &Sid) -> bool {
        let mut duplicate = std::ptr::null_mut();
        // SAFETY: plain out parameter; an impersonation token opened without
        // TOKEN_DUPLICATE fails here and is used as it is.
        if unsafe { DuplicateToken(self.token.0, SecurityIdentification, &mut duplicate) } != 0 {
            let duplicate = Handle(duplicate);
            return member_of(duplicate.0, group);
        }
        member_of(self.token.0, group)
    }
}

fn member_of(token: HANDLE, group: &Sid) -> bool {
    let mut member = 0;
    // SAFETY: the token and SID are valid.
    unsafe { CheckTokenMembership(token, group.as_psid(), &mut member) != 0 && member != 0 }
}

fn token_user(token: &Handle) -> io::Result<Sid> {
    use windows_sys::Win32::Security::GetTokenInformation;
    let mut buffer = vec![0u64; 64];
    let mut length = 0;
    // SAFETY: the buffer is large enough for TOKEN_USER and its SID and
    // aligned for it.
    check(unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut length,
        )
    })?;
    // SAFETY: GetTokenInformation filled in a TOKEN_USER.
    Ok(unsafe { Sid::copy((*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid) })
}

/// Whether this process runs as LocalSystem or elevated administrator.
pub fn is_elevated() -> bool {
    Identity::current().is_ok_and(|identity| identity.admin)
}

/// The SID of a local group or account, e.g. `singbox-board`.
pub fn lookup_account(name: &str) -> io::Result<Sid> {
    use windows_sys::Win32::Security::LookupAccountNameW;
    let name = wide(name);
    let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut sid_size = sid.len() as u32;
    let mut domain = vec![0u16; 256];
    let mut domain_size = domain.len() as u32;
    let mut kind = 0;
    // SAFETY: the buffers hold the sizes passed.
    check(unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_size,
            domain.as_mut_ptr(),
            &mut domain_size,
            &mut kind,
        )
    })?;
    sid.truncate(sid_size as usize);
    Ok(Sid(sid))
}

/// The user's display language, e.g. `zh-CN`.
pub fn user_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buffer = [0u16; 85];
    // SAFETY: the buffer holds LOCALE_NAME_MAX_LENGTH characters.
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    (length > 1).then(|| from_wide(&buffer).to_string_lossy().into_owned())
}

/// Whether the process has a standard error to write to (a console, a
/// file or a pipe). A process started detached, like the tray from its
/// launcher, has none.
pub fn has_stderr() -> bool {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: no side effects.
    let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

/// Shows `text` in a message box, for processes without a console.
pub fn message_box(title: &str, text: &str, error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    let (title, text) = (wide(title), wide(text));
    let icon = if error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    // SAFETY: both strings are NUL-terminated.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | icon | MB_SETFOREGROUND,
        )
    };
}

/// Whether this runs under Wine, whose symbolic links and first pipe
/// instances do not behave like Windows'; tests skip what needs them.
#[cfg(test)]
pub fn under_wine() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    // SAFETY: looking up an export of a module every process has loaded.
    unsafe {
        let ntdll = GetModuleHandleW(wide("ntdll.dll").as_ptr());
        !ntdll.is_null() && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some()
    }
}

/// Quotes one argument for a command line parsed by CommandLineToArgvW and
/// the C runtime.
pub fn quote_arg(arg: &OsStr) -> String {
    let arg = arg.to_string_lossy();
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
        return arg.into_owned();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            c => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_for_argv() {
        assert_eq!(quote_arg(OsStr::new("tray")), "tray");
        assert_eq!(
            quote_arg(OsStr::new(r"C:\Program Files\sb\sb.exe")),
            r#""C:\Program Files\sb\sb.exe""#
        );
        assert_eq!(quote_arg(OsStr::new(r"a\ b\")), r#""a\ b\\""#);
        assert_eq!(quote_arg(OsStr::new(r#"say "hi""#)), r#""say \"hi\"""#);
        assert_eq!(quote_arg(OsStr::new("")), r#""""#);
    }

    #[test]
    fn well_known_sids() {
        let system = Sid::well_known(WinLocalSystemSid).unwrap();
        assert_eq!(system.to_string_sid(), "S-1-5-18");
        assert!(Identity::current().is_ok());
    }
}
