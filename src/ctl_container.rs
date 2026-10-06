//! Command-line front-end for containers: `singbox-board container`.
// `container` commands need Linux; the TUI's container tab is not shown on
// Windows either.
#![cfg_attr(windows, allow(dead_code))]

use std::io::{IsTerminal, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use anyhow::{Context, Result, anyhow, bail};

use crate::client::DaemonClient;
use crate::ctl::{ask, checksum_text, paint, print_rows};
use crate::i18n::fl;
use crate::protocol::{
    Container, ContainerAction, ContainerOverview, ContainerRuntime, ContainerSpec, CoreState,
    Request,
};
use crate::util::{error_chain, fmt_bytes, fmt_duration, now_unix, pad, text_width};

/// State of a container as a short coloured word, padded to `width`
/// columns before colouring.
fn state_text(container: &Container, width: usize) -> String {
    if let Some(busy) = container.busy_label() {
        return paint(&pad(&busy, width), "33");
    }
    let code = match container.state {
        CoreState::Running => "32",
        CoreState::Failed => "31",
        CoreState::Starting | CoreState::Stopping | CoreState::Backoff => "33",
        CoreState::Stopped => "2",
    };
    paint(&pad(&container.state.label(), width), code)
}

/// `nat 172.28.0.2/16 via kurumi-br0`, `host`, ...
pub fn network_text(spec: &ContainerSpec) -> String {
    let mut text = spec.network.clone();
    if let Some(address) = &spec.address {
        text.push(' ');
        text.push_str(address);
    }
    if let Some(bridge) = &spec.bridge {
        text.push(' ');
        text.push_str(&fl!("ctl-container-via", bridge = bridge.clone()));
    }
    text
}

/// `PID 42, up 05:06, 12 processes, 128.0 MiB`.
pub fn live_text(container: &Container, now: u64) -> Option<String> {
    let live = container.live.as_ref()?;
    Some(fl!(
        "ctl-container-live",
        pid = live.init_pid.to_string(),
        uptime = fmt_duration(now.saturating_sub(live.started_at)),
        processes = live.processes,
        memory = fmt_bytes(live.memory)
    ))
}

pub fn runtime_text(runtime: &ContainerRuntime) -> String {
    if let Some(busy) = &runtime.busy {
        return match busy.as_str() {
            "updating" => fl!("busy-updating"),
            _ => fl!("busy-installing"),
        };
    }
    match (&runtime.version, runtime.origin) {
        (Some(version), Some(origin)) => fl!(
            "ctl-container-runtime-version",
            version = version.clone(),
            origin = origin.label()
        ),
        (None, Some(origin)) => fl!(
            "ctl-container-runtime-unknown-version",
            origin = origin.label()
        ),
        _ if runtime.target.is_some() => fl!("ctl-container-runtime-missing"),
        _ => fl!("ctl-container-runtime-unsupported"),
    }
}

pub async fn list(client: &DaemonClient) -> Result<()> {
    let overview = client.containers().await?;
    print_runtime_line(&overview.runtime);
    if overview.containers.is_empty() {
        println!("{}", fl!("ctl-containers-empty"));
    }
    let now = now_unix();
    let width = overview
        .containers
        .iter()
        .map(|c| text_width(&c.name))
        .max()
        .unwrap_or(0)
        .clamp(8, 28);
    for container in &overview.containers {
        let marker = if container.running() {
            paint("●", "32")
        } else {
            " ".to_owned()
        };
        let mut line = format!(
            "{marker} {} {}",
            paint(&pad(&container.name, width), "1"),
            state_text(container, 10)
        );
        if let Some(spec) = &container.spec {
            line.push_str("  ");
            line.push_str(&network_text(spec));
        }
        if let Some(text) = live_text(container, now) {
            line.push_str("  ");
            line.push_str(&text);
        }
        println!("{line}");
        let mut details = vec![container.id.clone()];
        if let Some(spec) = &container.spec {
            details.push(spec.name.clone());
            if !spec.installed {
                details.push(fl!("ctl-container-no-rootfs"));
            }
        }
        details.push(container.file.clone());
        if container.autostart {
            details.push(fl!("ctl-container-autostart"));
        }
        println!("    {}", paint(&details.join("  "), "2"));
        for problem in [&container.spec_error, &container.last_error]
            .into_iter()
            .flatten()
        {
            println!("    {}", paint(problem, "33"));
        }
    }
    println!(
        "\n{}\n{}",
        paint(&fl!("ctl-container-hint-new"), "2"),
        paint(&fl!("ctl-container-hint-use"), "2")
    );
    Ok(())
}

fn print_runtime_line(runtime: &ContainerRuntime) {
    let mut line = format!("kurumi-containerd  {}", runtime_text(runtime));
    if let Some(problem) = &runtime.problem {
        line.push_str("  ");
        line.push_str(&paint(problem, "33"));
    }
    println!("{line}\n");
}

pub async fn show(client: &DaemonClient, query: &str, config: bool) -> Result<()> {
    let (container, content) = client.container(query).await?;
    if config {
        let mut stdout = std::io::stdout();
        stdout.write_all(content.as_bytes())?;
        if !content.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
        return Ok(());
    }
    let now = now_unix();
    let mut rows = vec![
        (fl!("ctl-label-name"), container.name.clone()),
        ("ID".to_owned(), container.id.clone()),
        (fl!("ctl-label-state"), state_text(&container, 0)),
    ];
    if let Some(live) = &container.live {
        rows.push((
            fl!("ctl-label-process"),
            fl!(
                "ctl-container-process",
                pid = live.init_pid.to_string(),
                monitor = live.monitor_pid.to_string(),
                init = live.init_system.clone(),
                uptime = fmt_duration(now.saturating_sub(live.started_at))
            ),
        ));
        rows.push((
            fl!("ctl-label-usage"),
            fl!(
                "ctl-container-usage",
                processes = live.processes,
                memory = fmt_bytes(live.memory),
                cpu = format!("{:.1}", live.cpu_ms as f64 / 1000.0)
            ),
        ));
        if live.generation > 0 {
            rows.push((fl!("ctl-label-reboots"), live.generation.to_string()));
        }
    }
    match &container.spec {
        Some(spec) => spec_rows(&mut rows, spec),
        None => rows.push((
            fl!("ctl-label-config-error"),
            paint(container.spec_error.as_deref().unwrap_or_default(), "31"),
        )),
    }
    rows.push((
        fl!("ctl-label-autostart"),
        if container.autostart {
            fl!("answer-yes")
        } else {
            fl!("answer-no")
        },
    ));
    rows.push((
        fl!("ctl-label-config"),
        if container.managed {
            container.file.clone()
        } else {
            fl!("ctl-container-linked", file = container.file.clone())
        },
    ));
    if let Some(error) = &container.last_error {
        rows.push((fl!("ctl-label-last-error"), paint(error, "33")));
    }
    print_rows(&rows);
    Ok(())
}

fn spec_rows(rows: &mut Vec<(String, String)>, spec: &ContainerSpec) {
    rows.push(("container.name".to_owned(), spec.name.clone()));
    rows.push((fl!("ctl-label-hostname"), spec.hostname.clone()));
    if let Some(uuid) = &spec.uuid {
        rows.push(("UUID".to_owned(), uuid.clone()));
    }
    let mut rootfs = spec.rootfs.clone();
    if spec.image {
        rootfs.push_str(&format!("  ({})", fl!("ctl-container-image-file")));
    }
    if !spec.installed {
        rootfs.push_str("  ");
        rootfs.push_str(&paint(&fl!("ctl-container-no-rootfs"), "33"));
    }
    rows.push((fl!("ctl-label-rootfs"), rootfs));
    rows.push((fl!("ctl-label-init"), spec.init.clone()));
    rows.push((fl!("ctl-label-network"), network_text(spec)));
    if !spec.ports.is_empty() {
        let ports: Vec<String> = spec
            .ports
            .iter()
            .map(|p| format!("{} → {}/{}", p.host, p.container, p.protocol))
            .collect();
        rows.push((fl!("ctl-label-ports"), ports.join(", ")));
    }
    for mount in &spec.mounts {
        let mode = if mount.read_only { " (ro)" } else { "" };
        rows.push((
            fl!("ctl-label-mount"),
            format!("{} → {}{mode}", mount.source, mount.target),
        ));
    }
    let mut limits = Vec::new();
    if let Some(memory) = spec.memory_limit {
        limits.push(fl!(
            "ctl-container-limit-memory",
            memory = fmt_bytes(memory)
        ));
    }
    if let Some(cpu) = spec.cpu_limit {
        limits.push(fl!(
            "ctl-container-limit-cpu",
            cpus = format!("{:.2}", cpu as f64 / 1000.0)
        ));
    }
    if let Some(pids) = spec.pids_limit {
        limits.push(fl!("ctl-container-limit-pids", pids = pids));
    }
    rows.push((
        fl!("ctl-label-limits"),
        if limits.is_empty() {
            fl!("none")
        } else {
            crate::util::join_list(&limits)
        },
    ));
    let mut flags = Vec::new();
    if spec.volatile {
        flags.push(fl!("ctl-container-flag-volatile"));
    }
    if spec.user_namespaces {
        flags.push(fl!("ctl-container-flag-userns"));
    }
    if spec.foreground {
        flags.push(paint(&fl!("ctl-container-flag-foreground"), "33"));
    }
    if !flags.is_empty() {
        rows.push((fl!("ctl-label-options"), crate::util::join_list(&flags)));
    }
}

/// `container new`: from the template, optionally installing an image and
/// starting it.
pub async fn new(
    client: &DaemonClient,
    name: String,
    network: Option<String>,
    image: Option<String>,
    edit_after: bool,
    start: bool,
) -> Result<()> {
    let request = Request::ContainerAdd {
        name: Some(name),
        content: None,
        file: None,
        network,
    };
    let (container, message) = client.container_saved(request).await?;
    println!("{message}");
    if edit_after {
        edit(client, &container.id, false, false).await?;
    }
    if let Some(image) = image {
        install(client, &container.id, &image, None, None, false).await?;
    }
    if start {
        control(client, &container.id, ContainerAction::Start).await?;
    }
    Ok(())
}

/// `container add`: a copy of a file (or stdin), or the file in place.
pub async fn add(
    client: &DaemonClient,
    source: &str,
    name: Option<String>,
    link: bool,
) -> Result<()> {
    let request = if link {
        let path = std::fs::canonicalize(source).with_context(|| fl!("err-read", path = source))?;
        Request::ContainerAdd {
            name,
            content: None,
            file: Some(path.display().to_string()),
            network: None,
        }
    } else {
        // Read with the caller's permissions, not the daemon's.
        let content = if source == "-" {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            text
        } else {
            std::fs::read_to_string(source).with_context(|| fl!("err-read", path = source))?
        };
        Request::ContainerAdd {
            name,
            content: Some(content),
            file: None,
            network: None,
        }
    };
    let (_, message) = client.container_saved(request).await?;
    println!("{message}");
    Ok(())
}

pub async fn control(client: &DaemonClient, query: &str, action: ContainerAction) -> Result<()> {
    if action != ContainerAction::Stop {
        let overview = client.containers().await?;
        if overview.runtime.binary.is_none() {
            eprintln!("{}", fl!("ctl-container-runtime-downloading"));
        }
    }
    let request = Request::ContainerControl {
        id: query.to_owned(),
        action,
    };
    println!("{}", client.command(request).await?);
    Ok(())
}

/// `container install`: a local archive (made absolute here), a URL or an
/// image name.
pub async fn install(
    client: &DaemonClient,
    query: &str,
    source: &str,
    size: Option<String>,
    sha256: Option<String>,
    force: bool,
) -> Result<()> {
    let is_url = source.starts_with("http://") || source.starts_with("https://");
    let source = match std::fs::canonicalize(source) {
        Ok(path) if !is_url && path.is_file() => path.display().to_string(),
        _ => source.to_owned(),
    };
    eprintln!("{}", fl!("ctl-container-installing"));
    let request = Request::ContainerInstall {
        id: query.to_owned(),
        source,
        size,
        sha256,
        force,
    };
    println!("{}", client.command(request).await?);
    Ok(())
}

/// `container exec`: prints the command's output and exits with its status.
pub async fn exec(
    client: &DaemonClient,
    query: &str,
    timeout: Option<u64>,
    command: Vec<String>,
) -> Result<()> {
    let result = client.container_exec(query, command, timeout).await?;
    let mut stdout = std::io::stdout();
    stdout.write_all(result.stdout.as_bytes())?;
    stdout.flush()?;
    let mut stderr = std::io::stderr();
    stderr.write_all(result.stderr.as_bytes())?;
    if result.truncated {
        writeln!(stderr, "{}", fl!("ctl-container-output-truncated"))?;
    }
    stderr.flush()?;
    if result.code != 0 {
        std::process::exit(result.code.clamp(1, 255));
    }
    Ok(())
}

/// The command that opens a shell in a container: the runtime itself, run
/// as root with the daemon's registry.
pub struct EnterCommand {
    pub binary: String,
    pub home: String,
    pub id: String,
}

/// What `enter` runs, after checking that it can.
pub fn enter_command(overview: &ContainerOverview, container: &Container) -> Result<EnterCommand> {
    let binary = overview
        .runtime
        .binary
        .clone()
        .ok_or_else(|| anyhow!(fl!("containers-runtime-missing")))?;
    if !container.running() {
        bail!(fl!("containers-not-running", name = container.name.clone()));
    }
    // The path comes from the daemon; still refuse to run as root a binary
    // anyone but root could have replaced.
    let meta =
        std::fs::metadata(&binary).with_context(|| fl!("err-read", path = binary.clone()))?;
    #[cfg(unix)]
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!(fl!("ctl-container-runtime-unsafe", path = binary));
    }
    #[cfg(windows)]
    let _ = meta;
    Ok(EnterCommand {
        binary,
        home: overview.home.clone(),
        id: container.id.clone(),
    })
}

