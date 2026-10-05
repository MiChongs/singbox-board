//! Plain command-line front-end for scripts and quick checks.

use anyhow::Result;

use crate::client::DaemonClient;
use crate::protocol::{LogSource, Request, Status};
use crate::util::{fmt_clock, fmt_duration, now_unix};

pub async fn status(client: &DaemonClient, json: bool) -> Result<()> {
    let status = client.status().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    print_status(&status);
    Ok(())
}

fn print_status(status: &Status) {
    let now = now_unix();
    println!(
        "daemon      v{} (pid {}, up {})",
        status.daemon_version,
        status.daemon_pid,
        fmt_duration(now.saturating_sub(status.daemon_started_at))
    );
    let mut core = status.state.label().to_owned();
    if let (Some(pid), Some(started)) = (status.pid, status.started_at) {
        core.push_str(&format!(
            " (pid {pid}, up {})",
            fmt_duration(now.saturating_sub(started))
        ));
    }
    if let Some(at) = status.next_restart_at {
        core.push_str(&format!(" (restart in {}s)", at.saturating_sub(now)));
    }
    println!("sing-box    {core}");
    println!(
        "version     {}",
        status.core_version.as_deref().unwrap_or("not installed")
    );
    println!("binary      {}", status.binary);
    println!("args        {}", status.args.join(" "));
    match &status.clash_api {
        Some(api) => println!(
            "clash api   {}{}",
            api.url,
            if api.secret.is_empty() {
                ""
            } else {
                " (secret set)"
            }
        ),
        None => println!("clash api   not configured (experimental.clash_api)"),
    }
    println!("restarts    {}", status.restarts);
    if let Some(exit) = &status.last_exit {
        println!("last exit   {exit}");
    }
    if status.update_in_progress {
        println!("update      in progress");
    }
}

pub async fn command(client: &DaemonClient, request: Request) -> Result<()> {
    println!("{}", client.command(request).await?);
    Ok(())
}

pub async fn logs(client: &DaemonClient, tail: usize, follow: bool) -> Result<()> {
    let mut stream = client.logs(tail, follow).await?;
    while let Some(entry) = stream.next().await? {
        match entry.source {
            LogSource::Core => println!("{}", entry.line),
            LogSource::Daemon => println!("{} [daemon] {}", fmt_clock(entry.ts), entry.line),
        }
    }
    Ok(())
}

pub async fn update(
    client: &DaemonClient,
    check_only: bool,
    tag: Option<String>,
    force: bool,
) -> Result<()> {
    if check_only {
        let info = client.check_update().await?;
        println!("installed   {}", info.current.as_deref().unwrap_or("none"));
        println!(
            "latest      {}{} ({})",
            info.latest,
            if info.prerelease {
                " [pre-release]"
            } else {
                ""
            },
            info.published_at.as_deref().unwrap_or("unknown date")
        );
        println!("asset       {}", info.asset);
        println!(
            "{}",
            if info.update_available {
                "an update is available: run `singbox-board update`"
            } else {
                "sing-box is up to date"
            }
        );
        return Ok(());
    }
    eprintln!("downloading and verifying the release, this may take a while…");
    command(client, Request::Update { tag, force }).await
}
