// SPDX-License-Identifier: GPL-3.0-or-later
//! The window, the connection to the agent (org.omamobile.Phone1), the
//! call and answer pages, command lines from other launches, and the test
//! script. The tabs are in `views.rs`, pushed pages in `pages.rs`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use clap::Parser;
use futures_util::StreamExt;
use gtk::{gio, glib};
use gtk4 as gtk;
use libadwaita as adw;
use omatel_api::{CallInfo, Messages1Proxy, Phone1Proxy};
use omatel_contacts::{number, AddressBook};
use omatel_store::{BlockList, CallLog};

use crate::script::{self, Step};
use crate::util;
use crate::Cli;

pub const APP_ID: &str = "org.omamobile.Phone";

const EXTRA_CSS: &str = "
.incall-page, .incoming-page { padding: 24px 16px; }
.dtmf-digits { font-size: 22px; font-feature-settings: \"tnum\"; }
.emergency-title { font-size: 26px; font-weight: 700; }
.favourite-tile { padding: 10px 4px; border-radius: 16px; }
.route-note { font-size: 12px; opacity: 0.7; }
.call-button, .hangup-button { padding: 0; min-width: 76px; min-height: 76px; }
.incall-toggle { padding: 0; }
";

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let ui: Rc<RefCell<Option<Rc<Ui>>>> = Rc::new(RefCell::new(None));
    app.connect_command_line(move |app, cmdline| {
        let mut args: Vec<OsString> = cmdline.arguments();
        if args.get(1).is_some_and(|a| a == "--") {
            args.remove(1);
        }
        let cli = match Cli::try_parse_from(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                return glib::ExitCode::FAILURE;
            }
        };
        let current = ui.borrow().clone();
        let ui = match current {
            Some(u) => u,
            None => {
                let u = Ui::build(app);
                *ui.borrow_mut() = Some(u.clone());
                u
            }
        };
        ui.handle(cli);
        glib::ExitCode::SUCCESS
    });
    // Our arguments go behind "--" so GApplication passes them through
    // untouched, to this instance or to the one already running.
    let mut args: Vec<String> = std::env::args().take(1).collect();
    args.push("--".into());
    args.extend(std::env::args().skip(1));
    app.run_with_args(&args)
}

/// The widgets of the in-call page.
pub struct CallPage {
    pub root: gtk::Box,
    pub avatar: adw::Avatar,
    pub name: gtk::Label,
    pub number: gtk::Label,
    pub status: gtk::Label,
    pub mute: gtk::ToggleButton,
    pub speaker: gtk::ToggleButton,
    pub keypad: gtk::ToggleButton,
    pub dtmf: gtk::Revealer,
    pub dtmf_digits: gtk::Label,
    pub route_note: gtk::Label,
    pub hangup: gtk::Button,
}

pub struct IncomingPage {
    pub root: gtk::Box,
    pub avatar: adw::Avatar,
    pub name: gtk::Label,
    pub number: gtk::Label,
    pub replies: gtk::Revealer,
    pub replies_list: gtk::ListBox,
}

pub struct Ui {
    pub app: adw::Application,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub top: gtk::Stack,
    pub nav: adw::NavigationView,
    pub views: adw::ViewStack,
    pub banner: adw::Banner,
    pub header_add: gtk::Button,
    pub header_menu: gtk::MenuButton,
    // Keypad tab
    pub dial_text: RefCell<String>,
    pub display: gtk::Label,
    pub suggestions: gtk::ListBox,
    pub call_button: gtk::Button,
    // Other tabs
    pub favourites: gtk::Box,
    pub recents: gtk::Box,
    pub contacts_list: gtk::ListBox,
    pub contacts_search: gtk::SearchEntry,
    pub contacts_empty: adw::StatusPage,
    pub contacts_stack: gtk::Stack,
    pub call: CallPage,
    pub incoming: IncomingPage,
    pub emergency: gtk::Box,
    // Data
    pub book: RefCell<AddressBook>,
    pub log: Option<CallLog>,
    pub blocked: Option<BlockList>,
    pub region: String,
    pub phone: RefCell<Option<Phone1Proxy<'static>>>,
    pub messages: RefCell<Option<Messages1Proxy<'static>>>,
    pub available: Cell<bool>,
    pub emergency_numbers: RefCell<Vec<String>>,
    /// Every call the agent told us about, by ID.
    pub calls: RefCell<HashMap<String, CallInfo>>,
    /// The call the call or answer page shows.
    pub shown: RefCell<Option<String>>,
    /// Calls injected by the test script (no agent behind them).
    pub fake: Cell<bool>,
    /// Set while code (not the user) moves a toggle.
    pub updating: Cell<bool>,
    pub recent_numbers: RefCell<Vec<String>>,
    pub contact_uids: RefCell<Vec<String>>,
    pub open_contact: RefCell<Option<String>>,
    pub monitors: RefCell<Vec<gio::FileMonitor>>,
    /// Keeps the Omarchy theme followed.
    pub _theme: RefCell<Option<gio::FileMonitor>>,
    pub reload_pending: RefCell<Option<glib::SourceId>>,
}

