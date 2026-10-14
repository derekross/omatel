// SPDX-License-Identifier: GPL-3.0-or-later
//! Write fictional demo data into `$XDG_DATA_HOME` (or the directory
//! given): `omatel-demo-data [DATA_HOME]`. Refuses a directory that
//! already has omatel data, so it never mixes into someone's real history.

fn main() -> anyhow::Result<()> {
    let dir = match std::env::args_os().nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("give a data directory or set XDG_DATA_HOME"))?,
    };
    if dir.join("omatel").exists() || dir.join("contacts").exists() {
        anyhow::bail!("{} already has omatel data or contacts", dir.display());
    }
    omatel_mock::demo::seed(&dir, omatel_store::now_ms())?;
    println!("demo data written to {}", dir.display());
    Ok(())
}
