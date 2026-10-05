mod clash;
mod client;
mod config;
mod ctl;
mod daemon;
mod protocol;
mod substore;
mod tui;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::client::DaemonClient;
use crate::config::{DEFAULT_CONFIG_PATH, DEFAULT_SOCKET, DaemonConfig};
use crate::protocol::{Component, ComponentAction, Request};

#[derive(Parser)]
#[command(
    name = "singbox-board",
    version,
    about = "Root daemon and terminal dashboard for MiChongs/sing-box",
    long_about = "Root daemon and terminal dashboard for MiChongs/sing-box.\n\n\
                  Run `singbox-board daemon` as root to supervise sing-box, then use \
                  `singbox-board` (TUI) or the subcommands below as root or as a member \
                  of the socket group."
)]
struct Cli {
    /// Control socket of the daemon [default: from daemon.toml or /run/singbox-board/daemon.sock]
    #[arg(long, global = true, env = "SINGBOX_BOARD_SOCKET")]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the terminal dashboard (default)
    Tui,
    /// Run the root daemon that supervises sing-box
    Daemon {
        /// Daemon configuration file
        #[arg(short, long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// Allow running without root (development only)
        #[arg(long)]
        allow_non_root: bool,
        /// Print the commented default configuration and exit
        #[arg(long)]
        print_default_config: bool,
    },
    /// Show daemon and sing-box status
    Status {
        /// Print raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Start sing-box
    Start,
    /// Stop sing-box
    Stop,
    /// Restart sing-box
    Restart,
    /// Validate the configuration and hot-reload sing-box (SIGHUP)
    Reload,
    /// Validate the sing-box configuration (`sing-box check`)
    Check,
    /// Print sing-box and daemon output
    Logs {
        /// Number of buffered lines to print first
        #[arg(short = 'n', long, default_value_t = 200)]
        tail: usize,
        /// Keep printing new lines
        #[arg(short, long)]
        follow: bool,
    },
    /// Install or update sing-box from the MiChongs/sing-box GitHub releases
    Update {
        /// Only report whether an update is available
        #[arg(long)]
        check: bool,
        /// Install a specific release tag, e.g. v1.14.1-xiaobaf14g.1
        #[arg(long)]
        tag: Option<String>,
        /// Reinstall even if the version is unchanged
        #[arg(long)]
        force: bool,
    },
    /// Choose the optional components (Sub-Store, http-meta); asked on first run
    Setup {
        /// Enable Sub-Store (yes/no); asked interactively when omitted
        #[arg(long, value_name = "BOOL", value_parser = clap::builder::BoolishValueParser::new())]
        sub_store: Option<bool>,
        /// Enable http-meta (yes/no); asked interactively when omitted
        #[arg(long, value_name = "BOOL", value_parser = clap::builder::BoolishValueParser::new())]
        http_meta: Option<bool>,
    },
    /// Manage an optional component; shows its details and URLs without an action
    Component {
        #[arg(value_enum)]
        component: Component,
        #[arg(value_enum)]
        action: Option<ComponentAction>,
    },
    /// Core versions: list releases, install, switch, import custom builds
    Core {
        #[command(subcommand)]
        action: Option<CoreCmd>,
    },
}

#[derive(Subcommand)]
enum CoreCmd {
    /// Release sources (MiChongs, SagerNet and custom repositories)
    Sources,
    /// Add or remove a custom GitHub source (root only)
    Source {
        #[command(subcommand)]
        action: SourceCmd,
    },
    /// Releases of a source with the builds available for this machine
    List {
        /// owner/repo [default: the source `update` follows]
        #[arg(long)]
        source: Option<String>,
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Hide pre-releases
        #[arg(long)]
        stable: bool,
        /// Bypass the 10 minute cache
        #[arg(long)]
        refresh: bool,
    },
    /// Cores in the local version store
    Installed,
    /// Download a release into the store and switch to it
    Install {
        /// Release tag, e.g. v1.14.1-xiaobaf14g.1
        tag: String,
        /// owner/repo [default: the source `update` follows]
        #[arg(long)]
        source: Option<String>,
        /// Build variant, e.g. ebpf, glibc, musl [default: plain build]
        #[arg(long, default_value = "")]
        variant: String,
        /// Only store it, do not switch
        #[arg(long)]
        no_switch: bool,
        /// Switch even if the new core rejects the configuration
        #[arg(long)]
        force: bool,
    },
    /// Switch to a stored core (id, version or tag)
    Use {
        core: String,
        #[arg(long)]
        force: bool,
    },
    /// Delete a stored core (id, version or tag)
    Remove { core: String },
    /// Store a custom core from a local file or http(s) URL (root only)
    Import {
        /// Absolute path or URL of a binary, .tar.gz, .zip or .gz
        location: String,
        /// Expected sha256 of the file
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        no_switch: bool,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    /// Add a GitHub repository publishing sing-box-<version>-linux-<arch> archives
    Add {
        /// owner/repo
        repo: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a custom source
    Remove { repo: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Cmd::Tui) {
        Cmd::Daemon {
            config,
            allow_non_root,
            print_default_config,
        } => {
            if print_default_config {
                print!("{}", config::TEMPLATE);
                return ExitCode::SUCCESS;
            }
            run_daemon(cli.socket, config, allow_non_root)
        }
        command => run_client(cli.socket, command),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_daemon(
    socket: Option<PathBuf>,
    config_path: Option<PathBuf>,
    allow_non_root: bool,
) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let explicit = config_path.is_some();
    let path = config_path.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
    let (mut config, found) = DaemonConfig::load(&path, explicit)?;
    if found {
        tracing::info!("loaded {}", path.display());
    } else {
        tracing::info!("{} not found, using defaults", path.display());
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

fn run_client(socket: Option<PathBuf>, command: Cmd) -> Result<()> {
    if !matches!(command, Cmd::Tui) {
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
    let socket = socket.unwrap_or_else(default_socket);
    let client = DaemonClient::new(socket);
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        match command {
            Cmd::Tui => tui::run(client).await,
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
            eprintln!("storing the custom core, this may take a while…");
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

/// Clients follow a readable daemon.toml so a custom socket path just works.
fn default_socket() -> PathBuf {
    DaemonConfig::load(std::path::Path::new(DEFAULT_CONFIG_PATH), false)
        .map(|(config, _)| config.socket)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SOCKET))
}