impl Ui {
    fn build(app: &adw::Application) -> Rc<Ui> {
        let theme = omatel_theme::install(EXTRA_CSS);
        let settings = omatel_api::settings::Settings::load();
        let region = settings.region(number::default_region);
        let data = omatel_store::data_dir();
        let log = CallLog::open(&data.join("calls.db"))
            .map_err(|e| tracing::warn!(error = %e, "call log unavailable"))
            .ok();
        let blocked = BlockList::open(&data.join("blocked.db"))
            .map_err(|e| tracing::warn!(error = %e, "block list unavailable"))
            .ok();
        let book = AddressBook::open(omatel_contacts::default_root(), &region);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Phone")
            .default_width(480)
            .default_height(1000)
            .build();
        let views = adw::ViewStack::new();
        let nav = adw::NavigationView::new();
        let top = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&top));
        window.set_content(Some(&toasts));

        let banner = adw::Banner::new("Phone service isn't running");
        let header_add = gtk::Button::from_icon_name("contact-new-symbolic");
        header_add.set_tooltip_text(Some("New contact"));
        let header_menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Menu")
            .build();

        let display = gtk::Label::builder()
            .css_classes(["dial-display"])
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .height_request(48)
            .build();
        let suggestions = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .valign(gtk::Align::End)
            .build();
        let call_button = gtk::Button::builder()
            .icon_name("call-start-symbolic")
            .css_classes(["call-button"])
            .tooltip_text("Call")
            .halign(gtk::Align::Center)
            .build();

        let contacts_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let contacts_search = gtk::SearchEntry::builder()
            .placeholder_text("Search contacts")
            .build();
        let contacts_empty = adw::StatusPage::builder()
            .icon_name("avatar-default-symbolic")
            .title("No contacts")
            .description("Add one with the button above, or import a .vcf file from the menu.")
            .build();

        let ui = Rc::new(Ui {
            app: app.clone(),
            window,
            toasts,
            top,
            nav,
            views,
            banner,
            header_add,
            header_menu,
            dial_text: RefCell::new(String::new()),
            display,
            suggestions,
            call_button,
            favourites: gtk::Box::new(gtk::Orientation::Vertical, 18),
            recents: gtk::Box::new(gtk::Orientation::Vertical, 0),
            contacts_list,
            contacts_search,
            contacts_empty,
            contacts_stack: gtk::Stack::new(),
            call: build_call_page(),
            incoming: build_incoming_page(),
            emergency: gtk::Box::new(gtk::Orientation::Vertical, 18),
            book: RefCell::new(book),
            log,
            blocked,
            region,
            phone: RefCell::new(None),
            messages: RefCell::new(None),
            available: Cell::new(false),
            emergency_numbers: RefCell::new(Vec::new()),
            calls: RefCell::new(HashMap::new()),
            shown: RefCell::new(None),
            fake: Cell::new(false),
            updating: Cell::new(false),
            recent_numbers: RefCell::new(Vec::new()),
            contact_uids: RefCell::new(Vec::new()),
            open_contact: RefCell::new(None),
            monitors: RefCell::new(Vec::new()),
            _theme: RefCell::new(theme),
            reload_pending: RefCell::new(None),
        });
        ui.assemble();
        ui.watch_contacts();
        ui.set_available(false);
        glib::spawn_future_local(Self::connect(ui.clone()));
        // The call timer.
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(ui) => {
                ui.tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        ui
    }

    fn assemble(self: &Rc<Self>) {
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Phone", "")));
        header.pack_end(&self.header_menu);
        header.pack_end(&self.header_add);
        let menu = gio::Menu::new();
        menu.append(Some("Import contacts (.vcf)…"), Some("app.import-vcf"));
        menu.append(Some("Export contacts (.vcf)…"), Some("app.export-vcf"));
        menu.append(Some("Blocked numbers"), Some("app.blocked"));
        menu.append(Some("Clear call history"), Some("app.clear-log"));
        self.header_menu.set_menu_model(Some(&menu));
        self.add_actions();

        let switcher = adw::ViewSwitcherBar::builder()
            .stack(&self.views)
            .reveal(true)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&self.banner);
        toolbar.set_content(Some(&self.views));
        toolbar.add_bottom_bar(&switcher);

        self.build_tabs();
        let root = adw::NavigationPage::new(&toolbar, "Phone");
        root.set_tag(Some("root"));
        self.nav.add(&root);

        self.top.add_named(&self.nav, Some("main"));
        self.top.add_named(&self.call.root, Some("call"));
        self.top.add_named(&self.incoming.root, Some("incoming"));
        self.build_emergency_page();
        self.top.add_named(&self.emergency, Some("emergency"));
        self.top.set_visible_child_name("main");

        let ui = self.clone();
        self.views
            .connect_visible_child_name_notify(move |_| ui.tab_changed());
        let ui = self.clone();
        self.header_add
            .connect_clicked(move |_| ui.edit_contact(None, None));
        self.wire_call_page();
        self.wire_incoming_page();
        self.views.set_visible_child_name("keypad");
        self.tab_changed();
        self.refresh_all();
    }

    fn add_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn()>| {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(move |_, _| f());
            self.app.add_action(&a);
        };
        let ui = self.clone();
        add("import-vcf", Box::new(move || ui.import_dialog()));
        let ui = self.clone();
        add("export-vcf", Box::new(move || ui.export_dialog()));
        let ui = self.clone();
        add("blocked", Box::new(move || ui.show_blocked()));
        let ui = self.clone();
        add(
            "clear-log",
            Box::new(move || {
                let dialog = adw::AlertDialog::new(
                    Some("Clear call history?"),
                    Some("Every entry in Recents is deleted."),
                );
                dialog.add_responses(&[("cancel", "Cancel"), ("clear", "Clear")]);
                dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
                let ui2 = ui.clone();
                dialog.connect_response(None, move |_, r| {
                    if r == "clear" {
                        if let Some(log) = &ui2.log {
                            let _ = log.clear();
                        }
                        ui2.refresh_recents();
                    }
                });
                dialog.present(Some(&ui.window));
            }),
        );
    }

    fn tab_changed(self: &Rc<Self>) {
        let tab = self.views.visible_child_name().unwrap_or_default();
        let contacts = tab == "contacts";
        self.header_add.set_visible(contacts);
        if tab == "recents" {
            if let Some(log) = &self.log {
                let _ = log.mark_all_seen();
            }
            self.update_missed_badge();
            if let Some(p) = self.phone.borrow().clone() {
                if self.available.get() {
                    glib::spawn_future_local(async move {
                        let _ = p.mark_missed_seen().await;
                    });
                }
            }
        }
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    pub fn set_available(&self, on: bool) {
        self.available.set(on);
        self.banner.set_revealed(!on);
        self.call_button.set_sensitive(on);
    }

    // ---- command lines -------------------------------------------------

    fn handle(self: &Rc<Self>, cli: Cli) {
        if let Some(path) = &cli.import {
            self.import_file(path);
        }
        if cli.contacts {
            self.views.set_visible_child_name("contacts");
        } else if cli.recents {
            self.views.set_visible_child_name("recents");
        } else if cli.favourites {
            self.views.set_visible_child_name("favourites");
        } else if cli.keypad {
            self.views.set_visible_child_name("keypad");
        }
        if let Some(uri) = &cli.uri {
            match util::parse_dial_uri(uri) {
                Some(n) => {
                    self.nav.pop_to_tag("root");
                    self.top.set_visible_child_name("main");
                    self.views.set_visible_child_name("keypad");
                    self.set_dial_text(&n);
                }
                None => self.toast("That link has no phone number"),
            }
        }
        if cli.incoming.is_some() || cli.in_call {
            let ui = self.clone();
            let want = cli.incoming.clone();
            glib::spawn_future_local(async move {
                ui.sync_calls().await;
                if let Some(id) = want {
                    if ui.calls.borrow().contains_key(&id) {
                        *ui.shown.borrow_mut() = Some(id);
                    }
                }
                ui.show_current();
            });
        }
        self.window.present();
        if let Some(text) = cli.script {
            if let Ok(steps) = script::parse(&text) {
                glib::spawn_future_local(self.clone().run_script(steps));
            }
        }
    }

    // ---- the agent -----------------------------------------------------

    async fn connect(ui: Rc<Ui>) {
        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "no session bus");
                return;
            }
        };
        let phone = Phone1Proxy::builder(&conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await;
        let messages = Messages1Proxy::builder(&conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await;
        let (Ok(phone), Ok(messages)) = (phone, messages) else {
            tracing::warn!("cannot make the agent's proxies");
            return;
        };
        *ui.phone.borrow_mut() = Some(phone.clone());
        *ui.messages.borrow_mut() = Some(messages);

        // Signals first, so nothing is missed between the check and them.
        let u = ui.clone();
        let p = phone.clone();
        glib::spawn_future_local(async move {
            let Ok(mut s) = p.receive_incoming_call().await else {
                return;
            };
            while let Some(sig) = s.next().await {
                if let Ok(a) = sig.args() {
                    u.on_incoming(a.id, a.number, a.name);
                }
            }
        });
        let u = ui.clone();
        let p = phone.clone();
        glib::spawn_future_local(async move {
            let Ok(mut s) = p.receive_call_state_changed().await else {
                return;
            };
            while let Some(sig) = s.next().await {
                if let Ok(a) = sig.args() {
                    u.on_call_state(a.call);
                }
            }
        });
        let u = ui.clone();
        let p = phone.clone();
        glib::spawn_future_local(async move {
            let Ok(mut s) = p.receive_call_log_changed().await else {
                return;
            };
            while s.next().await.is_some() {
                u.refresh_recents();
                u.refresh_favourites();
            }
        });
        let u = ui.clone();
        let p = phone.clone();
        glib::spawn_future_local(async move {
            let Ok(mut s) = p.inner().receive_owner_changed().await else {
                return;
            };
            while let Some(owner) = s.next().await {
                u.service_changed(owner.is_some()).await;
            }
        });

        let has = match zbus::fdo::DBusProxy::new(&conn).await {
            Ok(d) => match zbus::names::BusName::try_from(omatel_api::PHONE_NAME) {
                Ok(name) => d.name_has_owner(name).await.unwrap_or(false),
                Err(_) => false,
            },
            Err(_) => false,
        };
        ui.service_changed(has).await;
    }

    async fn service_changed(self: &Rc<Self>, up: bool) {
        tracing::info!(up, "phone service");
        self.set_available(up);
        if !up {
            return;
        }
        let phone = self.phone.borrow().clone();
        if let Some(p) = phone {
            if let Ok(list) = p.emergency_numbers().await {
                *self.emergency_numbers.borrow_mut() = list;
            }
        }
        self.sync_calls().await;
        if self.shown.borrow().is_some() || !self.calls.borrow().is_empty() {
            self.show_current();
        }
    }

    /// Fetch the calls in progress from the agent.
    async fn sync_calls(self: &Rc<Self>) {
        let phone = self.phone.borrow().clone();
        let Some(p) = phone else {
            return;
        };
        if let Ok(list) = p.list_calls().await {
            let mut calls = self.calls.borrow_mut();
            calls.retain(|_, c| self.fake.get() && !c.is_ended());
            for c in list.into_iter().filter(|c| !c.is_ended()) {
                calls.insert(c.id.clone(), c);
            }
        }
    }

    fn on_incoming(self: &Rc<Self>, id: String, number: String, name: String) {
        tracing::info!("incoming call");
        let info = CallInfo {
            id: id.clone(),
            number,
            name,
            direction: "incoming".into(),
            state: "ringing-in".into(),
            started_ms: omatel_store::now_ms(),
            answered_ms: 0,
        };
        self.calls.borrow_mut().insert(id.clone(), info);
        *self.shown.borrow_mut() = Some(id);
        self.show_current();
        self.window.present();
    }

    pub fn on_call_state(self: &Rc<Self>, info: CallInfo) {
        tracing::info!(state = %info.state, "call state");
        let id = info.id.clone();
        let ended = info.is_ended();
        let first = !self.calls.borrow().contains_key(&id);
        self.calls.borrow_mut().insert(id.clone(), info.clone());
        let shown = self.shown.borrow().clone();
        if ended {
            if shown.as_deref() == Some(id.as_str()) {
                self.fill_call_page(&info);
                if self.top.visible_child_name().as_deref() == Some("incoming") {
                    self.top.set_visible_child_name("call");
                }
                let ui = self.clone();
                glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                    ui.calls.borrow_mut().remove(&id);
                    if ui.shown.borrow().as_deref() == Some(id.as_str()) {
                        *ui.shown.borrow_mut() = None;
                        ui.show_current();
                    }
                });
            } else {
                self.calls.borrow_mut().remove(&id);
            }
            self.refresh_recents();
            return;
        }
        if shown.is_none()
            || shown.as_deref() == Some(id.as_str())
            || (first && info.is_ringing_in())
        {
            *self.shown.borrow_mut() = Some(id);
            self.show_current();
        }
    }

    /// Show the page for the shown call, or the next live call, or the
    /// tabs when there is none.
    pub fn show_current(self: &Rc<Self>) {
        let mut shown = self.shown.borrow().clone();
        let live = |c: &CallInfo| !c.is_ended();
        if shown
            .as_ref()
            .is_none_or(|id| !self.calls.borrow().get(id).is_some_and(live))
        {
            // Prefer a ringing call, then any other.
            let calls = self.calls.borrow();
            shown = calls
                .values()
                .find(|c| c.is_ringing_in())
                .or_else(|| calls.values().find(|c| live(c)))
                .map(|c| c.id.clone());
        }
        *self.shown.borrow_mut() = shown.clone();
        let info = shown.and_then(|id| self.calls.borrow().get(&id).cloned());
        match info {
            None => self.top.set_visible_child_name("main"),
            Some(c) if c.is_ringing_in() => {
                self.fill_incoming_page(&c);
                self.top.set_visible_child_name("incoming");
            }
            Some(c) => {
                self.fill_call_page(&c);
                self.top.set_visible_child_name("call");
            }
        }
    }

    fn shown_call(&self) -> Option<CallInfo> {
        let id = self.shown.borrow().clone()?;
        self.calls.borrow().get(&id).cloned()
    }

    fn tick(&self) {
        if let Some(c) = self.shown_call() {
            if c.state == "active" {
                self.call
                    .status
                    .set_text(&util::call_status("active", Some(talked_secs(&c))));
            }
        }
    }

    // ---- dialling --------------------------------------------------------

    /// Place a call, unless it's an emergency number (not supported yet:
    /// the warning page instead, and the agent is never asked).
    pub fn dial(self: &Rc<Self>, raw: &str) {
        let mut n = number::sanitize_dial_string(raw);
        if n.is_empty() {
            // As on Android: the call button on an empty dialpad brings
            // back the last number dialled.
            if let Some(last) = self.last_dialled() {
                self.set_dial_text(&last);
            }
            return;
        }
        if number::is_emergency(&n, &self.emergency_numbers.borrow()) {
            self.show_emergency();
            return;
        }
        if !self.available.get() {
            self.toast("Phone service isn't running");
            return;
        }
        let Some(phone) = self.phone.borrow().clone() else {
            return;
        };
        let ui = self.clone();
        n = n.trim().to_string();
        glib::spawn_future_local(async move {
            match phone.dial(&n).await {
                Ok(id) => {
                    let info = CallInfo {
                        id: id.clone(),
                        number: n.clone(),
                        name: ui.book.borrow().name_for(&n).unwrap_or_default(),
                        direction: "outgoing".into(),
                        state: "dialing".into(),
                        started_ms: omatel_store::now_ms(),
                        answered_ms: 0,
                    };
                    ui.calls.borrow_mut().entry(id.clone()).or_insert(info);
                    *ui.shown.borrow_mut() = Some(id);
                    ui.set_dial_text("");
                    ui.show_current();
                }
                Err(zbus::Error::MethodError(name, msg, _))
                    if name.as_str() == omatel_api::errors::EMERGENCY_UNSUPPORTED =>
                {
                    let _ = msg;
                    ui.show_emergency();
                }
                Err(zbus::Error::MethodError(_, msg, _)) => {
                    ui.toast(&format!(
                        "Couldn't call: {}",
                        msg.unwrap_or_else(|| "unknown error".into())
                    ));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "dial failed");
                    ui.toast("Couldn't call");
                }
            }
        });
    }

    fn last_dialled(&self) -> Option<String> {
        let log = self.log.as_ref()?;
        log.recent(50)
            .ok()?
            .into_iter()
            .find(|c| c.direction == omatel_store::CallDirection::Out)
            .map(|c| c.raw_number)
    }

    /// Open a conversation in Messages with this number.
    pub fn message(self: &Rc<Self>, raw: &str) {
        let n = raw.to_string();
        let proxy = self.messages.borrow().clone();
        let ui = self.clone();
        glib::spawn_future_local(async move {
            if let Some(m) = proxy {
                if m.open_conversation(&n).await.is_ok() {
                    return;
                }
            }
            let uri = format!("sms:{}", number::sanitize_dial_string(&n));
            if let Err(e) = std::process::Command::new("omamessages").arg(uri).spawn() {
                tracing::warn!(error = %e, "cannot start omamessages");
                ui.toast("Messages isn't installed");
            }
        });
    }

    // ---- the emergency page -----------------------------------------------

    fn build_emergency_page(self: &Rc<Self>) {
        let b = &self.emergency;
        b.set_valign(gtk::Align::Center);
        b.set_margin_start(24);
        b.set_margin_end(24);
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        icon.set_pixel_size(96);
        icon.add_css_class("warning");
        let title = gtk::Label::builder()
            .label("This phone can't call emergency services yet")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["emergency-title"])
            .build();
        let text = gtk::Label::builder()
            .label(omatel_api::EMERGENCY_WARNING)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let more = gtk::Label::builder()
            .label("The call was not placed. Use another phone to reach emergency services.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        let close = gtk::Button::builder()
            .label("Close")
            .css_classes(["pill", "suggested-action"])
            .halign(gtk::Align::Center)
            .margin_top(12)
            .height_request(56)
            .width_request(200)
            .build();
        b.append(&icon);
        b.append(&title);
        b.append(&text);
        b.append(&more);
        b.append(&close);
        let ui = self.clone();
        close.connect_clicked(move |_| {
            ui.set_dial_text("");
            ui.show_current();
        });
    }

    pub fn show_emergency(&self) {
        tracing::info!("emergency number dialled: warning shown, nothing placed");
        self.top.set_visible_child_name("emergency");
        self.window.present();
    }

    // ---- the call page -------------------------------------------------------

    fn wire_call_page(self: &Rc<Self>) {
        let ui = self.clone();
        self.call.mute.connect_toggled(move |b| {
            if ui.updating.get() {
                return;
            }
            let on = b.is_active();
            b.set_icon_name(if on {
                "microphone-disabled-symbolic"
            } else {
                "microphone-sensitivity-high-symbolic"
            });
            if let Some(p) = ui.agent_call() {
                glib::spawn_future_local(async move {
                    if let Err(e) = p.set_mute(on).await {
                        tracing::warn!(error = %e, "mute failed");
                    }
                });
            }
        });
        let ui = self.clone();
        self.call.speaker.connect_toggled(move |b| {
            if ui.updating.get() {
                return;
            }
            let route = if b.is_active() { "speaker" } else { "earpiece" };
            if let Some(p) = ui.agent_call() {
                let ui2 = ui.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = p.set_route(route).await {
                        tracing::warn!(error = %e, "route failed");
                    }
                    let routing = p.audio_routing().await.unwrap_or(false);
                    ui2.call.route_note.set_visible(!routing);
                });
            }
        });
        let ui = self.clone();
        self.call.keypad.connect_toggled(move |b| {
            ui.call.dtmf.set_reveal_child(b.is_active());
        });
        let ui = self.clone();
        self.call.hangup.connect_clicked(move |_| ui.hang_up());
        // The in-call dialpad sends touch tones.
        let ui = self.clone();
        let pad = build_dialpad(Rc::new(move |d| ui.send_dtmf(d)), false);
        if let Some(b) = self.call.dtmf.child().and_downcast::<gtk::Box>() {
            b.append(&pad);
        }
    }

    /// The proxy for a request about the shown call, unless it's a fake.
    fn agent_call(&self) -> Option<Phone1Proxy<'static>> {
        if self.fake.get() {
            return None;
        }
        self.phone.borrow().clone()
    }

    fn send_dtmf(self: &Rc<Self>, d: char) {
        let mut text = self.call.dtmf_digits.text().to_string();
        text.push(d);
        self.call.dtmf_digits.set_text(&text);
        let (Some(c), Some(p)) = (self.shown_call(), self.agent_call()) else {
            return;
        };
        glib::spawn_future_local(async move {
            if let Err(e) = p.send_dtmf(&c.id, &d.to_string()).await {
                tracing::warn!(error = %e, "touch tone failed");
            }
        });
    }

    fn hang_up(self: &Rc<Self>) {
        let Some(c) = self.shown_call() else {
            self.show_current();
            return;
        };
        match self.agent_call() {
            Some(p) => {
                let ui = self.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = p.hangup(&c.id).await {
                        tracing::warn!(error = %e, "hangup failed");
                        // The agent lost it: don't leave the page up.
                        let mut ended = c.clone();
                        ended.state = "terminated".into();
                        ui.on_call_state(ended);
                    }
                });
            }
            None => {
                let mut ended = c;
                ended.state = "terminated".into();
                self.on_call_state(ended);
            }
        }
    }

    fn fill_call_page(&self, c: &CallInfo) {
        let name = if c.name.is_empty() {
            self.book.borrow().name_for(&c.number)
        } else {
            Some(c.name.clone())
        };
        let shown_number = number::display(&c.number, &self.region);
        match &name {
            Some(n) => {
                self.call.name.set_text(n);
                self.call.number.set_text(&shown_number);
                self.call.number.set_visible(true);
            }
            None => {
                self.call.name.set_text(&shown_number);
                self.call.number.set_visible(false);
            }
        }
        self.call
            .avatar
            .set_text(Some(name.as_deref().unwrap_or("")));
        self.call.avatar.set_show_initials(name.is_some());
        let talked = (c.answered_ms > 0).then(|| talked_secs(c));
        self.call
            .status
            .set_text(&util::call_status(&c.state, talked));
        let live = !c.is_ended();
        self.call.hangup.set_sensitive(live);
        self.call.mute.set_sensitive(live);
        self.call.speaker.set_sensitive(live);
        self.call.keypad.set_sensitive(c.state == "active");
        if !live {
            self.updating.set(true);
            self.call.keypad.set_active(false);
            self.updating.set(false);
        }
    }

    fn wire_incoming_page(self: &Rc<Self>) {
        let ui = self.clone();
        let answer = find_button(&self.incoming.root, "answer");
        let decline = find_button(&self.incoming.root, "decline");
        let reply = find_button(&self.incoming.root, "reply");
        answer.connect_clicked(move |_| ui.answer());
        let ui = self.clone();
        decline.connect_clicked(move |_| ui.decline(None));
        let ui = self.clone();
        reply.connect_clicked(move |_| {
            let r = &ui.incoming.replies;
            r.set_reveal_child(!r.reveals_child());
        });
        for text in omatel_api::settings::Settings::load().quick_replies {
            let row = adw::ActionRow::builder()
                .title(text.as_str())
                .activatable(true)
                .build();
            let ui = self.clone();
            row.connect_activated(move |_| ui.decline(Some(text.clone())));
            self.incoming.replies_list.append(&row);
        }
    }

    fn fill_incoming_page(&self, c: &CallInfo) {
        let name = if c.name.is_empty() {
            self.book.borrow().name_for(&c.number)
        } else {
            Some(c.name.clone())
        };
        let shown_number = if c.number.is_empty() {
            "Unknown caller".to_string()
        } else {
            number::display(&c.number, &self.region)
        };
        match &name {
            Some(n) => {
                self.incoming.name.set_text(n);
                self.incoming.number.set_text(&shown_number);
                self.incoming.number.set_visible(true);
            }
            None => {
                self.incoming.name.set_text(&shown_number);
                self.incoming.number.set_visible(false);
            }
        }
        self.incoming
            .avatar
            .set_text(Some(name.as_deref().unwrap_or("")));
        self.incoming.avatar.set_show_initials(name.is_some());
        self.incoming.replies.set_reveal_child(false);
    }

    fn answer(self: &Rc<Self>) {
        let Some(c) = self.shown_call() else {
            return;
        };
        match self.agent_call() {
            Some(p) => {
                let ui = self.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = p.answer(&c.id).await {
                        tracing::warn!(error = %e, "answer failed");
                        ui.toast("Couldn't answer");
                    }
                });
            }
            None => {
                let mut a = c;
                a.state = "active".into();
                a.answered_ms = omatel_store::now_ms();
                self.on_call_state(a);
            }
        }
    }

    /// Decline the shown call, with a text message to the caller.
    fn decline(self: &Rc<Self>, reply: Option<String>) {
        let Some(c) = self.shown_call() else {
            return;
        };
        if let (Some(text), Some(m)) = (reply, self.messages.borrow().clone()) {
            if !self.fake.get() && !c.number.is_empty() {
                let n = c.number.clone();
                let ui = self.clone();
                glib::spawn_future_local(async move {
                    match m.send(&n, &text).await {
                        Ok(_) => ui.toast("Message sent"),
                        Err(e) => {
                            tracing::warn!(error = %e, "quick reply failed");
                            ui.toast("Couldn't send the message");
                        }
                    }
                });
            }
        }
        match self.agent_call() {
            Some(p) => {
                glib::spawn_future_local(async move {
                    if let Err(e) = p.decline(&c.id).await {
                        tracing::warn!(error = %e, "decline failed");
                    }
                });
            }
            None => {
                let mut d = c;
                d.state = "terminated".into();
                self.on_call_state(d);
            }
        }
    }

    // ---- contacts on disk ------------------------------------------------------

    /// Watch the address book's directories; reload after a burst settles.
    fn watch_contacts(self: &Rc<Self>) {
        let root = self.book.borrow().root().to_path_buf();
        let mut dirs = vec![root.clone()];
        if let Ok(rd) = std::fs::read_dir(&root) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
        let mut monitors = Vec::new();
        for d in dirs {
            let Ok(m) = gio::File::for_path(&d)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            else {
                continue;
            };
            let ui = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, _| {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                if let Some(id) = ui.reload_pending.borrow_mut().take() {
                    id.remove();
                }
                let u = ui.clone();
                let id = glib::timeout_add_local_once(Duration::from_millis(300), move || {
                    u.reload_pending.borrow_mut().take();
                    if u.book.borrow_mut().reload_if_changed() {
                        u.refresh_contacts_everywhere();
                    }
                    u.watch_contacts();
                });
                *ui.reload_pending.borrow_mut() = Some(id);
            });
            monitors.push(m);
        }
        *self.monitors.borrow_mut() = monitors;
    }

    pub fn refresh_contacts_everywhere(self: &Rc<Self>) {
        self.refresh_all();
        self.update_suggestions();
    }

    pub fn refresh_all(self: &Rc<Self>) {
        self.refresh_favourites();
        self.refresh_recents();
        self.refresh_contacts();
    }

    pub fn import_file(self: &Rc<Self>, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let r = self.book.borrow_mut().import(&text);
                match r {
                    Ok(n) => {
                        self.toast(&format!(
                            "Imported {n} contact{}",
                            if n == 1 { "" } else { "s" }
                        ));
                        tracing::info!(count = n, "contacts imported");
                        self.refresh_contacts_everywhere();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "import failed");
                        self.toast("Couldn't import those contacts");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "import file unreadable");
                self.toast("Couldn't read that file");
            }
        }
    }

    fn import_dialog(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Contacts (.vcf)"));
        filter.add_pattern("*.vcf");
        filter.add_pattern("*.VCF");
        filter.add_mime_type("text/vcard");
        filter.add_mime_type("text/x-vcard");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import contacts")
            .filters(&filters)
            .build();
        let ui = self.clone();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |r| {
            if let Ok(f) = r {
                if let Some(p) = f.path() {
                    ui.import_file(&p);
                }
            }
        });
    }

    fn export_dialog(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Export contacts")
            .initial_name("contacts.vcf")
            .build();
        let ui = self.clone();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |r| {
            if let Ok(f) = r {
                if let Some(p) = f.path() {
                    let text = ui.book.borrow().export();
                    match std::fs::write(&p, text) {
                        Ok(()) => ui.toast("Contacts exported"),
                        Err(e) => {
                            tracing::warn!(error = %e, "export failed");
                            ui.toast("Couldn't write that file");
                        }
                    }
                }
            }
        });
    }

    // ---- the test script ---------------------------------------------------------

    async fn run_script(self: Rc<Self>, steps: Vec<Step>) {
        // Let the window map first.
        glib::timeout_future(Duration::from_millis(300)).await;
        for step in steps {
            tracing::info!(?step, "script");
            match step {
                Step::Wait(d) => glib::timeout_future(d).await,
                Step::Tab(t) => {
                    self.top.set_visible_child_name("main");
                    self.nav.pop_to_tag("root");
                    self.views.set_visible_child_name(&t);
                }
                Step::Type(digits) => {
                    self.views.set_visible_child_name("keypad");
                    for d in digits.chars() {
                        self.press_key(d);
                    }
                }
                Step::OpenContact(n) => {
                    let uid = self.contact_uids.borrow().get(n - 1).cloned();
                    match uid {
                        Some(u) => self.show_contact(&u),
                        None => tracing::warn!(n, "no such contact row"),
                    }
                }
                Step::EditContact => {
                    let uid = self.open_contact.borrow().clone();
                    self.edit_contact(uid, None);
                }
                Step::NewContact => self.edit_contact(None, None),
                Step::Back => {
                    self.nav.pop();
                }
                Step::Details(n) => {
                    let num = self.recent_numbers.borrow().get(n - 1).cloned();
                    match num {
                        Some(num) => self.show_details(&num),
                        None => tracing::warn!(n, "no such recents row"),
                    }
                }
                Step::FakeIncoming(num) => {
                    self.fake.set(true);
                    let name = self.book.borrow().name_for(&num).unwrap_or_default();
                    self.on_incoming("fake/1".into(), num, name);
                }
                Step::FakeCall(num, state) => {
                    self.fake.set(true);
                    if state != "ended" {
                        // A fresh call replaces whatever was shown.
                        self.calls.borrow_mut().clear();
                        *self.shown.borrow_mut() = None;
                    }
                    let now = omatel_store::now_ms();
                    let info = CallInfo {
                        id: "fake/2".into(),
                        name: self.book.borrow().name_for(&num).unwrap_or_default(),
                        number: num,
                        direction: "outgoing".into(),
                        state: match state.as_str() {
                            "ended" => "terminated".into(),
                            s => s.into(),
                        },
                        started_ms: now - 80_000,
                        answered_ms: if state == "dialing" { 0 } else { now - 75_000 },
                    };
                    self.on_call_state(info);
                }
                Step::Emergency => self.show_emergency(),
                Step::Shot(p) => {
                    if let Err(e) = self.shot_after_paint(p).await {
                        tracing::warn!(error = %e, "shot failed");
                    }
                }
                Step::Quit => {
                    self.app.quit();
                    return;
                }
            }
        }
    }

    /// Wait until the window has painted a frame (at most 500 ms), so its
    /// render nodes are fresh for a snapshot (as omacam does it).
    async fn shot_after_paint(self: &Rc<Self>, path: PathBuf) -> anyhow::Result<()> {
        let Some(clock) = self.window.frame_clock() else {
            return self.render_png(&path);
        };
        self.window.queue_draw();
        let (tx, rx) = async_channel::bounded::<anyhow::Result<()>>(2);
        let done = Rc::new(Cell::new(false));
        let (ui, p, t, d) = (Rc::downgrade(self), path.clone(), tx.clone(), done.clone());
        let id = clock.connect_after_paint(move |_| {
            if d.replace(true) {
                return;
            }
            if let Some(u) = ui.upgrade() {
                let _ = t.try_send(u.render_png(&p));
            }
        });
        let ui = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if done.replace(true) {
                return;
            }
            if let Some(u) = ui.upgrade() {
                let _ = tx.try_send(u.render_png(&path));
            }
        });
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("no result")));
        clock.disconnect(id);
        result
    }

    fn render_png(&self, path: &Path) -> anyhow::Result<()> {
        let content: gtk::Widget = self.window.clone().upcast();
        let (w, h) = (content.width() as f32, content.height() as f32);
        if w <= 0.0 || h <= 0.0 || !content.is_mapped() {
            anyhow::bail!("window not drawn");
        }
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(&content)).snapshot(&snapshot, w as f64, h as f64);
        let node = snapshot
            .to_node()
            .ok_or_else(|| anyhow::anyhow!("nothing drawn"))?;
        let renderer = match self.window.renderer() {
            Some(r) => r,
            None => {
                let r = gtk::gsk::CairoRenderer::new();
                r.realize_for_display(&WidgetExt::display(&self.window))?;
                r.upcast()
            }
        };
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, w, h);
        let texture = renderer.render_texture(node, Some(&bounds));
        texture.save_to_png(path)?;
        tracing::info!(path = %path.display(), "window saved");
        Ok(())
    }
}

