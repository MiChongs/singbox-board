//! Plain command-line front-end for scripts and quick checks.

use std::io::{IsTerminal, Write};

use anyhow::{Result, bail};

use crate::client::DaemonClient;
use crate::protocol::{
    Checksum, Component, ComponentAction, ComponentStatus, CoreState, LogSource, Request, Status,
    StoredCore, core_store_id,
};
use crate::substore::{SubStoreClient, provider_snippet};
use crate::util::{fmt_bytes, fmt_clock, fmt_duration, now_unix};

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
    for component in &status.components {
        println!(
            "{:<11} {}",
            component.component.name(),
            component_summary(component, now)
        );
    }
    if status.setup_required {
        println!(
            "\nfirst run: choose the optional components (Sub-Store, http-meta) \
             with `singbox-board setup` or in the TUI"
        );
    }
}

fn component_summary(c: &ComponentStatus, now: u64) -> String {
    if let Some(busy) = &c.busy {
        return format!("{busy}…");
    }
    if !c.enabled {
        return "disabled".to_owned();
    }
    let mut text = c.state.label().to_owned();
    if let (Some(pid), Some(started)) = (c.pid, c.started_at) {
        text.push_str(&format!(
            " (pid {pid}, up {})",
            fmt_duration(now.saturating_sub(started))
        ));
    }
    if c.state == CoreState::Running
        && let Some(url) = &c.url
    {
        text.push_str(&format!("  {url}"));
    }
    text
}

const COMPONENT_HELP: &str = "\
singbox-board can also install and supervise two optional components:
  Sub-Store  subscription manager with a web UI; converts subscriptions into
             sing-box format for `providers` (sub-store-org/Sub-Store)
  http-meta  starts mihomo on demand so Sub-Store scripts can test whether
             nodes are reachable (xream/http-meta)
Both run as an unprivileged user; Node.js is downloaded if none is installed.
";

/// Answers the first-run question, prompting for whatever was not given.
pub async fn setup(
    client: &DaemonClient,
    sub_store: Option<bool>,
    http_meta: Option<bool>,
) -> Result<()> {
    if sub_store.is_none() || http_meta.is_none() {
        println!("{COMPONENT_HELP}");
    }
    let sub_store = match sub_store {
        Some(answer) => answer,
        None => ask("Enable Sub-Store?")?,
    };
    let http_meta = match http_meta {
        Some(answer) => answer,
        None => ask("Enable http-meta?")?,
    };
    if sub_store || http_meta {
        eprintln!("installing and starting, this may take a while…");
    }
    command(
        client,
        Request::Setup {
            sub_store,
            http_meta,
        },
    )
    .await
}

fn ask(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("{question} needs an answer; pass --sub-store/--http-meta yes|no");
    }
    loop {
        print!("{question} [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!("no answer given");
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" | "n" | "no" => return Ok(false),
            "y" | "yes" => return Ok(true),
            _ => println!("please answer y or n"),
        }
    }
}

pub async fn component(
    client: &DaemonClient,
    component: Component,
    action: Option<ComponentAction>,
) -> Result<()> {
    if let Some(action) = action {
        if matches!(action, ComponentAction::Enable | ComponentAction::Update) {
            eprintln!("this may download files and take a while…");
        }
        return command(client, Request::Component { component, action }).await;
    }
    let status = client.status().await?;
    let Some(c) = status.components.iter().find(|c| c.component == component) else {
        bail!("daemon does not report {}", component.name());
    };
    let now = now_unix();
    println!("{:<11} {}", component.title(), component_summary(c, now));
    println!("installed   {}", if c.installed { "yes" } else { "no" });
    for (name, version) in &c.versions {
        println!("{:<11} {version}", name);
    }
    if c.restarts > 0 {
        println!("restarts    {}", c.restarts);
    }
    if let Some(exit) = &c.last_exit {
        println!("last exit   {exit}");
    }
    if let Some(url) = &c.url {
        let label = if component == Component::SubStore {
            "web ui"
        } else {
            "endpoint"
        };
        println!("{label:<11} {url}");
    }
    if component == Component::SubStore
        && c.state == CoreState::Running
        && let Some(api) = &c.api
    {
        print_sub_store_entries(api).await;
    }
    Ok(())
}

