mod clash;
mod cli;
mod client;
mod config;
mod container;
mod ctl;
mod ctl_container;
mod daemon;
mod i18n;
mod profile;
mod protocol;
mod substore;
mod tray;
mod tui;
mod util;
#[cfg(windows)]
mod win;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::client::DaemonClient;
use crate::config::DaemonConfig;
use crate::i18n::{Lang, fl, fl_log};
#[cfg(unix)]
use crate::protocol::ContainerAction;
use crate::protocol::{Component, ComponentAction, Request};
use crate::util::error_chain;

#[derive(Parser)]
#[command(
    name = "singbox-board",
    version,
    about = fl!("cli-about"),
    long_about = fl!("cli-long-about"),
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "SINGBOX_BOARD_SOCKET",
        hide_env = true,
        value_name = "PATH",
        display_order = 100,
        help = fl!("cli-socket", default = config::default_socket().display().to_string())
    )]
    socket: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        env = "SINGBOX_BOARD_LANG",
        hide_env = true,
        value_name = "LANG",
        value_parser = parse_lang,
        display_order = 101,
        help = fl!("cli-lang")
    )]
    lang: Option<String>,

    #[arg(
        short,
        long,
        global = true,
        action = ArgAction::Help,
        display_order = 102,
        help = fl!("cli-help")
    )]
    help: Option<bool>,

    #[arg(
        short = 'V',
        long,
        action = ArgAction::Version,
        display_order = 103,
        help = fl!("cli-version")
    )]
    version: Option<bool>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = fl!("cli-tui"))]
    Tui,
    #[cfg_attr(unix, command(about = fl!("cli-tray")))]
    #[cfg_attr(windows, command(about = fl!("win-cli-tray")))]
    Tray,
    #[command(about = fl!("cli-daemon"))]
    Daemon {
        #[arg(short, long, value_name = "FILE", help = fl!("cli-daemon-config"))]
        config: Option<PathBuf>,
        #[arg(long, help = fl!("cli-daemon-allow-non-root"))]
        allow_non_root: bool,
        #[arg(long, help = fl!("cli-daemon-print-default-config"))]
        print_default_config: bool,
        /// Started by the Windows service control manager.
        #[arg(long, hide = true)]
        service: bool,
    },
    #[command(about = fl!("cli-status"))]
    Status {
        #[arg(long, help = fl!("cli-status-json"))]
        json: bool,
    },
    #[command(about = fl!("cli-start"))]
    Start,
    #[command(about = fl!("cli-stop"))]
    Stop,
    #[command(about = fl!("cli-restart"))]
    Restart,
    #[command(about = fl!("cli-reload"))]
    Reload,
    #[command(about = fl!("cli-check"))]
    Check,
    #[command(about = fl!("cli-logs"))]
    Logs {
        #[arg(
            short = 'n',
            long,
            default_value_t = 200,
            hide_default_value = true,
            help = fl!("cli-logs-tail")
        )]
        tail: usize,
        #[arg(short, long, help = fl!("cli-logs-follow"))]
        follow: bool,
    },
    #[command(about = fl!("cli-update"))]
    Update {
        #[arg(long, help = fl!("cli-update-check"))]
        check: bool,
        #[arg(long, help = fl!("cli-update-tag"))]
        tag: Option<String>,
        #[arg(long, help = fl!("cli-update-force"))]
        force: bool,
    },
    #[command(about = fl!("cli-setup"))]
    Setup {
        #[arg(
            long,
            value_name = "BOOL",
            value_parser = clap::builder::BoolishValueParser::new(),
            help = fl!("cli-setup-sub-store")
        )]
        sub_store: Option<bool>,
        #[arg(
            long,
            value_name = "BOOL",
            value_parser = clap::builder::BoolishValueParser::new(),
            help = fl!("cli-setup-http-meta")
        )]
        http_meta: Option<bool>,
    },
    #[command(about = fl!("cli-component"))]
    Component {
        #[arg(value_enum, hide_possible_values = true, help = fl!("cli-component-name"))]
        component: Component,
        #[arg(value_enum, hide_possible_values = true, help = fl!("cli-component-action"))]
        action: Option<ComponentAction>,
    },
    #[command(about = fl!("cli-core"))]
    Core {
        #[command(subcommand)]
        action: Option<CoreCmd>,
    },
    #[command(about = fl!("cli-profile"), visible_alias = "config")]
    Profile {
        #[command(subcommand)]
        action: Option<ProfileCmd>,
    },
    // kurumi-containerd needs Linux.
    #[command(about = fl!("cli-container"), visible_alias = "ct", hide = cfg!(windows))]
    Container {
        #[command(subcommand)]
        action: Option<ContainerCmd>,
    },
}

