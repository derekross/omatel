// SPDX-License-Identifier: GPL-3.0-or-later
//! A private dbus-daemon in a temporary directory, for tests: never the
//! machine's own system or session bus.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>omatel-test</type>
  <listen>unix:path=@PATH@</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow receive_sender="*"/>
    <allow eavesdrop="true"/>
  </policy>
</busconfig>
"#;

pub struct PrivateBus {
    _dir: tempfile::TempDir,
    child: Child,
    socket: PathBuf,
}

impl PrivateBus {
    /// Start a dbus-daemon (from the `dbus` package) on a socket in a new
    /// temporary directory.
    pub fn start() -> anyhow::Result<PrivateBus> {
        let dir = tempfile::Builder::new().prefix("omatel-bus-").tempdir()?;
        let socket = dir.path().join("bus");
        let config = dir.path().join("bus.conf");
        std::fs::write(&config, CONFIG.replace("@PATH@", &socket.to_string_lossy()))?;
        let child = Command::new("dbus-daemon")
            .arg("--nofork")
            .arg(format!("--config-file={}", config.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("dbus-daemon (package dbus) is needed: {e}"))?;
        let mut bus = PrivateBus {
            _dir: dir,
            child,
            socket,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !bus.socket.exists() {
            if Instant::now() > deadline || bus.child.try_wait()?.is_some() {
                anyhow::bail!("the private dbus-daemon did not start");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(bus)
    }

    /// The address to connect to (`unix:path=...`).
    pub fn address(&self) -> String {
        format!("unix:path={}", self.socket.display())
    }

    /// A new connection to the bus.
    pub async fn connect(&self) -> zbus::Result<zbus::Connection> {
        zbus::connection::Builder::address(self.address().as_str())?
            .build()
            .await
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
