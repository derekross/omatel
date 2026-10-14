// SPDX-License-Identifier: GPL-3.0-or-later
//! omatel-agent: the headless service behind Phone and Messages. Started
//! by its systemd user unit (`omatel-agent.service`) at login.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "omatel-agent",
    version,
    about = "The background service of Omarchy Mobile's Phone and Messages"
)]
struct Cli {
    /// Never open the apps (no answer page, no conversation on a click).
    #[arg(long)]
    no_launch: bool,
}

/// Log to the terminal when there is one, otherwise to the journal as
/// `omatel-agent` (omacam's pattern). States and paths only, never a
/// number or text.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::prelude::*;
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "omatel_agent=info".into())
    };
    let journal = if std::io::stderr().is_terminal() {
        None
    } else {
        tracing_journald::layer()
            .map(|l| l.with_syslog_identifier("omatel-agent".into()))
            .ok()
    };
    match journal {
        Some(layer) => tracing_subscriber::registry()
            .with(layer.with_filter(filter()))
            .init(),
        None => tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .init(),
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let system = zbus::Connection::system().await?;
        let session = zbus::Connection::session().await?;
        let mut cfg = omatel_agent::Config::from_environment();
        if cli.no_launch {
            cfg.launcher = omatel_agent::Launcher::Off;
        }
        tokio::select! {
            r = omatel_agent::run(cfg, system, session) => r,
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("stopping");
                Ok(())
            }
            _ = sigterm() => {
                tracing::info!("stopping");
                Ok(())
            }
        }
    })
}

async fn sigterm() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}