#[derive(Subcommand)]
enum ContainerCmd {
    #[command(about = fl!("cli-container-list"))]
    List,
    #[command(about = fl!("cli-container-show"))]
    Show {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(long, help = fl!("cli-container-show-config"))]
        config: bool,
    },
    #[command(about = fl!("cli-container-new"))]
    New {
        #[arg(help = fl!("cli-container-name"))]
        name: String,
        #[arg(
            long,
            value_parser = ["host", "nat", "none"],
            hide_possible_values = true,
            help = fl!("cli-container-network")
        )]
        network: Option<String>,
        #[arg(long, value_name = "DISTRO/RELEASE", help = fl!("cli-container-new-image"))]
        image: Option<String>,
        #[arg(long, help = fl!("cli-container-new-edit"))]
        edit: bool,
        #[arg(long, help = fl!("cli-container-new-start"))]
        start: bool,
    },
    #[command(about = fl!("cli-container-add"))]
    Add {
        #[arg(value_name = "FILE", help = fl!("cli-container-add-file"))]
        file: String,
        #[arg(long, help = fl!("cli-container-name"))]
        name: Option<String>,
        #[arg(long, help = fl!("cli-container-add-link"))]
        link: bool,
    },
    #[command(about = fl!("cli-container-adopt"))]
    Adopt {
        #[arg(value_name = "CONFIG.JSON", help = fl!("cli-container-adopt-registry"))]
        registry: Option<String>,
    },
    #[command(about = fl!("cli-container-edit"))]
    Edit {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(long, help = fl!("cli-container-force-save"))]
        force: bool,
        #[arg(long, help = fl!("cli-profile-edit-external"))]
        external: bool,
    },
    #[command(about = fl!("cli-container-start"))]
    Start {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
    },
    #[command(about = fl!("cli-container-stop"))]
    Stop {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
    },
    #[command(about = fl!("cli-container-restart"))]
    Restart {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
    },
    #[command(about = fl!("cli-container-install"))]
    Install {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(value_name = "ARCHIVE|URL|DISTRO/RELEASE", help = fl!("cli-container-install-source"))]
        source: String,
        #[arg(long, help = fl!("cli-container-install-size"))]
        size: Option<String>,
        #[arg(long, help = fl!("cli-container-install-sha256"))]
        sha256: Option<String>,
        #[arg(long, help = fl!("cli-container-install-force"))]
        force: bool,
    },
    #[command(about = fl!("cli-container-exec"), visible_alias = "run")]
    Exec {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(long, value_name = "SECONDS", help = fl!("cli-container-exec-timeout"))]
        timeout: Option<u64>,
        #[arg(
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "COMMAND",
            help = fl!("cli-container-exec-command")
        )]
        command: Vec<String>,
    },
    #[command(about = fl!("cli-container-enter"), visible_alias = "shell")]
    Enter {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(default_value = "root", hide_default_value = true, help = fl!("cli-container-enter-user"))]
        user: String,
    },
    #[command(about = fl!("cli-container-autostart"))]
    Autostart {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(
            value_name = "on|off",
            action = ArgAction::Set,
            value_parser = clap::builder::BoolishValueParser::new(),
            help = fl!("cli-container-autostart-value")
        )]
        enabled: bool,
    },
    #[command(about = fl!("cli-container-rename"))]
    Rename {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(help = fl!("cli-container-name"))]
        name: String,
    },
    #[command(about = fl!("cli-container-remove"))]
    Remove {
        #[arg(help = fl!("cli-container-id"))]
        container: String,
        #[arg(long, help = fl!("cli-container-remove-purge"))]
        purge: bool,
    },
    #[command(about = fl!("cli-container-images"))]
    Images {
        #[arg(value_name = "DISTRO", help = fl!("cli-container-images-filter"))]
        distro: Option<String>,
        #[arg(long, help = fl!("cli-core-list-refresh"))]
        refresh: bool,
    },
    #[command(about = fl!("cli-container-check"))]
    Check,
    #[command(about = fl!("cli-container-scan"))]
    Scan,
    #[command(about = fl!("cli-container-runtime"))]
    Runtime {
        #[command(subcommand)]
        action: Option<RuntimeCmd>,
    },
}

