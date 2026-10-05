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
            Cmd::Daemon { .. } => unreachable!("handled in main"),
        }
    })
}

/// Clients follow a readable daemon.toml so a custom socket path just works.
fn default_socket() -> PathBuf {
    DaemonConfig::load(std::path::Path::new(DEFAULT_CONFIG_PATH), false)
        .map(|(config, _)| config.socket)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SOCKET))
}
