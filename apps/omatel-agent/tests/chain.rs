// SPDX-License-Identifier: GPL-3.0-or-later
//! The real chain, offline: omatel-agent -> omamobile-mmd (the
//! ModemManager bridge) -> omamobile-sitd (the RIL) -> tools/sit-sim (a
//! scripted modem), from an Omarchy Mobile checkout. Each runs on private
//! buses and sockets in a temporary directory.
//!
//! Skipped unless `OMATEL_CHAIN_REPO` names the omarchy-mobile checkout;
//! `scripts/chain-test.sh` runs it in a container that has Python,
//! PyGObject and dbus. Every number is a fictional 555-01xx one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use omatel_agent::{Config, Launcher, LockProbe};
use omatel_api::{Messages1Proxy, Phone1Proxy};
use omatel_mock::bus::PrivateBus;
use omatel_mock::notifications::MockNotifications;
use omatel_store::{CallLog, CallOutcome, MessageState, MessageStore};

/// sit-sim's caller and SMS sender.
const SIM_SENDER: &str = "+12025550123";
const OTHER: &str = "+12025550199";

struct Procs(Vec<Child>);

impl Drop for Procs {
    fn drop(&mut self) {
        for c in self.0.iter_mut().rev() {
            // SIGTERM first: sitd hangs up and shuts the modem down cleanly.
            let _ = Command::new("kill").arg(c.id().to_string()).status();
        }
        for c in self.0.iter_mut().rev() {
            for _ in 0..100 {
                if c.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wait_for(path: &Path, what: &str, secs: u64) {
    for _ in 0..secs * 20 {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{what} did not start");
}

async fn eventually<F, Fut>(secs: u64, what: &str, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..secs * 20 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what}: not reached in {secs} s");
}

fn control(path: &Path, command: &str) {
    let s = std::os::unix::net::UnixDatagram::unbound().unwrap();
    s.send_to(command.as_bytes(), path).unwrap();
}

fn transcript(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_over_the_real_bridge_and_ril() {
    let Some(repo) = std::env::var_os("OMATEL_CHAIN_REPO").map(PathBuf::from) else {
        eprintln!("skipped: set OMATEL_CHAIN_REPO to an omarchy-mobile checkout");
        return;
    };
    let logs = LogBuffer::default();
    let sink = logs.clone();
    tracing_subscriber::fmt()
        .with_env_filter("omatel_agent=debug")
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .init();

    let sim = repo.join("tools/sit-sim");
    let sitd = repo.join("packages/omamobile-modem-sit/bin/omamobile-sitd");
    let mmd = repo.join("packages/omamobile-mmd/bin/omamobile-mmd");
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let run = d.join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::write(
        d.join("sit.conf"),
        "[network]\napn = internet.example\nsim_timeout = 20\nregister_timeout = 30\n[audio]\ncard = none\n",
    )
    .unwrap();
    let log = |name: &str| std::fs::File::create(d.join(name)).unwrap();
    let mut procs = Procs(Vec::new());
    procs.0.push(
        Command::new("python3")
            .arg(&sim)
            .args(["--ipc", &d.join("ipc0").to_string_lossy()])
            .args(["--router", &d.join("router").to_string_lossy()])
            .args([
                "--transcript",
                &d.join("transcript.jsonl").to_string_lossy(),
            ])
            .args(["--control", &d.join("control").to_string_lossy()])
            .stdout(log("sim.log"))
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(&d.join("router"), "sit-sim", 10);
    procs.0.push(
        Command::new("python3")
            .arg(&sitd)
            .arg("--bringup")
            .env("OMAMOBILE_SIT_RUN", &run)
            .env("OMAMOBILE_SIT_STATE", d.join("state"))
            .env("OMAMOBILE_SIT_CONFIG", d.join("sit.conf"))
            .env(
                "OMAMOBILE_SIT_DEVICE",
                format!("unix:{}", d.join("ipc0").display()),
            )
            .env(
                "OMAMOBILE_SIT_ROUTER",
                format!("unix:{}", d.join("router").display()),
            )
            .env("OMAMOBILE_SIT_BOOT", "none")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdout(log("sitd.log"))
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(&run.join("socket"), "omamobile-sitd", 40);

    let system = PrivateBus::start().unwrap();
    let session = PrivateBus::start().unwrap();
    std::fs::write(d.join("mmd.conf"), "[network]\napn = internet.example\n").unwrap();
    procs.0.push(
        Command::new("python3")
            .arg(&mmd)
            .args(["--socket", &run.join("socket").to_string_lossy()])
            .arg("--no-polkit")
            .args(["--sit-config", &d.join("mmd.conf").to_string_lossy()])
            .env("DBUS_SYSTEM_BUS_ADDRESS", system.address())
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdout(log("mmd.log"))
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let notes = MockNotifications::start(session.connect().await.unwrap(), true)
        .await
        .unwrap();
    let cfg = Config {
        data_dir: d.join("data"),
        contacts_dir: d.join("contacts"),
        settings_path: d.join("settings.toml"),
        launcher: Launcher::Off,
        lock_probe: LockProbe::Fixed(false),
    };
    let (sys, ses) = (
        system.connect().await.unwrap(),
        session.connect().await.unwrap(),
    );
    tokio::spawn(async move { omatel_agent::run(cfg, sys, ses).await.unwrap() });
    let client = session.connect().await.unwrap();
    let dbus = zbus::fdo::DBusProxy::new(&client).await.unwrap();
    eventually(10, "the agent", || async {
        dbus.name_has_owner("org.omamobile.Phone1".try_into().unwrap())
            .await
            .unwrap_or(false)
    })
    .await;
    let phone = Phone1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    let messages = Messages1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    // sitd's start-up registers and brings the bearer up: CONNECTED.
    eventually(60, "the modem ready", || async {
        phone.modem_state().await.ok().as_deref() == Some("ready")
    })
    .await;
    let control_path = d.join("control");

    // An incoming call, answered, a touch tone, hung up.
    let mut incoming = phone.receive_incoming_call().await.unwrap();
    control(&control_path, "ring");
    let sig = tokio::time::timeout(Duration::from_secs(10), incoming.next())
        .await
        .expect("no IncomingCall")
        .unwrap();
    let a = sig.args().unwrap();
    assert_eq!(a.number, SIM_SENDER);
    let id = a.id.to_string();
    eventually(10, "the incoming notification", || async {
        notes.open().iter().any(|n| n.summary == "Incoming call")
    })
    .await;
    phone.answer(&id).await.unwrap();
    eventually(10, "active", || async {
        phone
            .list_calls()
            .await
            .unwrap()
            .iter()
            .any(|c| c.id == id && c.state == "active")
    })
    .await;
    phone.send_dtmf(&id, "5").await.unwrap();
    phone.hangup(&id).await.unwrap();
    let calls_db = d.join("data/calls.db");
    eventually(10, "the call logged", || async {
        CallLog::open(&calls_db).unwrap().recent(5).unwrap().len() == 1
    })
    .await;
    assert_eq!(
        CallLog::open(&calls_db).unwrap().recent(1).unwrap()[0].outcome,
        CallOutcome::Answered
    );

    // An outgoing call, answered by the simulated network.
    let out = phone.dial(OTHER).await.unwrap();
    eventually(10, "outgoing active", || async {
        phone
            .list_calls()
            .await
            .unwrap()
            .iter()
            .any(|c| c.id == out && c.state == "active")
    })
    .await;
    phone.hangup("").await.unwrap();
    eventually(10, "the outgoing call logged", || async {
        CallLog::open(&calls_db).unwrap().recent(5).unwrap().len() == 2
    })
    .await;

    // Emergency numbers never get to the RIL.
    let dials_before = transcript(&d.join("transcript.jsonl"))
        .iter()
        .filter(|e| e["type"] == "req" && e["id"] == 0x0d00)
        .count();
    assert!(phone
        .dial("911")
        .await
        .unwrap_err()
        .to_string()
        .contains("EmergencyUnsupported"));

    // An incoming SMS: stored, deleted from the bridge (and sitd), notified.
    let mut changed = messages.receive_messages_changed().await.unwrap();
    control(&control_path, "sms");
    tokio::time::timeout(Duration::from_secs(15), changed.next())
        .await
        .expect("no MessagesChanged")
        .unwrap();
    let store = MessageStore::open(&d.join("data/messages.db")).unwrap();
    let conv = store.find_conversation(SIM_SENDER).unwrap().unwrap();
    let msgs = store.messages(conv, 10, None).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].body, "hello");
    let mm_messages = omatel_mm::MessagingProxy::builder(&system.connect().await.unwrap())
        .path("/org/freedesktop/ModemManager1/Modem/0")
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    eventually(10, "the SMS deleted from the bridge", || async {
        mm_messages.list().await.unwrap().is_empty()
    })
    .await;
    eventually(5, "the message notification", || async {
        notes.open().iter().any(|n| n.body == "hello")
    })
    .await;

    // A reply from the notification goes out through the RIL, once.
    let n = notes
        .open()
        .into_iter()
        .find(|n| n.body == "hello")
        .unwrap();
    notes.reply(n.id, "omatel chain reply").await.unwrap();
    eventually(60, "the reply sent", || async {
        store
            .messages(conv, 10, None)
            .unwrap()
            .iter()
            .any(|m| m.body == "omatel chain reply" && m.state == MessageState::Sent)
    })
    .await;
    let sends = transcript(&d.join("transcript.jsonl"))
        .iter()
        .filter(|e| e["event"] == "send_sms")
        .count();
    assert_eq!(sends, 1);

    // A WAP push is left for mmsd-tng.
    // (Our sent reply stays on the bridge too, while its delivery report
    // is awaited.)
    let sys2 = system.connect().await.unwrap();
    let binary_on_bridge = || async {
        let mut n = 0;
        for p in mm_messages.list().await.unwrap() {
            if omatel_mm::SmsInfo::read(&sys2, p)
                .await
                .unwrap()
                .is_binary()
            {
                n += 1;
            }
        }
        n
    };
    control(&control_path, "wap-push");
    eventually(10, "the WAP push on the bridge", || async {
        binary_on_bridge().await == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(binary_on_bridge().await, 1);
    assert_eq!(store.messages(conv, 10, None).unwrap().len(), 2);

    // The simulated modem saw no violation, one touch tone, and no
    // emergency dial.
    let entries = transcript(&d.join("transcript.jsonl"));
    assert!(entries.iter().all(|e| e.get("violation").is_none()));
    let tones: Vec<&str> = entries
        .iter()
        .filter(|e| e["event"] == "dtmf")
        .filter_map(|e| e["key"].as_str())
        .collect();
    assert_eq!(tones, vec!["5"]);
    let dials_after = entries
        .iter()
        .filter(|e| e["type"] == "req" && e["id"] == 0x0d00)
        .count();
    assert_eq!(dials_before, dials_after);

    // Nothing private in the agent's own log.
    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(
        text.contains("SMS stored"),
        "the agent logged nothing useful"
    );
    for secret in ["2025550", "555-01", "hello", "chain reply"] {
        assert!(!text.contains(secret), "the agent log holds {secret:?}");
    }
    drop(procs);
}
