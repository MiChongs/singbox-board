//! Plain command-line front-end for scripts and quick checks.

use std::io::{IsTerminal, Write};

use anyhow::{Result, bail};

use crate::client::DaemonClient;
use crate::i18n::fl;
use crate::protocol::{
    Checksum, Component, ComponentAction, ComponentStatus, CoreState, LogSource, Request, Status,
    StoredCore, core_store_id, variant_label, version_label,
};
use crate::substore::{SubStoreClient, provider_snippet};
use crate::util::{error_chain, fmt_bytes, fmt_clock, fmt_duration, now_unix, pad, text_width};

pub async fn status(client: &DaemonClient, json: bool) -> Result<()> {
    let status = client.status().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    print_status(&status);
    Ok(())
}

/// Prints `label  value` rows with the values aligned.
fn print_rows(rows: &[(String, String)]) {
    let width = rows
        .iter()
        .map(|(label, _)| text_width(label))
        .max()
        .unwrap_or(0)
        + 2;
    for (label, value) in rows {
        println!("{}{value}", pad(label, width));
    }
}

/// `text (PID 42, running for 05:06)`
fn with_process(text: String, pid: u32, started: u64, now: u64) -> String {
    fl!(
        "with-process",
        text = text,
        pid = pid.to_string(),
        uptime = fmt_duration(now.saturating_sub(started))
    )
}

fn print_status(status: &Status) {
    let now = now_unix();
    let mut rows = vec![(
        fl!("ctl-label-daemon"),
        with_process(
            format!("v{}", status.daemon_version),
            status.daemon_pid,
            status.daemon_started_at,
            now,
        ),
    )];
    let mut core = status.state.label();
    if let (Some(pid), Some(started)) = (status.pid, status.started_at) {
        core = with_process(core, pid, started, now);
    }
    if let Some(at) = status.next_restart_at {
        core = fl!(
            "with-restart-in",
            text = core,
            seconds = at.saturating_sub(now)
        );
    }
    rows.push(("sing-box".to_owned(), core));
    rows.push((
        fl!("ctl-label-version"),
        status
            .core_version
            .clone()
            .unwrap_or_else(|| fl!("not-installed")),
    ));
    rows.push((fl!("ctl-label-binary"), status.binary.clone()));
    rows.push((fl!("ctl-label-args"), status.args.join(" ")));
    rows.push((
        "Clash API".to_owned(),
        match &status.clash_api {
            Some(api) if api.secret.is_empty() => api.url.clone(),
            Some(api) => fl!("ctl-clash-api-secret", url = api.url.clone()),
            None => fl!("clash-api-not-configured"),
        },
    ));
    rows.push((fl!("ctl-label-restarts"), status.restarts.to_string()));
    if let Some(exit) = &status.last_exit {
        rows.push((fl!("ctl-label-last-exit"), exit.clone()));
    }
    if status.update_in_progress {
        rows.push((fl!("ctl-label-update"), fl!("ctl-update-in-progress")));
    }
    for component in &status.components {
        rows.push((
            component.component.title().to_owned(),
            component_summary(component, now),
        ));
    }
    print_rows(&rows);
    if status.setup_required {
        println!("\n{}", fl!("ctl-setup-hint"));
    }
}

fn component_summary(c: &ComponentStatus, now: u64) -> String {
    if let Some(busy) = c.busy_label() {
        return busy;
    }
    if !c.enabled {
        return fl!("state-disabled");
    }
    let mut text = c.state.label();
    if let (Some(pid), Some(started)) = (c.pid, c.started_at) {
        text = with_process(text, pid, started, now);
    }
    if c.state == CoreState::Running
        && let Some(url) = &c.url
    {
        text.push_str(&format!("  {url}"));
    }
    text
}