/// Seconds talked so far.
fn talked_secs(c: &CallInfo) -> i64 {
    if c.answered_ms <= 0 {
        return 0;
    }
    (omatel_store::now_ms() - c.answered_ms) / 1000
}

fn find_button(root: &gtk::Box, name: &str) -> gtk::Button {
    let mut child = root.first_child();
    let mut stack = Vec::new();
    loop {
        while let Some(w) = child {
            if w.widget_name() == name {
                if let Ok(b) = w.clone().downcast::<gtk::Button>() {
                    return b;
                }
            }
            stack.push(w.clone());
            child = w.next_sibling();
        }
        match stack.pop() {
            Some(w) => child = w.first_child(),
            None => break,
        }
    }
    panic!("no button named {name}");
}

/// One dialpad key: the digit and its letters.
pub fn key_button(d: char) -> gtk::Button {
    let v = gtk::Box::new(gtk::Orientation::Vertical, 0);
    v.set_valign(gtk::Align::Center);
    let digit = gtk::Label::builder()
        .label(d.to_string())
        .css_classes(["digit"])
        .build();
    let letters = gtk::Label::builder()
        .label(match number::t9_letters(d) {
            "" => " ",
            l => l,
        })
        .css_classes(["letters"])
        .build();
    v.append(&digit);
    v.append(&letters);
    let b = gtk::Button::builder()
        .child(&v)
        .css_classes(["dialpad-key"])
        .build();
    b.update_property(&[gtk::accessible::Property::Label(&d.to_string())]);
    b
}

