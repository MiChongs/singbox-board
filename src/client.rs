//! Client for the daemon's control socket.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::i18n::{self, fl};
use crate::protocol::{
    Container, ContainerOverview, CoreReleasePage, CoreSource, Envelope, ExecResult, ImageList,
    LogEntry, Profile, ProfileList, Request, Response, Status, StoredCore, UpdateInfo,
};

/// A connection to the daemon: a Unix socket, or a named pipe on Windows.
#[cfg(unix)]
type Stream = tokio::net::UnixStream;
#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

async fn connect(socket: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    return tokio::net::UnixStream::connect(socket).await;
    #[cfg(windows)]
    return crate::win::pipe::connect(socket).await;
}

#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket: PathBuf,
}

impl DaemonClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn open(&self, request: &Request) -> Result<BufReader<Stream>> {
        let mut stream = connect(&self.socket)
            .await
            .map_err(|err| self.connect_error(err))?;
        // Replies come back in the language of this client.
        let envelope = Envelope {
            request: request.clone(),
            lang: Some(i18n::current().tag().to_owned()),
        };
        let mut line = serde_json::to_vec(&envelope)?;
        line.push(b'\n');
        stream.write_all(&line).await?;
        Ok(BufReader::new(stream))
    }

    fn connect_error(&self, err: io::Error) -> anyhow::Error {
        let socket = self.socket.display().to_string();
        match err.kind() {
            #[cfg(unix)]
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                anyhow!(fl!("client-daemon-not-running", socket = socket))
            }
            #[cfg(windows)]
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                anyhow!(fl!("win-client-daemon-not-running", socket = socket))
            }
            #[cfg(windows)]
            io::ErrorKind::PermissionDenied => {
                anyhow!(fl!("win-client-permission-denied", socket = socket))
            }
            #[cfg(unix)]
            io::ErrorKind::PermissionDenied => match stale_session_group(&self.socket) {
                Some(group) => anyhow!(fl!(
                    "client-permission-stale-group",
                    socket = socket,
                    group = group
                )),
                None => anyhow!(fl!("client-permission-denied", socket = socket)),
            },
            _ => anyhow!(fl!(
                "client-connect-failed",
                socket = socket,
                error = err.to_string()
            )),
        }
    }

    /// Sends a request and reads its single response; daemon errors become `Err`.
    pub async fn call(&self, request: Request) -> Result<Response> {
        let limit = timeout_for(&request);
        let exchange = async {
            let mut reader = self.open(&request).await?;
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                bail!(fl!("client-connection-closed"));
            }
            match serde_json::from_str(&line)? {
                Response::Error { message } => Err(anyhow!(message)),
                response => Ok(response),
            }
        };
        tokio::time::timeout(limit, exchange)
            .await
            .map_err(|_| anyhow!(fl!("client-timeout", seconds = limit.as_secs())))?
    }

    pub async fn status(&self) -> Result<Status> {
        match self.call(Request::Status).await? {
            Response::Status(status) => Ok(*status),
            other => Err(unexpected(&other)),
        }
    }

    /// Runs a command that answers with a `done` message.
    pub async fn command(&self, request: Request) -> Result<String> {
        match self.call(request).await? {
            Response::Done { message } => Ok(message),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn check_update(&self) -> Result<UpdateInfo> {
        match self.call(Request::CheckUpdate).await? {
            Response::UpdateInfo(info) => Ok(info),
            other => Err(unexpected(&other)),
        }
    }

    /// Sources and the one `update` follows.
    pub async fn core_sources(&self) -> Result<(Vec<CoreSource>, String)> {
        match self.call(Request::CoreSources).await? {
            Response::CoreSources { sources, default } => Ok((sources, default)),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn core_releases(
        &self,
        source: &str,
        page: u32,
        refresh: bool,
    ) -> Result<CoreReleasePage> {
        let request = Request::CoreReleases {
            source: source.to_owned(),
            page,
            refresh,
        };
        match self.call(request).await? {
            Response::CoreReleases(page) => Ok(page),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn core_installed(&self) -> Result<Vec<StoredCore>> {
        match self.call(Request::CoreInstalled).await? {
            Response::CoreInstalled { cores } => Ok(cores),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn profiles(&self) -> Result<ProfileList> {
        match self.call(Request::ProfileList).await? {
            Response::Profiles(list) => Ok(list),
            other => Err(unexpected(&other)),
        }
    }

    /// A profile and its content.
    pub async fn profile(&self, id: &str) -> Result<(Profile, String)> {
        let request = Request::ProfileGet { id: id.to_owned() };
        match self.call(request).await? {
            Response::ProfileContent { profile, content } => Ok((*profile, content)),
            other => Err(unexpected(&other)),
        }
    }

    /// `profile_add` or `profile_save`: the stored profile and a message.
    pub async fn profile_saved(&self, request: Request) -> Result<(Profile, String)> {
        match self.call(request).await? {
            Response::ProfileSaved { profile, message } => Ok((*profile, message)),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn containers(&self) -> Result<ContainerOverview> {
        match self.call(Request::ContainerList).await? {
            Response::Containers(overview) => Ok(*overview),
            other => Err(unexpected(&other)),
        }
    }

    /// A container and its configuration.
    pub async fn container(&self, id: &str) -> Result<(Container, String)> {
        let request = Request::ContainerGet { id: id.to_owned() };
        match self.call(request).await? {
            Response::ContainerContent { container, content } => Ok((*container, content)),
            other => Err(unexpected(&other)),
        }
    }

    /// `container_add` or `container_save`: the container and a message.
    pub async fn container_saved(&self, request: Request) -> Result<(Container, String)> {
        match self.call(request).await? {
            Response::ContainerSaved { container, message } => Ok((*container, message)),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn container_exec(
        &self,
        id: &str,
        command: Vec<String>,
        timeout: Option<u64>,
    ) -> Result<ExecResult> {
        let request = Request::ContainerExec {
            id: id.to_owned(),
            command,
            timeout,
        };
        match self.call(request).await? {
            Response::ContainerExec(result) => Ok(result),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn container_images(&self, refresh: bool) -> Result<ImageList> {
        match self.call(Request::ContainerImages { refresh }).await? {
            Response::ContainerImages(list) => Ok(list),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn logs(&self, tail: usize, follow: bool) -> Result<LogStream> {
        Ok(LogStream {
            reader: self.open(&Request::Logs { tail, follow }).await?,
            line: String::new(),
        })
    }
}

pub struct LogStream {
    reader: BufReader<Stream>,
    line: String,
}

impl LogStream {
    /// Next log entry, or `None` once the daemon closes the stream.
    pub async fn next(&mut self) -> Result<Option<LogEntry>> {
        self.line.clear();
        if self.reader.read_line(&mut self.line).await? == 0 {
            return Ok(None);
        }
        match serde_json::from_str(&self.line)? {
            Response::Log(entry) => Ok(Some(entry)),
            Response::Error { message } => Err(anyhow!(message)),
            other => Err(unexpected(&other)),
        }
    }
}

/// The socket's group when the user is a member in the group database but
/// the current process credentials (fixed at login) do not include it yet.
#[cfg(unix)]
fn stale_session_group(socket: &Path) -> Option<String> {
    use std::ffi::CString;
    use std::os::unix::fs::MetadataExt;

    use nix::unistd::{Gid, Group, Uid, User, getgrouplist, getgroups};

    let gid = Gid::from_raw(std::fs::metadata(socket).ok()?.gid());
    if Gid::effective() == gid || getgroups().ok()?.contains(&gid) {
        return None;
    }
    let user = User::from_uid(Uid::current()).ok()??;
    let member = getgrouplist(&CString::new(user.name).ok()?, user.gid)
        .ok()?
        .contains(&gid);
    member.then(|| Group::from_gid(gid).ok().flatten().map(|g| g.name))?
}

fn timeout_for(request: &Request) -> Duration {
    Duration::from_secs(match request {
        Request::Status => 5,
        Request::CheckUpdate => 90,
        Request::Update { .. }
        | Request::Setup { .. }
        | Request::Component { .. }
        | Request::CoreInstall { .. }
        | Request::CoreImport { .. } => 30 * 60,
        Request::CoreReleases { .. } | Request::CoreSourceAdd { .. } => 90,
        Request::CoreActivate { .. } | Request::ProfileActivate { .. } => 180,
        Request::ProfileAdd { .. } | Request::ProfileSave { .. } => 180,
        Request::ProfileUpdate { .. } => 15 * 60,
        Request::Start | Request::Restart | Request::Stop => 120,
        // Installing a root filesystem downloads and unpacks hundreds of MiB.
        Request::ContainerInstall { .. } => 3 * 60 * 60,
        // Starting may download the runtime first; deleting a root
        // filesystem may take a while.
        Request::ContainerControl { .. }
        | Request::ContainerRemove { .. }
        | Request::ContainerRuntimeUpdate { .. }
        | Request::ContainerRuntimeImport { .. } => 30 * 60,
        Request::ContainerExec { timeout, .. } => timeout.unwrap_or(60).clamp(1, 3600) + 30,
        Request::ContainerAdd { .. }
        | Request::ContainerSave { .. }
        | Request::ContainerAdopt { .. } => 180,
        // The first check may download the runtime.
        Request::ContainerCheck => 10 * 60,
        Request::ContainerImages { .. } | Request::ContainerScan => 90,
        _ => 60,
    })
}

fn unexpected(response: &Response) -> anyhow::Error {
    anyhow!(fl!(
        "client-unexpected-response",
        response = format!("{response:?}")
    ))
}