/// Answers the first-run question, prompting for whatever was not given.
pub async fn setup(
    client: &DaemonClient,
    sub_store: Option<bool>,
    http_meta: Option<bool>,
) -> Result<()> {
    if sub_store.is_none() || http_meta.is_none() {
        println!("{}\n", fl!("ctl-components-intro"));
    }
    let sub_store = match sub_store {
        Some(answer) => answer,
        None => ask(&fl!("ask-sub-store"))?,
    };
    let http_meta = match http_meta {
        Some(answer) => answer,
        None => ask(&fl!("ask-http-meta"))?,
    };
    if sub_store || http_meta {
        eprintln!("{}", fl!("ctl-setup-installing"));
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
        bail!(fl!("ctl-ask-no-terminal", question = question));
    }
    loop {
        print!("{question} [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!(fl!("ctl-ask-no-answer"));
        }
        match line.trim().to_lowercase().as_str() {
            "" | "n" | "no" | "否" => return Ok(false),
            "y" | "yes" | "是" => return Ok(true),
            _ => println!("{}", fl!("ctl-ask-retry")),
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
            eprintln!("{}", fl!("ctl-may-download"));
        }
        return command(client, Request::Component { component, action }).await;
    }
    let status = client.status().await?;
    let Some(c) = status.components.iter().find(|c| c.component == component) else {
        bail!(fl!("ctl-component-unknown", component = component.title()));
    };
    let now = now_unix();
    let mut rows = vec![
        (component.title().to_owned(), component_summary(c, now)),
        (
            fl!("ctl-label-installed"),
            if c.installed {
                fl!("answer-yes")
            } else {
                fl!("answer-no")
            },
        ),
    ];
    for (name, version) in &c.versions {
        rows.push((version_label(name), version.clone()));
    }
    if c.restarts > 0 {
        rows.push((fl!("ctl-label-restarts"), c.restarts.to_string()));
    }
    if let Some(exit) = &c.last_exit {
        rows.push((fl!("ctl-label-last-exit"), exit.clone()));
    }
    if let Some(url) = &c.url {
        let label = if component == Component::SubStore {
            fl!("ctl-label-web-ui")
        } else {
            fl!("ctl-label-endpoint")
        };
        rows.push((label, url.clone()));
    }
    print_rows(&rows);
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
            println!(
                "\n{}",
                fl!("ctl-subscriptions-error", error = error_chain(&err))
            );
            return;
        }
    };
    if overview.entries.is_empty() {
        println!("\n{}", fl!("ctl-subscriptions-empty"));
        return;
    }
    println!("\n{}", fl!("ctl-subscriptions-title"));
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
        "\n{}\n{}",
        fl!("ctl-provider-title"),
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
            LogSource::Daemon => println!(
                "{} [{}] {}",
                fmt_clock(entry.ts),
                entry.source.label(),
                entry.line
            ),
            source => println!("[{}] {}", source.label(), entry.line),
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
        let date = info
            .published_at
            .as_deref()
            .and_then(|d| d.get(..10))
            .map_or_else(|| fl!("unknown"), str::to_owned);
        let latest = if info.prerelease {
            fl!(
                "ctl-latest-prerelease",
                version = info.latest.clone(),
                date = date
            )
        } else {
            fl!("ctl-latest", version = info.latest.clone(), date = date)
        };
        print_rows(&[
            (
                fl!("ctl-label-current"),
                info.current.clone().unwrap_or_else(|| fl!("none")),
            ),
            (fl!("ctl-label-latest"), latest),
            (fl!("ctl-label-asset"), info.asset.clone()),
        ]);
        println!(
            "{}",
            if info.update_available {
                fl!("ctl-update-available")
            } else {
                fl!("ctl-up-to-date")
            }
        );
        return Ok(());
    }
    eprintln!("{}", fl!("ctl-update-downloading"));
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

fn checksum_text(checksum: Checksum) -> String {
    match checksum {
        Checksum::None => paint(&checksum.label(), "33"),
        verified => paint(&format!("✓ {}", verified.label()), "32"),
    }
}

