//! The control channel on Windows: a local named pipe.
//!
//! The pipe's DACL plays the part of the socket's mode and group: SYSTEM
//! and administrators get full access, members of `socket_group` may read
//! and write, nobody else may open it. Clients ask for `FILE_WRITE_DATA`
//! instead of `GENERIC_WRITE`, so group members are never granted
//! `FILE_CREATE_PIPE_INSTANCE` and cannot put up a server of their own
//! under the daemon's name. Remote clients are rejected.
//!
//! Who is on the other end is read from the client's token by
//! impersonating it, the counterpart of `SO_PEERCRED`.

use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::Path;
use std::time::Duration;

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_WRITE_DATA, OPEN_EXISTING,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

use super::fs::SecurityDescriptor;
use super::{Handle, Identity, Sid, wide};

/// What a member of the socket group may do with the pipe.
const CLIENT_ACCESS: u32 = FILE_GENERIC_READ | FILE_WRITE_DATA;

/// The server end, always holding one instance that waits for a client.
pub struct PipeListener {
    path: std::ffi::OsString,
    security: SecurityDescriptor,
    next: NamedPipeServer,
}

impl PipeListener {
    /// Creates the pipe. Fails with `AlreadyExists` when another process
    /// serves it already.
    pub fn bind(path: &Path, group: Option<&Sid>) -> io::Result<Self> {
        let mut sddl = String::from("D:P(A;;GA;;;SY)(A;;GA;;;BA)");
        if let Some(group) = group {
            sddl.push_str(&format!(
                "(A;;{CLIENT_ACCESS:#x};;;{})",
                group.to_string_sid()
            ));
        }
        let security = SecurityDescriptor::from_sddl(&sddl)?;
        let path = path.as_os_str().to_owned();
        let next = create(&path, &security, true).map_err(|err| {
            // FILE_FLAG_FIRST_PIPE_INSTANCE: the name is taken.
            if err.kind() == io::ErrorKind::PermissionDenied {
                io::Error::new(io::ErrorKind::AlreadyExists, err)
            } else {
                err
            }
        })?;
        Ok(Self {
            path,
            security,
            next,
        })
    }

    /// Waits for the next client.
    pub async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        self.next.connect().await?;
        let next = create(&self.path, &self.security, false)?;
        Ok(std::mem::replace(&mut self.next, next))
    }
}

fn create(
    path: &std::ffi::OsStr,
    security: &SecurityDescriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ptr(),
        bInheritHandle: 0,
    };
    // SAFETY: the attributes and the descriptor they point to outlive the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(path, (&raw mut attributes).cast())
    }
}

/// The identity of the client of `pipe`. Windows only lets a server
/// impersonate a client it has read from, so this follows the request.
pub fn client_identity(pipe: &NamedPipeServer) -> io::Result<Identity> {
    let handle = pipe.as_raw_handle();
    // SAFETY: the pipe handle is open; impersonation is reverted before
    // anything else runs on this thread.
    unsafe {
        super::check(ImpersonateNamedPipeClient(handle))?;
        let mut token = std::ptr::null_mut();
        // OpenAsSelf: the access check uses the daemon's own token, as an
        // identification-level token cannot open anything itself.
        let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token);
        let error = io::Error::last_os_error();
        if RevertToSelf() == 0 {
            // Carrying on as the client would be far worse than stopping.
            std::process::abort();
        }
        if opened == 0 {
            return Err(error);
        }
        Identity::from_token(Handle(token))
    }
}

/// The process id of the client of `pipe`, for the log.
pub fn client_pid(pipe: &NamedPipeServer) -> Option<u32> {
    let mut pid = 0;
    // SAFETY: the pipe handle is open.
    (unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } != 0).then_some(pid)
}

/// Opens the pipe at `path`, waiting a while if every instance is busy.
pub async fn connect(path: &Path) -> io::Result<NamedPipeClient> {
    let name = wide(path);
    let mut attempts = 0;
    loop {
        // SAFETY: the name is NUL-terminated.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | FILE_WRITE_DATA,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: a freshly opened overlapped pipe handle, owned from here on.
            return unsafe { NamedPipeClient::from_raw_handle(handle as RawHandle) };
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) || attempts >= 100 {
            return Err(error);
        }
        attempts += 1;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn clients_reach_the_server_and_are_identified() {
        let path = format!(r"\\.\pipe\singbox-board-test-{}", std::process::id());
        let path = Path::new(&path);
        let mut listener = PipeListener::bind(path, None).unwrap();
        if !crate::win::under_wine() {
            assert_eq!(
                PipeListener::bind(path, None).err().map(|e| e.kind()),
                Some(io::ErrorKind::AlreadyExists)
            );
        }
        let server = tokio::spawn(async move {
            let pipe = listener.accept().await.unwrap();
            let mut reader = BufReader::new(pipe);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let identity = client_identity(reader.get_ref()).unwrap();
            assert!(client_pid(reader.get_ref()).is_some());
            reader.get_mut().write_all(b"pong\n").await.unwrap();
            (line, identity.user.to_string_sid())
        });
        let mut client = BufReader::new(connect(path).await.unwrap());
        client.get_mut().write_all(b"ping\n").await.unwrap();
        let mut reply = String::new();
        client.read_line(&mut reply).await.unwrap();
        assert_eq!(reply, "pong\n");
        let (line, user) = server.await.unwrap();
        assert_eq!(line, "ping\n");
        assert_eq!(user, Identity::current().unwrap().user.to_string_sid());
        assert_eq!(
            connect(Path::new(r"\\.\pipe\singbox-board-missing"))
                .await
                .err()
                .map(|e| e.kind()),
            Some(io::ErrorKind::NotFound)
        );
    }
}