async fn print_sub_store_entries(api: &str) {
    let overview = match SubStoreClient::new(api) {
        Ok(client) => client.overview().await,
        Err(err) => Err(err),
    };
    let overview = match overview {
        Ok(overview) => overview,
        Err(err) => {
            println!("\ncannot list subscriptions: {err:#}");
            return;
        }
    };
    if overview.entries.is_empty() {
        println!("\nno subscriptions yet; add them in the web UI");
        return;
    }
    println!("\nsing-box subscription URLs:");
    for entry in &overview.entries {
        println!(
            "  [{}] {}\n      {}",
            entry.kind.label(),
            entry.name,
            entry.singbox_url
        );
    }
    let first = &overview.entries[0];
    println!(
        "\nprovider for sing-box (add to your configuration):\n{}",
        provider_snippet(&first.name, &first.singbox_url)
    );
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
            source => println!("[{}] {}", source.tag(), entry.line),
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

// ----- core versions --------------------------------------------------------

fn paint(text: &str, code: &str) -> String {
    if std::io::stdout().is_terminal() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn variant_name(variant: &str) -> &str {
    if variant.is_empty() {
        "default"
    } else {
        variant
    }
}

fn checksum_text(checksum: Checksum) -> String {
    match checksum {
        Checksum::Sums => paint("✓ SHA256SUMS", "32"),
        Checksum::Digest => paint("✓ digest", "32"),
        Checksum::Pinned => paint("✓ pinned", "32"),
        Checksum::None => paint("unverified", "33"),
    }
}

/// `core` without arguments: the active core and the version store.
pub async fn core_overview(client: &DaemonClient) -> Result<()> {
    let status = client.status().await?;
    match &status.active_core {
        Some(core) => println!(
            "{} sing-box {}  {} · {}  {}",
            paint("●", "32"),
            paint(&core.version, "1"),
            core.source_name,
            variant_name(&core.variant),
            checksum_text(core.checksum)
        ),
        None => match &status.core_version {
            Some(version) => println!(
                "{} sing-box {version} at {} (not managed by the version store)",
                paint("●", "33"),
                status.binary
            ),
            None => println!("{} no sing-box core installed", paint("○", "31")),
        },
    }
    println!();
    core_installed(client).await?;
    println!(
        "\n{}",
        paint(
            "list releases: singbox-board core list · switch: singbox-board core install <tag> / core use <id>",
            "2"
        )
    );
    Ok(())
}

pub async fn core_sources(client: &DaemonClient) -> Result<()> {
    let (sources, default) = client.core_sources().await?;
    for source in sources {
        let marker = if source.id == default {
            paint("●", "32")
        } else {
            " ".to_owned()
        };
        let kind = if source.builtin { "" } else { " [custom]" };
        println!(
            "{marker} {:<26} {}{}\n    {}",
            source.id,
            paint(&source.name, "1"),
            kind,
            paint(&source.description, "2")
        );
    }
    Ok(())
}

pub async fn core_list(
    client: &DaemonClient,
    source: Option<String>,
    page: u32,
    stable: bool,
    refresh: bool,
) -> Result<()> {
    let source = match source {
        Some(source) => source,
        None => client.core_sources().await?.1,
    };
    let page = client.core_releases(&source, page, refresh).await?;
    let installed = client.core_installed().await?;
    println!(
        "{} · {} · page {}{}",
        paint(&page.source, "1"),
        page.platform,
        page.page,
        if page.has_more {
            format!(" (more: --page {})", page.page + 1)
        } else {
            String::new()
        }
    );
    println!(
        "  {:<34} {:<11} {}",
        paint("VERSION", "2"),
        paint("PUBLISHED", "2"),
        paint("VARIANTS (● active, ✓ stored)", "2")
    );
    for release in page.releases.iter().filter(|r| !stable || !r.prerelease) {
        let variants: Vec<String> = release
            .variants
            .iter()
            .map(|v| {
                let id = core_store_id(&page.source, &release.tag, &v.name);
                match installed.iter().find(|c| c.id == id) {
                    Some(core) if core.active => {
                        paint(&format!("●{}", variant_name(&v.name)), "32;1")
                    }
                    Some(_) => paint(&format!("✓{}", variant_name(&v.name)), "34"),
                    None => variant_name(&v.name).to_owned(),
                }
            })
            .collect();
        let pre = if release.prerelease {
            paint(" pre", "33")
        } else {
            "    ".to_owned()
        };
        let date = release
            .published_at
            .as_deref()
            .unwrap_or("")
            .get(..10)
            .unwrap_or("");
        let variants = if variants.is_empty() {
            paint("no build for this platform", "2")
        } else {
            variants.join(" ")
        };
        println!("  {:<30}{pre} {:<11} {variants}", release.version, date);
    }
    println!(
        "\n{}",
        paint(
            &format!(
                "install: singbox-board core install <tag> --source {} [--variant <name>]",
                page.source
            ),
            "2"
        )
    );
    Ok(())
}

pub async fn core_installed(client: &DaemonClient) -> Result<()> {
    let cores = client.core_installed().await?;
    if cores.is_empty() {
        println!("no cores in the version store yet");
        return Ok(());
    }
    println!("{}", paint("installed cores:", "2"));
    for core in cores {
        let marker = if core.active {
            paint("●", "32")
        } else {
            " ".to_owned()
        };
        println!(
            "{marker} {:<32} {:<24} {:<10} {:>10}  {}\n    {}",
            core.version,
            core.source_name,
            variant_name(&core.variant),
            fmt_bytes(core.size),
            checksum_text(core.checksum),
            paint(&core.id, "2")
        );
    }
    Ok(())
}

/// Resolves a store id, version or tag to a stored core id.
async fn resolve_core(client: &DaemonClient, query: &str) -> Result<String> {
    let cores = client.core_installed().await?;
    if let Some(core) = cores.iter().find(|c| c.id == query) {
        return Ok(core.id.clone());
    }
    let matches: Vec<&StoredCore> = cores
        .iter()
        .filter(|c| {
            c.version == query
                || c.tag.as_deref() == Some(query)
                || c.tag.as_deref() == Some(&format!("v{query}"))
        })
        .collect();
    match matches.as_slice() {
        [core] => Ok(core.id.clone()),
        [] => bail!("no stored core matches {query:?}; see `singbox-board core installed`"),
        many => bail!(
            "{query:?} matches several cores, pass an id:\n  {}",
            many.iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ),
    }
}

pub async fn core_install(
    client: &DaemonClient,
    tag: String,
    source: Option<String>,
    variant: String,
    activate: bool,
    force: bool,
) -> Result<()> {
    let source = match source {
        Some(source) => source,
        None => client.core_sources().await?.1,
    };
    let variant = if variant == "default" {
        String::new()
    } else {
        variant
    };
    eprintln!(
        "downloading {} {tag} ({}) if it is not stored yet…",
        source,
        variant_name(&variant)
    );
    command(
        client,
        Request::CoreInstall {
            source,
            tag,
            variant,
            activate,
            force,
        },
    )
    .await
}

pub async fn core_use(client: &DaemonClient, query: &str, force: bool) -> Result<()> {
    let id = resolve_core(client, query).await?;
    command(client, Request::CoreActivate { id, force }).await
}

pub async fn core_remove(client: &DaemonClient, query: &str) -> Result<()> {
    let id = resolve_core(client, query).await?;
    command(client, Request::CoreRemove { id }).await
}
