mod converter;
mod db;
mod notifier;
mod service;
pub mod tools;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use db::Db;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "PowerEXT", about = "File rename monitor with backup and conversion")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the watcher (used by the system service)
    Start {
        /// Directory tree to watch recursively (default: $HOME)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Install as a system service and start it
    Install,
    /// Remove the system service
    Uninstall,
    /// Show service status
    Status,
}

fn resolve_data_dir() -> Result<PathBuf> {
    let preferred = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine data directory"))?
        .join("PowerEXT");
    // If preferred exists but isn't writable (e.g. owned by root), fall back to ~/.PowerEXT
    if preferred.exists() {
        let probe = preferred.join(".write_test");
        if std::fs::write(&probe, b"").is_err() {
            let fallback = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
                .join(".PowerEXT");
            return Ok(fallback);
        }
        let _ = std::fs::remove_file(probe);
    }
    Ok(preferred)
}

fn main() -> Result<()> {
    let data_dir = resolve_data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    converter::set_data_dir(data_dir.clone());

    let file_appender = tracing_appender::rolling::daily(&data_dir, "PowerEXT.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("PowerEXT=debug"));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_target(false);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_target(false)
        .with_ansi(false)
        .with_writer(non_blocking);

    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Start { path } => {
            let watch_path = path
                .or_else(dirs::home_dir)
                .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

            let db = Arc::new(Db::open(&data_dir)?);
            tracing::info!(data_dir = %data_dir.display(), watch = %watch_path.display(), "PowerEXT starting");

            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(watcher::run(watch_path, db, data_dir))?;
        }
        Cmd::Install   => service::install()?,
        Cmd::Uninstall => service::uninstall()?,
        Cmd::Status    => service::status()?,
    }

    Ok(())
}
