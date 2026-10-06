//! File permissions and links on NTFS.
//!
//! Directories created below `C:\ProgramData` inherit an ACL that lets every
//! user create files in them. The daemon runs cores and components as
//! LocalSystem from these directories, so it locks them down the way Linux
//! has root own `/var/lib/singbox-board`.

use std::io;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use windows_sys::Win32::Foundation::{ERROR_PRIVILEGE_NOT_HELD, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

use super::{Identity, check, wide};
use crate::i18n::fl;

/// SYSTEM and administrators may do anything, users read and run.
const SHARED: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

/// A security descriptor parsed from SDDL, freed on drop.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: the string is NUL-terminated; the descriptor is freed with
        // LocalFree.
        check(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        })?;
        Ok(Self(descriptor))
    }

    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    fn dacl(&self) -> io::Result<*mut ACL> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl = std::ptr::null_mut();
        // SAFETY: the descriptor is valid.
        check(unsafe {
            GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted)
        })?;
        Ok(dacl)
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

// SAFETY: the descriptor is an immutable heap block.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

/// Replaces the DACL of `path` with the one in `sddl`, dropping inherited
/// entries.
fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let descriptor = SecurityDescriptor::from_sddl(sddl)?;
    let dacl = descriptor.dacl()?;
    let name = wide(path);
    // SAFETY: the name is NUL-terminated and the DACL lives in `descriptor`.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// Makes `path` accessible to SYSTEM, administrators and the current user
/// only, like mode 0600 (files) or 0700 (`dir`, inherited by its contents).
pub fn restrict(path: &Path, dir: bool) -> io::Result<()> {
    let user = Identity::current()?.user.to_string_sid();
    let inherit = if dir { "OICI" } else { "" };
    let sddl = format!("D:PAI(A;{inherit};FA;;;SY)(A;{inherit};FA;;;BA)(A;{inherit};FA;;;{user})");
    set_dacl(path, &sddl)
}

/// Creates `dir` if needed and lets only SYSTEM and administrators change
/// what is inside; everyone else may read.
pub fn secure_dir(dir: &Path) -> Result<()> {
    let shown = || dir.display().to_string();
    std::fs::create_dir_all(dir).with_context(|| fl!("err-create", path = shown()))?;
    set_dacl(dir, SHARED).with_context(|| fl!("win-acl-failed", path = shown()))
}

/// A symbolic link to a file. Creating one needs administrator rights or
/// Developer Mode.
pub fn symlink_file(target: &Path, link: &Path) -> Result<()> {
    std::os::windows::fs::symlink_file(target, link).map_err(|err| {
        if err.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) {
            anyhow!(fl!(
                "win-symlink-privilege",
                path = link.display().to_string()
            ))
        } else {
            anyhow::Error::new(err).context(fl!("err-create", path = link.display().to_string()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_parse() {
        assert!(SecurityDescriptor::from_sddl(SHARED).is_ok());
        assert!(SecurityDescriptor::from_sddl("not sddl").is_err());
    }

    #[test]
    fn restricted_files_stay_readable_by_their_owner() {
        let dir = std::env::temp_dir().join(format!("sbb-acl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secret.json");
        std::fs::write(&file, b"{}").unwrap();
        restrict(&file, false).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"{}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
