// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxies. Written by hand from ModemManager 1.24's introspection XML
//! (names and signatures only). Where an interface has both a `State`
//! property and a `StateChanged` signal, the signal's Rust name carries a
//! prefix so it doesn't collide with zbus's property-change stream.

//! Each proxy sits in a module of its own: zbus names a signal's types
//! after the signal, and Modem and Call both have `StateChanged`.

pub mod modem {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Modem",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Modem {
        fn enable(&self, enable: bool) -> zbus::Result<()>;

        #[zbus(property)]
        fn state(&self) -> zbus::Result<i32>;
        #[zbus(property)]
        fn state_failed_reason(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn unlock_required(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn sim(&self) -> zbus::Result<OwnedObjectPath>;
        #[zbus(property)]
        fn own_numbers(&self) -> zbus::Result<Vec<String>>;
        #[zbus(property)]
        fn signal_quality(&self) -> zbus::Result<(u32, bool)>;
        #[zbus(property)]
        fn access_technologies(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn power_state(&self) -> zbus::Result<u32>;

        #[zbus(signal, name = "StateChanged")]
        fn modem_state_changed(&self, old: i32, new: i32, reason: u32) -> zbus::Result<()>;
    }
}
pub use modem::ModemProxy;

pub mod modem3gpp {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Modem3gpp {
        #[zbus(property)]
        fn registration_state(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn operator_name(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn operator_code(&self) -> zbus::Result<String>;
    }
}
pub use modem3gpp::Modem3gppProxy;

pub mod voice {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Modem.Voice",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Voice {
        fn list_calls(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
        fn create_call(
            &self,
            properties: HashMap<&str, Value<'_>>,
        ) -> zbus::Result<OwnedObjectPath>;
        fn delete_call(&self, path: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;
        fn hangup_all(&self) -> zbus::Result<()>;
        fn hold_and_accept(&self) -> zbus::Result<()>;
        fn hangup_and_accept(&self) -> zbus::Result<()>;

        #[zbus(property)]
        fn calls(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
        #[zbus(property)]
        fn emergency_only(&self) -> zbus::Result<bool>;

        #[zbus(signal)]
        fn call_added(&self, path: OwnedObjectPath) -> zbus::Result<()>;
        #[zbus(signal)]
        fn call_deleted(&self, path: OwnedObjectPath) -> zbus::Result<()>;
    }
}
pub use voice::VoiceProxy;

pub mod call {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Call",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Call {
        fn start(&self) -> zbus::Result<()>;
        fn accept(&self) -> zbus::Result<()>;
        fn hangup(&self) -> zbus::Result<()>;
        fn send_dtmf(&self, dtmf: &str) -> zbus::Result<()>;

        #[zbus(property)]
        fn state(&self) -> zbus::Result<i32>;
        #[zbus(property)]
        fn state_reason(&self) -> zbus::Result<i32>;
        #[zbus(property)]
        fn direction(&self) -> zbus::Result<i32>;
        #[zbus(property)]
        fn number(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn multiparty(&self) -> zbus::Result<bool>;

        #[zbus(signal, name = "StateChanged")]
        fn call_state_changed(&self, old: i32, new: i32, reason: u32) -> zbus::Result<()>;
        #[zbus(signal)]
        fn dtmf_received(&self, dtmf: String) -> zbus::Result<()>;
    }
}
pub use call::CallProxy;

pub mod messaging {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Modem.Messaging",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Messaging {
        fn list(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
        fn create(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;
        fn delete(&self, path: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

        #[zbus(property)]
        fn messages(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

        #[zbus(signal)]
        fn added(&self, path: OwnedObjectPath, received: bool) -> zbus::Result<()>;
        #[zbus(signal)]
        fn deleted(&self, path: OwnedObjectPath) -> zbus::Result<()>;
    }
}
pub use messaging::MessagingProxy;

pub mod sms {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Sms",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Sms {
        fn send(&self) -> zbus::Result<()>;

        #[zbus(property)]
        fn state(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn pdu_type(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn number(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn text(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn data(&self) -> zbus::Result<Vec<u8>>;
        #[zbus(property)]
        fn timestamp(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn delivery_state(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn message_reference(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn delivery_report_request(&self) -> zbus::Result<bool>;
    }
}
pub use sms::SmsProxy;

pub mod sim {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Sim",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Sim {
        #[zbus(property)]
        fn active(&self) -> zbus::Result<bool>;
        #[zbus(property)]
        fn operator_name(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn emergency_numbers(&self) -> zbus::Result<Vec<String>>;
    }
}
pub use sim::SimProxy;

pub mod signal {
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use zbus::proxy;
    #[allow(unused_imports)]
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Modem.Signal",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait Signal {
        fn setup(&self, rate: u32) -> zbus::Result<()>;

        #[zbus(property)]
        fn rate(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn lte(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
        #[zbus(property)]
        fn nr5g(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    }
}
pub use signal::SignalProxy;