/// `container enter`: replaces this process with an interactive login in
/// the container. Needs root, like the runtime itself.
#[cfg(unix)]
pub async fn enter(client: &DaemonClient, query: &str, user: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let (container, _) = client.container(query).await?;
    if !nix::unistd::Uid::effective().is_root() {
        bail!(fl!(
            "ctl-container-enter-needs-root",
            command = format!("sudo singbox-board container enter {query}")
        ));
    }
    if !std::io::stdin().is_terminal() {
        bail!(fl!("ctl-container-enter-needs-terminal"));
    }
    let overview = client.containers().await?;
    let command = enter_command(&overview, &container)?;
    let error = std::process::Command::new(&command.binary)
        .args(["--name", &command.id, "enter", user])
        .env("HOME", &command.home)
        .exec();
    Err(error).with_context(|| fl!("err-spawn", path = command.binary))
}

pub async fn images(client: &DaemonClient, filter: Option<String>, refresh: bool) -> Result<()> {
    let list = client.container_images(refresh).await?;
    let filter = filter.map(|f| f.to_ascii_lowercase());
    let images: Vec<_> = list
        .images
        .iter()
        .filter(|image| {
            filter
                .as_deref()
                .is_none_or(|f| image.distro.to_ascii_lowercase().contains(f))
        })
        .collect();
    println!(
        "{}\n",
        fl!(
            "ctl-container-images-title",
            server = list.server.clone(),
            arch = list.arch.clone()
        )
    );
    if images.is_empty() {
        println!("{}", fl!("ctl-container-images-empty"));
        return Ok(());
    }
    let width = images
        .iter()
        .map(|i| text_width(&i.spec()))
        .max()
        .unwrap_or(0)
        + 2;
    for image in images {
        println!(
            "  {}{}",
            pad(&image.spec(), width),
            paint(&image.build, "2")
        );
    }
    println!("\n{}", paint(&fl!("ctl-container-images-hint"), "2"));
    Ok(())
}