#[derive(Subcommand)]
enum RuntimeCmd {
    #[command(about = fl!("cli-container-runtime-update"))]
    Update {
        #[arg(long, help = fl!("cli-container-runtime-tag"))]
        tag: Option<String>,
        #[arg(long, help = fl!("cli-update-force"))]
        force: bool,
    },
    #[command(about = fl!("cli-container-runtime-import"))]
    Import {
        #[arg(help = fl!("cli-container-runtime-location"))]
        location: String,
        #[arg(long, help = fl!("cli-core-import-sha256"))]
        sha256: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    #[command(about = fl!("cli-profile-list"))]
    List,
    #[command(about = fl!("cli-profile-add"))]
    Add {
        #[arg(value_name = "FILE|URL", help = fl!("cli-profile-add-source"))]
        source: String,
        #[arg(long, help = fl!("cli-profile-name"))]
        name: Option<String>,
        #[arg(long, value_name = "MINUTES", help = fl!("cli-profile-interval"))]
        interval: Option<u64>,
        #[arg(long = "use", help = fl!("cli-profile-use-now"))]
        activate: bool,
    },
    #[command(about = fl!("cli-profile-new"))]
    New {
        #[arg(help = fl!("cli-profile-name"))]
        name: String,
        #[arg(long, help = fl!("cli-profile-new-edit"))]
        edit: bool,
        #[arg(long = "use", help = fl!("cli-profile-use-now"))]
        activate: bool,
    },
    #[command(about = fl!("cli-profile-use"))]
    Use {
        #[arg(help = fl!("cli-profile-id"))]
        profile: String,
        #[arg(long, help = fl!("cli-profile-force-use"))]
        force: bool,
    },
    #[command(about = fl!("cli-profile-show"))]
    Show {
        #[arg(help = fl!("cli-profile-id"))]
        profile: String,
    },
    #[command(about = fl!("cli-profile-edit"))]
    Edit {
        #[arg(help = fl!("cli-profile-id"))]
        profile: String,
        #[arg(long, help = fl!("cli-profile-force-save"))]
        force: bool,
        #[arg(long, help = fl!("cli-profile-edit-external"))]
        external: bool,
    },
    #[command(about = fl!("cli-profile-update"))]
    Update {
        #[arg(help = fl!("cli-profile-update-id"))]
        profile: Option<String>,
        #[arg(long, help = fl!("cli-profile-force-save"))]
        force: bool,
    },
    #[command(about = fl!("cli-profile-set"))]
    Set {
        #[arg(help = fl!("cli-profile-id"))]
        profile: String,
        #[arg(long, help = fl!("cli-profile-rename"))]
        name: Option<String>,
        #[arg(long, value_name = "URL", help = fl!("cli-profile-url"))]
        url: Option<String>,
        #[arg(long, value_name = "MINUTES", help = fl!("cli-profile-interval"))]
        interval: Option<u64>,
        #[arg(long, conflicts_with = "url", help = fl!("cli-profile-local"))]
        local: bool,
    },
    #[command(about = fl!("cli-profile-check"))]
    Check {
        #[arg(help = fl!("cli-profile-id"))]
        profile: String,
    },
    #[command(about = fl!("cli-profile-remove"))]
    Remove {
        #[arg(help = fl!("cli-profile-id"))]
        profile: String,
    },
    #[command(about = fl!("cli-profile-adopt"))]
    Adopt,
}

#[derive(Subcommand)]
enum CoreCmd {
    #[command(about = fl!("cli-core-sources"))]
    Sources,
    #[command(about = fl!("cli-core-source"))]
    Source {
        #[command(subcommand)]
        action: SourceCmd,
    },
    #[command(about = fl!("cli-core-list"))]
    List {
        #[arg(long, value_name = "OWNER/REPO", help = fl!("cli-core-source-option"))]
        source: Option<String>,
        #[arg(
            long,
            default_value_t = 1,
            hide_default_value = true,
            help = fl!("cli-core-list-page")
        )]
        page: u32,
        #[arg(long, help = fl!("cli-core-list-stable"))]
        stable: bool,
        #[arg(long, help = fl!("cli-core-list-refresh"))]
        refresh: bool,
    },
    #[command(about = fl!("cli-core-installed"))]
    Installed,
    #[command(about = fl!("cli-core-install"))]
    Install {
        #[arg(help = fl!("cli-core-install-tag"))]
        tag: String,
        #[arg(long, value_name = "OWNER/REPO", help = fl!("cli-core-source-option"))]
        source: Option<String>,
        #[arg(
            long,
            default_value = "",
            hide_default_value = true,
            help = fl!("cli-core-install-variant")
        )]
        variant: String,
        #[arg(long, help = fl!("cli-core-no-switch"))]
        no_switch: bool,
        #[arg(long, help = fl!("cli-core-force"))]
        force: bool,
    },
    #[command(about = fl!("cli-core-use"))]
    Use {
        #[arg(help = fl!("cli-core-id"))]
        core: String,
        #[arg(long, help = fl!("cli-core-force"))]
        force: bool,
    },
    #[command(about = fl!("cli-core-remove"))]
    Remove {
        #[arg(help = fl!("cli-core-id"))]
        core: String,
    },
    #[command(about = fl!("cli-core-import"))]
    Import {
        #[arg(help = fl!("cli-core-import-location"))]
        location: String,
        #[arg(long, help = fl!("cli-core-import-sha256"))]
        sha256: Option<String>,
        #[arg(long, help = fl!("cli-core-no-switch"))]
        no_switch: bool,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    #[command(about = fl!("cli-source-add"))]
    Add {
        #[arg(value_name = "OWNER/REPO", help = fl!("cli-source-add-repo"))]
        repo: String,
        #[arg(long, help = fl!("cli-source-add-name"))]
        name: Option<String>,
    },
    #[command(about = fl!("cli-source-remove"))]
    Remove {
        #[arg(value_name = "OWNER/REPO", help = fl!("cli-source-remove-repo"))]
        repo: String,
    },
}

