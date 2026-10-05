//! Client for the daemon's control socket.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::protocol::{LogEntry, Request, Response, Status, UpdateInfo};

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

    async fn open(&self, request: &Request) -> Result<BufReader<UnixStream>> {
        let mut stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|err| self.connect_error(err))?;
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        stream.write_all(&line).await?;
        Ok(BufReader::new(stream))
    }

    fn connect_error(&self, err: io::Error) -> anyhow::Error {
        let socket = self.socket.display();
        match err.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => anyhow!(
                "daemon is not running (no socket at {socket}); \
                 start it with `systemctl start singbox-board` or `sudo singbox-board daemon`"
            ),
            io::ErrorKind::PermissionDenied => anyhow!(
                "permission denied on {socket}; run as root or join the socket group \
                 (`sudo usermod -aG singbox-board $USER`, then log in again)"
            ),
            _ => anyhow!("connect {socket}: {err}"),
        }
    }

    /// Sends a request and reads its single response; daemon errors become `Err`.
    pub async fn call(&self, request: Request) -> Result<Response> {
        let limit = timeout_for(&request);
        let exchange = async {
            let mut reader = self.open(&request).await?;
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                bail!("daemon closed the connection");
            }
            match serde_json::from_str(&line)? {
                Response::Error { message } => Err(anyhow!(message)),
                response => Ok(response),
            }
        };
        tokio::time::timeout(limit, exchange)
            .await
            .map_err(|_| anyhow!("daemon did not answer within {}s", limit.as_secs()))?
    }

    pub async fn status(&self) -> Result<Status> {
        match self.call(Request::Status).await? {
            Response::Status(status) => Ok(status),
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

    pub async fn logs(&self, tail: usize, follow: bool) -> Result<LogStream> {
        Ok(LogStream {
            reader: self.open(&Request::Logs { tail, follow }).await?,
            line: String::new(),
        })
    }
}

pub struct LogStream {
    reader: BufReader<UnixStream>,
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

fn timeout_for(request: &Request) -> Duration {
    Duration::from_secs(match request {
        Request::Status => 5,
        Request::CheckUpdate => 90,
        Request::Update { .. } => 30 * 60,
        Request::Start | Request::Restart | Request::Stop => 120,
        _ => 60,
    })
}

fn unexpected(response: &Response) -> anyhow::Error {
    anyhow!("unexpected response from daemon: {response:?}")
}
