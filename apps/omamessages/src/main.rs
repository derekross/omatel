// SPDX-License-Identifier: GPL-3.0-or-later
//! omamessages: SMS for Omarchy Mobile, shown as "Messages".
//!
//! The app reads the conversations the agent (`omatel-agent`) stores in
//! `~/.local/share/omatel/messages.db` and sends through the agent's
//! `org.omamobile.Messages1`; it never talks to the modem itself.

mod app;
mod format;
mod script;
mod segments;
mod uri;

use std::path::PathBuf;

use clap::Parser;
use gtk4 as gtk;

#[derive(Debug, Clone, Parser)]
#[command(name = "omamessages", version, about = "Messages for Omarchy Mobile")]
pub struct Cli {
    /// An sms:, smsto:, mms: or mmsto: link, or a number: opens a new
    /// message to it, with the link's body filled in (never sent by itself).
    pub target: Option<String>,
    /// Open this conversation (its ID in the message store).
    #[arg(long)]
    pub conversation: Option<i64>,
    /// Open the new-message page.
    #[arg(long)]
    pub compose: bool,
    /// Write every conversation to FILE as JSON, then exit.
    #[arg(long, value_name = "FILE", conflicts_with = "import")]
    pub export: Option<PathBuf>,
    /// Merge conversations from a JSON export in FILE, then exit.
    #[arg(long, value_name = "FILE")]
    pub import: Option<PathBuf>,
    /// Run a test script (see script.rs), e.g. "wait 1; open 0; shot /tmp/t.png; quit".
    #[arg(long)]
    pub script: Option<String>,
}

/// Log to the terminal when there is one, else to the journal as
/// `omamessages`. Never a number, a name or message text.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::prelude::*;
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "omamessages=info,omatel_store=info".into())
    };
    let journal = if std::io::stderr().is_terminal() {
        None
    } else {
        tracing_journald::layer()
            .map(|l| l.with_syslog_identifier("omamessages".into()))
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

fn export_import(cli: &Cli) -> anyhow::Result<()> {
    let store = omatel_store::MessageStore::open(&omatel_store::data_dir().join("messages.db"))?;
    if let Some(path) = &cli.export {
        std::fs::write(path, store.export_json()?)?;
        println!("conversations written to {}", path.display());
    }
    if let Some(path) = &cli.import {
        let added = store.import_json(&std::fs::read_to_string(path)?)?;
        println!("{added} messages imported");
    }
    Ok(())
}

fn main() -> gtk::glib::ExitCode {
    init_logging();
    // Parsed here for --help, errors and the commands that need no window;
    // the running instance parses the arguments again (app::run).
    let cli = Cli::parse();
    if let Some(text) = &cli.script {
        if let Err(e) = script::parse(text) {
            eprintln!("omamessages: --script: {e}");
            return gtk::glib::ExitCode::FAILURE;
        }
    }
    if cli.export.is_some() || cli.import.is_some() {
        return match export_import(&cli) {
            Ok(()) => gtk::glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("omamessages: {e}");
                gtk::glib::ExitCode::FAILURE
            }
        };
    }
    app::run()
}