/// `core` without arguments: the active core and the version store.
pub async fn core_overview(client: &DaemonClient) -> Result<()> {
    let status = client.status().await?;
    match &status.active_core {
        Some(core) => println!(
            "{} sing-box {}  {}  {}  {}",
            paint("●", "32"),
            paint(&core.version, "1"),
            core.source_label(),
            variant_label(&core.variant),
            checksum_text(core.checksum)
        ),
        None => match &status.core_version {
            Some(version) => println!(
                "{} {}",
                paint("●", "33"),
                fl!(
                    "ctl-core-unmanaged",
                    version = version.clone(),
                    binary = status.binary.clone()
                )
            ),
            None => println!("{} {}", paint("○", "31"), fl!("ctl-core-none")),
        },
    }
    println!();
    core_installed(client).await?;
    println!(
        "\n{}\n{}",
        paint(&fl!("ctl-core-hint-list"), "2"),
        paint(&fl!("ctl-core-hint-switch"), "2")
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
        let kind = if source.builtin {
            String::new()
        } else {
            format!(" [{}]", fl!("source-custom-tag"))
        };
        println!(
            "{marker} {} {}{}\n    {}",
            pad(&source.id, 26),
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
    let title = if page.has_more {
        fl!(
            "ctl-releases-title-more",
            source = paint(&page.source, "1"),
            platform = page.platform.clone(),
            page = page.page,
            next = page.page.saturating_add(1)
        )
    } else {
        fl!(
            "ctl-releases-title",
            source = paint(&page.source, "1"),
            platform = page.platform.clone(),
            page = page.page
        )
    };
    println!("{title}");
    let pre_label = fl!("ctl-prerelease-marker");
    let pre_width = text_width(&pre_label) + 1;
    let version_width = 30 + pre_width;
    println!(
        "  {} {} {}",
        paint(&pad(&fl!("ctl-col-version"), version_width), "2"),
        paint(&pad(&fl!("ctl-col-published"), 11), "2"),
        paint(&fl!("ctl-col-variants"), "2")
    );
    for release in page.releases.iter().filter(|r| !stable || !r.prerelease) {
        let variants: Vec<String> = release
            .variants
            .iter()
            .map(|v| {
                let id = core_store_id(&page.source, &release.tag, &v.name);
                let name = variant_label(&v.name);
                match installed.iter().find(|c| c.id == id) {
                    Some(core) if core.active => paint(&format!("●{name}"), "32;1"),
                    Some(_) => paint(&format!("✓{name}"), "34"),
                    None => name,
                }
            })
            .collect();
        let pre = if release.prerelease {
            paint(&pad(&format!(" {pre_label}"), pre_width), "33")
        } else {
            " ".repeat(pre_width)
        };
        let date = release
            .published_at
            .as_deref()
            .unwrap_or("")
            .get(..10)
            .unwrap_or("");
        let variants = if variants.is_empty() {
            paint(&fl!("ctl-no-build"), "2")
        } else {
            variants.join(" ")
        };
        println!(
            "  {}{pre} {} {variants}",
            pad(&release.version, 30),
            pad(date, 11)
        );
    }
    println!(
        "\n{}",
        paint(&fl!("ctl-install-hint", source = page.source.clone()), "2")
    );
    Ok(())
}

pub async fn core_installed(client: &DaemonClient) -> Result<()> {
    let cores = client.core_installed().await?;
    if cores.is_empty() {
        println!("{}", fl!("ctl-store-empty"));
        return Ok(());
    }
    println!("{}", paint(&fl!("ctl-store-title"), "2"));
    for core in cores {
        let marker = if core.active {
            paint("●", "32")
        } else {
            " ".to_owned()
        };
        let size = fmt_bytes(core.size);
        println!(
            "{marker} {} {} {} {}{size}  {}\n    {}",
            pad(&core.version, 32),
            pad(&core.source_label(), 24),
            pad(&variant_label(&core.variant), 10),
            " ".repeat(10usize.saturating_sub(text_width(&size))),
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
        [] => bail!(fl!("ctl-core-not-found", query = query)),
        many => bail!(
            "{}\n  {}",
            fl!("ctl-core-ambiguous", query = query),
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
    // `--variant default` (or its translation, as listed by `core list`)
    // names the standard build, which the protocol calls "".
    let variant = if variant == "default" || variant == variant_label("") {
        String::new()
    } else {
        variant
    };
    eprintln!(
        "{}",
        fl!(
            "ctl-core-downloading",
            source = source.clone(),
            tag = tag.clone(),
            variant = variant_label(&variant)
        )
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
