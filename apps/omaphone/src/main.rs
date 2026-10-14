// SPDX-License-Identifier: GPL-3.0-or-later
//! omaphone: the phone app of Omarchy Mobile, shown as "Phone": dialpad
//! with smart dial, call log, favourites, contacts, and the in-call and
//! answer screens. Calls go through omatel-agent's org.omamobile.Phone1.

mod app;
mod pages;
mod script;
mod util;
mod views;

use clap::Parser;
use gtk4 as gtk;

#[derive(Debug, Clone, Default, Parser)]
#[command(name = "omaphone", version, about = "Phone for Omarchy Mobile")]
pub struct Cli {
    /// A `tel:` or `callto:` URI (or a number): opens the dialpad with it
    /// filled in. Nothing is dialled until you press call.
    pub uri: Option<String>,
    /// Open on the contacts tab.
    #[arg(long)]
    pub contacts: bool,
    /// Open on the call log.
    #[arg(long)]
    pub recents: bool,
    /// Open on the favourites.
    #[arg(long)]
    pub favourites: bool,
    /// Open on the dialpad.
    #[arg(long)]
    pub keypad: bool,
    /// Show the answer page for this call (the agent's call ID).
    #[arg(long, value_name = "ID")]
    pub incoming: Option<String>,
    /// Show the call in progress.
    #[arg(long)]
    pub in_call: bool,
    /// Import contacts from a .vcf file.
    #[arg(long, value_name = "FILE")]
    pub import: Option<std::path::PathBuf>,
    /// Run a test script (see script.rs), e.g.
    /// "wait 1; tab keypad; shot /tmp/keypad.png; quit".
    #[arg(long)]
    pub script: Option<String>,
}

/// Log to the terminal when there is one; otherwise (started from the app
/// launcher or by the agent) to the journal as `omaphone`
/// (`journalctl -t omaphone`). Never a number, a name or a text.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::prelude::*;
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "omaphone=info".into())
    };
    let journal = if std::io::stderr().is_terminal() {
        None
    } else {
        tracing_journald::layer()
            .map(|l| l.with_syslog_identifier("omaphone".into()))
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

fn main() -> gtk::glib::ExitCode {
    init_logging();
    // Parse here first so --help, --version and mistakes answer at once;
    // the arguments then travel to the running instance (if any) behind
    // "--", where GApplication leaves them alone.
    let cli = Cli::parse();
    if let Some(text) = &cli.script {
        if let Err(e) = script::parse(text) {
            eprintln!("omaphone: --script: {e}");
            return gtk::glib::ExitCode::FAILURE;
        }
    }
    app::run()
}