fn parse_lang(value: &str) -> Result<String, cli::LocalizedError> {
    match i18n::resolve(Some(value)) {
        Ok(_) => Ok(value.to_owned()),
        Err(value) => Err(cli::LocalizedError(fl!("cli-lang-invalid", value = value))),
    }
}

/// The language of this run, read before clap so that help and parse errors
/// are localized too: `--lang`, then `SINGBOX_BOARD_LANG`, then the locale.
fn initial_language() -> Lang {
    let mut args = std::env::args().skip(1);
    let mut flag = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--lang" {
            flag = args.next();
        } else if let Some(value) = arg.strip_prefix("--lang=") {
            flag = Some(value.to_owned());
        }
    }
    flag.or_else(|| std::env::var("SINGBOX_BOARD_LANG").ok())
        .and_then(|value| i18n::resolve(Some(&value)).ok())
        .unwrap_or_else(|| i18n::resolve(None).unwrap_or(Lang::En))
}

fn parse_cli() -> Cli {
    let matches = cli::localize(Cli::command())
        .try_get_matches()
        .unwrap_or_else(|err| cli::exit(err));
    Cli::from_arg_matches(&matches).unwrap_or_else(|err| cli::exit(err))
}

fn main() -> ExitCode {
    i18n::set_language(initial_language());
    let cli = parse_cli();
    let explicit_lang = cli.lang.is_some();
    #[cfg(windows)]
    let tray = matches!(cli.command, Some(Cmd::Tray));
    let result = match cli.command.unwrap_or(Cmd::Tui) {
        Cmd::Daemon {
            config,
            allow_non_root,
            print_default_config,
            service,
        } => {
            if print_default_config {
                print!("{}", config::template());
                return ExitCode::SUCCESS;
            }
            #[cfg(windows)]
            if service {
                let socket = cli.socket;
                win::service::dispatch(move || {
                    run_daemon(socket, config, allow_non_root, explicit_lang, true)
                })
            } else {
                run_daemon(cli.socket, config, allow_non_root, explicit_lang, false)
            }
            #[cfg(unix)]
            run_daemon(cli.socket, config, allow_non_root, explicit_lang, service)
        }
        command => run_client(cli.socket, cli.lang, command),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let message = fl!("error-line", message = error_chain(&err));
            // The tray runs detached, with nowhere to print to, when its
            // launcher starts it from the Start menu or at sign-in.
            #[cfg(windows)]
            if tray && !win::has_stderr() {
                win::message_box("singbox-board", &message, true);
                return ExitCode::FAILURE;
            }
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// `service`: started by the Windows service control manager, which gives
/// the daemon no stderr; its output goes to `logs\daemon.log` next to
/// daemon.toml instead.
fn run_daemon(
    socket: Option<PathBuf>,
    config_path: Option<PathBuf>,
    allow_non_root: bool,
    explicit_lang: bool,
    service: bool,
) -> Result<()> {
    let explicit = config_path.is_some();
    let path = config_path.unwrap_or_else(config::default_config_path);
    #[cfg(windows)]
    if service {
        // Before the console: allocating one resets the standard handles.
        win::process::ensure_console();
        let log = path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("logs")
            .join("daemon.log");
        win::service::redirect_output(&log)?;
    }
    #[cfg(unix)]
    let _ = service;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(!service)
        .with_writer(std::io::stderr)
        .init();
    let (mut config, found) = DaemonConfig::load(&path, explicit)?;
    // `--lang` / SINGBOX_BOARD_LANG win over daemon.toml.
    if !explicit_lang {
        match i18n::resolve(Some(&config.language)) {
            Ok(lang) => i18n::set_language(lang),
            Err(value) => tracing::warn!("{}", fl_log!("daemon-language-invalid", value = value)),
        }
    }
    let path_text = path.display().to_string();
    if found {
        tracing::info!("{}", fl_log!("daemon-config-loaded", path = path_text));
    } else {
        tracing::info!("{}", fl_log!("daemon-config-missing", path = path_text));
    }
    if let Some(socket) = socket {
        config.socket = socket;
    }
    // A single-threaded runtime: the supervisor spawns sing-box from the main
    // thread, which keeps PR_SET_PDEATHSIG tied to the daemon's lifetime.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(daemon::run(config, allow_non_root))
}

fn run_client(socket: Option<PathBuf>, lang: Option<String>, command: Cmd) -> Result<()> {
    #[cfg(unix)]
    if !matches!(command, Cmd::Tui | Cmd::Tray) {
        // Behave like other CLI tools in pipes (`singbox-board core | head`):
        // exit quietly on a closed stdout instead of panicking. The daemon
        // keeps ignoring SIGPIPE so a vanished client cannot kill it.
        // SAFETY: resetting a signal disposition before any thread exists.
        unsafe {
            let _ = nix::sys::signal::signal(
                nix::sys::signal::Signal::SIGPIPE,
                nix::sys::signal::SigHandler::SigDfl,
            );
        }
    }
    let options = tray::Options {
        socket: socket.clone(),
        lang,
    };
    let socket = socket.unwrap_or_else(client_socket);
    let client = DaemonClient::new(socket);
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        match command {
            Cmd::Tui => tui::run(client).await,
            Cmd::Tray => tray::run(client, options).await,
            Cmd::Status { json } => ctl::status(&client, json).await,
            Cmd::Start => ctl::command(&client, Request::Start).await,
            Cmd::Stop => ctl::command(&client, Request::Stop).await,
            Cmd::Restart => ctl::command(&client, Request::Restart).await,
            Cmd::Reload => ctl::command(&client, Request::Reload).await,
            Cmd::Check => ctl::command(&client, Request::Check).await,
            Cmd::Logs { tail, follow } => ctl::logs(&client, tail, follow).await,
            Cmd::Update { check, tag, force } => ctl::update(&client, check, tag, force).await,
            Cmd::Setup {
                sub_store,
                http_meta,
            } => ctl::setup(&client, sub_store, http_meta).await,
            Cmd::Component { component, action } => {
                ctl::component(&client, component, action).await
            }
            Cmd::Core { action } => run_core(&client, action).await,
            Cmd::Profile { action } => run_profile(&client, action).await,
            // kurumi-containerd needs Linux.
            #[cfg(windows)]
            Cmd::Container { .. } => Err(anyhow::anyhow!(fl!("win-containers-unsupported"))),
            #[cfg(unix)]
            Cmd::Container { action } => run_container(&client, action).await,
            Cmd::Daemon { .. } => unreachable!("handled in main"),
        }
    })
}