pub async fn runtime(client: &DaemonClient) -> Result<()> {
    let overview = client.containers().await?;
    let runtime = &overview.runtime;
    let mut rows = vec![("kurumi-containerd".to_owned(), runtime_text(runtime))];
    if let Some(binary) = &runtime.binary {
        rows.push((fl!("ctl-label-binary"), binary.clone()));
    }
    if let Some(tag) = &runtime.tag {
        rows.push((fl!("ctl-label-release"), tag.clone()));
    }
    if runtime.origin.is_some() {
        rows.push((fl!("col-checksum"), checksum_text(runtime.checksum)));
    }
    rows.push((
        fl!("ctl-label-release-build"),
        runtime
            .target
            .clone()
            .unwrap_or_else(|| fl!("ctl-container-runtime-no-build")),
    ));
    rows.push((
        fl!("ctl-label-registry"),
        format!("{}/.kurumi-containerd/config.json", overview.home),
    ));
    if let Some(problem) = &runtime.problem {
        rows.push((fl!("field-error"), paint(problem, "33")));
    }
    print_rows(&rows);
    Ok(())
}

pub async fn runtime_update(client: &DaemonClient, tag: Option<String>, force: bool) -> Result<()> {
    eprintln!("{}", fl!("ctl-container-runtime-checking"));
    println!(
        "{}",
        client
            .command(Request::ContainerRuntimeUpdate { tag, force })
            .await?
    );
    Ok(())
}

