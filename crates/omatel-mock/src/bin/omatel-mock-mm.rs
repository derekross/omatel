// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted ModemManager on the bus `DBUS_SYSTEM_BUS_ADDRESS` names, for
//! screenshots and trying the apps by hand in a container: never point it
//! at a real system bus with a real ModemManager. Drive it through
//! `org.omatel.MockModem` at `/org/omatel/MockModem`, for example
//!
//!   dbus-send --bus=$DBUS_SYSTEM_BUS_ADDRESS --print-reply \
//!     --dest=org.freedesktop.ModemManager1 /org/omatel/MockModem \
//!     org.omatel.MockModem.Ring string:+12025550101
//!
//! (also RemoteAnswer, RemoteHangup, ReceiveSms, Deliver, SetAutoAnswer).

use omatel_mock::mm::{Control, MockModem};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let addr = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").map_err(|_| {
        anyhow::anyhow!(
            "set DBUS_SYSTEM_BUS_ADDRESS to a private bus; this never uses the real one"
        )
    })?;
    let conn = zbus::connection::Builder::address(addr.as_str())?
        .build()
        .await?;
    let modem = MockModem::start(conn.clone()).await?;
    conn.object_server()
        .at("/org/omatel/MockModem", Control { modem })
        .await?;
    eprintln!("omatel-mock-mm: serving a mock ModemManager");
    std::future::pending::<()>().await;
    Ok(())
}