async fn run_core(client: &DaemonClient, action: Option<CoreCmd>) -> Result<()> {
    match action {
        None => ctl::core_overview(client).await,
        Some(CoreCmd::Sources) => ctl::core_sources(client).await,
        Some(CoreCmd::Source { action }) => match action {
            SourceCmd::Add { repo, name } => {
                ctl::command(client, Request::CoreSourceAdd { repo, name }).await
            }
            SourceCmd::Remove { repo } => {
                ctl::command(client, Request::CoreSourceRemove { id: repo }).await
            }
        },
        Some(CoreCmd::List {
            source,
            page,
            stable,
            refresh,
        }) => ctl::core_list(client, source, page, stable, refresh).await,
        Some(CoreCmd::Installed) => ctl::core_installed(client).await,
        Some(CoreCmd::Install {
            tag,
            source,
            variant,
            no_switch,
            force,
        }) => ctl::core_install(client, tag, source, variant, !no_switch, force).await,
        Some(CoreCmd::Use { core, force }) => ctl::core_use(client, &core, force).await,
        Some(CoreCmd::Remove { core }) => ctl::core_remove(client, &core).await,
        Some(CoreCmd::Import {
            location,
            sha256,
            no_switch,
        }) => {
            // The daemon resolves paths itself; make relative ones absolute here.
            let is_url = location.starts_with("http://") || location.starts_with("https://");
            let location = match std::fs::canonicalize(&location) {
                Ok(path) if !is_url => path.display().to_string(),
                _ => location,
            };
            eprintln!("{}", fl!("ctl-importing-core"));
            ctl::command(
                client,
                Request::CoreImport {
                    location,
                    sha256,
                    activate: !no_switch,
                },
            )
            .await
        }
    }
}