pub async fn runtime_import(
    client: &DaemonClient,
    location: String,
    sha256: Option<String>,
) -> Result<()> {
    let is_url = location.starts_with("http://") || location.starts_with("https://");
    let location = match std::fs::canonicalize(&location) {
        Ok(path) if !is_url => path.display().to_string(),
        _ => location,
    };
    println!(
        "{}",
        client
            .command(Request::ContainerRuntimeImport { location, sha256 })
            .await?
    );
    Ok(())
}

/// `container edit`: the built-in editor, or with `external`
/// `$VISUAL`/`$EDITOR` until the daemon accepts the result.
pub async fn edit(client: &DaemonClient, query: &str, force: bool, external: bool) -> Result<()> {
    let (container, original) = client.container(query).await?;
    if !external {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!(fl!("ctl-container-edit-needs-terminal"));
        }
        let message =
            crate::tui::edit_container(client.clone(), container, original, force).await?;
        println!(
            "{}",
            message.unwrap_or_else(|| fl!("ctl-profile-no-changes"))
        );
        return Ok(());
    }
    let mut text = original.clone();
    loop {
        text = crate::util::edit_text_as(&text, &container.name, "toml")?;
        if text == original {
            println!("{}", fl!("ctl-profile-no-changes"));
            return Ok(());
        }
        let request = Request::ContainerSave {
            id: container.id.clone(),
            content: text.clone(),
            force,
        };
        let problem = match client.container_saved(request).await {
            Ok((_, message)) => {
                println!("{message}");
                return Ok(());
            }
            Err(err) => error_chain(&err),
        };
        if !std::io::stdin().is_terminal() {
            bail!(problem);
        }
        eprintln!("{}", fl!("error-line", message = problem));
        if !ask(&fl!("ctl-profile-edit-again"))? {
            bail!(fl!("ctl-profile-discarded"));
        }
    }
}