/// A 3x4 dialpad calling `on_digit`. With `plus_on_zero`, a long press
/// on 0 gives '+'.
pub fn build_dialpad(on_digit: Rc<dyn Fn(char)>, plus_on_zero: bool) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .row_spacing(10)
        .column_spacing(20)
        .halign(gtk::Align::Center)
        .row_homogeneous(true)
        .column_homogeneous(true)
        .build();
    for (i, d) in "123456789*0#".chars().enumerate() {
        let b = key_button(d);
        let long = Rc::new(Cell::new(false));
        if d == '0' && plus_on_zero {
            let g = gtk::GestureLongPress::new();
            g.set_propagation_phase(gtk::PropagationPhase::Capture);
            let f = on_digit.clone();
            let l = long.clone();
            g.connect_pressed(move |_, _, _| {
                l.set(true);
                f('+');
            });
            b.add_controller(g);
        }
        let f = on_digit.clone();
        b.connect_clicked(move |_| {
            if long.replace(false) {
                return;
            }
            f(d);
        });
        grid.attach(&b, (i % 3) as i32, (i / 3) as i32, 1, 1);
    }
    grid
}

fn round_toggle(icon: &str, label: &str) -> (gtk::Box, gtk::ToggleButton) {
    let b = gtk::ToggleButton::builder()
        .icon_name(icon)
        .css_classes(["incall-toggle"])
        .tooltip_text(label)
        .halign(gtk::Align::Center)
        .build();
    let l = gtk::Label::builder()
        .label(label)
        .css_classes(["incall-label"])
        .build();
    let v = gtk::Box::new(gtk::Orientation::Vertical, 6);
    v.append(&b);
    v.append(&l);
    (v, b)
}