async fn run_profile(client: &DaemonClient, action: Option<ProfileCmd>) -> Result<()> {
    match action.unwrap_or(ProfileCmd::List) {
        ProfileCmd::List => ctl::profile_list(client).await,
        ProfileCmd::Add {
            source,
            name,
            interval,
            activate,
        } => ctl::profile_add(client, &source, name, interval, activate).await,
        ProfileCmd::New {
            name,
            edit,
            activate,
        } => ctl::profile_new(client, name, edit, activate).await,
        ProfileCmd::Use { profile, force } => {
            ctl::command(client, Request::ProfileActivate { id: profile, force }).await
        }
        ProfileCmd::Show { profile } => ctl::profile_show(client, &profile).await,
        ProfileCmd::Edit {
            profile,
            force,
            external,
        } => ctl::profile_edit(client, &profile, force, external).await,
        ProfileCmd::Update { profile, force } => {
            eprintln!("{}", fl!("ctl-profile-downloading"));
            ctl::command(client, Request::ProfileUpdate { id: profile, force }).await
        }
        ProfileCmd::Set {
            profile,
            name,
            url,
            interval,
            local,
        } => ctl::profile_set(client, profile, name, url, interval, local).await,
        ProfileCmd::Check { profile } => {
            ctl::command(client, Request::ProfileCheck { id: profile }).await
        }
        ProfileCmd::Remove { profile } => {
            ctl::command(client, Request::ProfileRemove { id: profile }).await
        }
        ProfileCmd::Adopt => ctl::command(client, Request::ProfileAdopt).await,
    }
}

#[cfg(unix)]
async fn run_container(client: &DaemonClient, action: Option<ContainerCmd>) -> Result<()> {
    use crate::ctl_container as ct;
    match action.unwrap_or(ContainerCmd::List) {
        ContainerCmd::List => ct::list(client).await,
        ContainerCmd::Show { container, config } => ct::show(client, &container, config).await,
        ContainerCmd::New {
            name,
            network,
            image,
            edit,
            start,
        } => ct::new(client, name, network, image, edit, start).await,
        ContainerCmd::Add { file, name, link } => ct::add(client, &file, name, link).await,
        ContainerCmd::Adopt { registry } => {
            ctl::command(client, Request::ContainerAdopt { path: registry }).await
        }
        ContainerCmd::Edit {
            container,
            force,
            external,
        } => ct::edit(client, &container, force, external).await,
        ContainerCmd::Start { container } => {
            ct::control(client, &container, ContainerAction::Start).await
        }
        ContainerCmd::Stop { container } => {
            ct::control(client, &container, ContainerAction::Stop).await
        }
        ContainerCmd::Restart { container } => {
            ct::control(client, &container, ContainerAction::Restart).await
        }
        ContainerCmd::Install {
            container,
            source,
            size,
            sha256,
            force,
        } => ct::install(client, &container, &source, size, sha256, force).await,
        ContainerCmd::Exec {
            container,
            timeout,
            command,
        } => ct::exec(client, &container, timeout, command).await,
        ContainerCmd::Enter { container, user } => ct::enter(client, &container, &user).await,
        ContainerCmd::Autostart { container, enabled } => {
            let request = Request::ContainerSet {
                id: container,
                name: None,
                autostart: Some(enabled),
            };
            ctl::command(client, request).await
        }
        ContainerCmd::Rename { container, name } => {
            let request = Request::ContainerSet {
                id: container,
                name: Some(name),
                autostart: None,
            };
            ctl::command(client, request).await
        }
        ContainerCmd::Remove { container, purge } => {
            ctl::command(
                client,
                Request::ContainerRemove {
                    id: container,
                    purge,
                },
            )
            .await
        }
        ContainerCmd::Images { distro, refresh } => ct::images(client, distro, refresh).await,
        ContainerCmd::Check => ctl::command(client, Request::ContainerCheck).await,
        ContainerCmd::Scan => ctl::command(client, Request::ContainerScan).await,
        ContainerCmd::Runtime { action } => match action {
            None => ct::runtime(client).await,
            Some(RuntimeCmd::Update { tag, force }) => ct::runtime_update(client, tag, force).await,
            Some(RuntimeCmd::Import { location, sha256 }) => {
                ct::runtime_import(client, location, sha256).await
            }
        },
    }
}

/// Clients follow a readable daemon.toml so a custom socket path just works.
fn client_socket() -> PathBuf {
    DaemonConfig::load(&config::default_config_path(), false)
        .map(|(config, _)| config.socket)
        .unwrap_or_else(|_| config::default_socket())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_is_well_formed() {
        cli::localize(Cli::command()).debug_assert();
    }

    #[test]
    fn container_commands_parse() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("singbox-board").chain(args.iter().copied()))
        };
        let cli = parse(&["container", "autostart", "dev", "off"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Cmd::Container {
                action: Some(ContainerCmd::Autostart { enabled: false, .. })
            })
        ));
        let cli = parse(&["ct", "exec", "dev", "--", "ls", "-la"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Cmd::Container {
                action: Some(ContainerCmd::Exec { ref command, .. })
            }) if command == &["ls", "-la"]
        ));
        let cli = parse(&["container", "run", "dev", "uname", "-a"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Cmd::Container {
                action: Some(ContainerCmd::Exec { ref command, .. })
            }) if command == &["uname", "-a"]
        ));
        assert!(parse(&["container", "new", "x", "--network", "bridge"]).is_err());
        let cli = parse(&["container", "enter", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Cmd::Container {
                action: Some(ContainerCmd::Enter { ref user, .. })
            }) if user == "root"
        ));
    }
}