fn build_call_page() -> CallPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("incall-page");
    let avatar = adw::Avatar::new(96, None, false);
    avatar.set_margin_top(32);
    let name = gtk::Label::builder()
        .css_classes(["caller-name"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let number = gtk::Label::builder().css_classes(["caller-number"]).build();
    let status = gtk::Label::builder().css_classes(["call-status"]).build();
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    let dtmf_digits = gtk::Label::builder()
        .css_classes(["dtmf-digits"])
        .ellipsize(gtk::pango::EllipsizeMode::Start)
        .build();
    let dtmf_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    dtmf_box.append(&dtmf_digits);
    let dtmf = gtk::Revealer::builder()
        .child(&dtmf_box)
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .build();
    let (mute_box, mute) = round_toggle("microphone-sensitivity-high-symbolic", "Mute");
    let (speaker_box, speaker) = round_toggle("audio-volume-high-symbolic", "Speaker");
    let (keypad_box, keypad) = round_toggle("input-dialpad-symbolic", "Keypad");
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(32)
        .halign(gtk::Align::Center)
        .homogeneous(true)
        .build();
    row.append(&mute_box);
    row.append(&speaker_box);
    row.append(&keypad_box);
    let route_note = gtk::Label::builder()
        .label("Audio routing arrives with call audio support")
        .css_classes(["route-note"])
        .visible(false)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let hangup = gtk::Button::builder()
        .icon_name("call-stop-symbolic")
        .css_classes(["hangup-button"])
        .tooltip_text("Hang up")
        .halign(gtk::Align::Center)
        .margin_top(16)
        .margin_bottom(16)
        .build();
    for w in [
        avatar.upcast_ref::<gtk::Widget>(),
        name.upcast_ref(),
        number.upcast_ref(),
        status.upcast_ref(),
        spacer.upcast_ref(),
        dtmf.upcast_ref(),
        row.upcast_ref(),
        route_note.upcast_ref(),
        hangup.upcast_ref(),
    ] {
        root.append(w);
    }
    CallPage {
        root,
        avatar,
        name,
        number,
        status,
        mute,
        speaker,
        keypad,
        dtmf,
        dtmf_digits,
        route_note,
        hangup,
    }
}

fn build_incoming_page() -> IncomingPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("incoming-page");
    let label = gtk::Label::builder()
        .label("Incoming call")
        .css_classes(["call-status"])
        .margin_top(24)
        .build();
    let avatar = adw::Avatar::new(96, None, false);
    avatar.set_margin_top(16);
    let name = gtk::Label::builder()
        .css_classes(["caller-name"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let number = gtk::Label::builder().css_classes(["caller-number"]).build();
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    let replies_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let replies = gtk::Revealer::builder()
        .child(&replies_list)
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .build();
    let reply = gtk::Button::builder()
        .label("Reply with message")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .height_request(48)
        .name("reply")
        .build();
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(96)
        .halign(gtk::Align::Center)
        .margin_top(12)
        .margin_bottom(24)
        .build();
    let col = |b: &gtk::Button, text: &str| {
        let v = gtk::Box::new(gtk::Orientation::Vertical, 6);
        v.append(b);
        v.append(
            &gtk::Label::builder()
                .label(text)
                .css_classes(["incall-label"])
                .build(),
        );
        v
    };
    let decline = gtk::Button::builder()
        .icon_name("call-stop-symbolic")
        .css_classes(["hangup-button"])
        .tooltip_text("Decline")
        .name("decline")
        .build();
    let answer = gtk::Button::builder()
        .icon_name("call-start-symbolic")
        .css_classes(["call-button"])
        .tooltip_text("Answer")
        .name("answer")
        .build();
    buttons.append(&col(&decline, "Decline"));
    buttons.append(&col(&answer, "Answer"));
    for w in [
        label.upcast_ref::<gtk::Widget>(),
        avatar.upcast_ref(),
        name.upcast_ref(),
        number.upcast_ref(),
        spacer.upcast_ref(),
        replies.upcast_ref(),
        reply.upcast_ref(),
        buttons.upcast_ref(),
    ] {
        root.append(w);
    }
    IncomingPage {
        root,
        avatar,
        name,
        number,
        replies,
        replies_list,
    }
}
