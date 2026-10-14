// SPDX-License-Identifier: GPL-3.0-or-later
//! The window: the conversation list, threads, compose, archived and
//! blocked pages, settings, and the script runner for screenshots.
//!
//! Data comes from the message store the agent writes; sending, retrying
//! and marking read go through the agent's `org.omamobile.Messages1`. With
//! the agent away, everything but sending still works.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use futures_util::StreamExt;
use gtk::{gio, glib, pango};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use omatel_api::settings::Settings;
use omatel_api::Messages1Proxy;
use omatel_contacts::{number, AddressBook};
use omatel_store::{BlockList, Conversation, Direction, Message, MessageState, MessageStore};

use crate::format;
use crate::script::{self, Step};
use crate::segments;
use crate::uri;
use crate::Cli;

/// One button of an action sheet: label, destructive, what it does.
type SheetItem = (String, bool, Box<dyn Fn()>);
type UiAction = Box<dyn Fn(&Rc<Ui>)>;
type FlagSetter = fn(&MessageStore, i64, bool) -> omatel_store::Result<()>;
type Flag = (&'static str, fn(&Conversation) -> bool, FlagSetter);

pub const APP_ID: &str = "org.omamobile.Messages";
/// Messages shown when a thread opens; scrolling to the top loads more.
const PAGE: usize = 60;

const EXTRA_CSS: &str = "
.conversation-row { padding: 10px 12px; }
.conversation-row .name { font-weight: 600; }
.conversation-row .unread-name { font-weight: 800; }
.unread-badge {
  background-color: var(--accent-bg-color);
  color: var(--accent-fg-color);
  border-radius: 999px;
  min-width: 22px;
  padding: 0 6px;
  font-size: 12px;
  font-weight: 700;
}
.draft-prefix { color: var(--accent-color); }
.failed-prefix { color: var(--destructive-color); }
.thread-box { padding: 8px 10px 12px 10px; }
.bubble-row { margin-top: 2px; }
.sheet-button { min-height: 52px; font-size: 16px; }
.counter { font-size: 12px; opacity: 0.75; }
.composer textview { background: none; padding: 10px 4px; font-size: 16px; }
.composer .text-frame {
  border-radius: 22px;
  background-color: var(--view-bg-color);
  border: 1px solid alpha(var(--window-fg-color), 0.18);
  padding: 0 12px;
}
.bubble.incoming {
  background-color: alpha(var(--window-fg-color), 0.08);
  color: var(--window-fg-color);
}
.send-button { min-width: 48px; min-height: 48px; }
";

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let ui: Rc<RefCell<Option<Rc<Ui>>>> = Rc::new(RefCell::new(None));
    app.connect_command_line(move |app, cmdline| {
        let cli = match Cli::try_parse_from(cmdline.arguments()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("omamessages: {e}");
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
        ui.window.present();
        ui.handle(cli);
        glib::ExitCode::SUCCESS
    });
    app.run()
}

/// A list of conversations shown on a page (the main list or archived).
struct ConversationList {
    list: gtk::ListBox,
    ids: RefCell<Vec<i64>>,
    stack: gtk::Stack,
    archived: bool,
}

struct Thread {
    conv: i64,
    page: adw::NavigationPage,
    title: adw::WindowTitle,
    messages: gtk::Box,
    scroller: gtk::ScrolledWindow,
    buffer: gtk::TextBuffer,
    view: gtk::TextView,
    send: gtk::Button,
    counter: gtk::Label,
    loaded: Cell<usize>,
    more: Cell<bool>,
    stick_bottom: Cell<bool>,
    anchor: Cell<Option<f64>>,
    saved_draft: RefCell<String>,
    actions: gio::SimpleActionGroup,
}

struct Compose {
    page: adw::NavigationPage,
    to: gtk::Entry,
    suggestions: gtk::ListBox,
    /// Numbers behind the suggestion rows, in order.
    numbers: RefCell<Vec<String>>,
    body: RefCell<Option<String>>,
}

pub struct Ui {
    app: adw::Application,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    nav: adw::NavigationView,
    banners: RefCell<Vec<adw::Banner>>,
    store: MessageStore,
    blocked: BlockList,
    book: RefCell<AddressBook>,
    region: RefCell<String>,
    proxy: RefCell<Option<Messages1Proxy<'static>>>,
    service: Cell<bool>,
    main_list: Rc<ConversationList>,
    archived: RefCell<Option<Rc<ConversationList>>>,
    search: gtk::SearchEntry,
    search_list: gtk::ListBox,
    search_ids: RefCell<Vec<i64>>,
    list_stack: gtk::Stack,
    thread: RefCell<Option<Rc<Thread>>>,
    compose: RefCell<Option<Rc<Compose>>>,
    blocked_list: RefCell<Option<gtk::ListBox>>,
    sheet: RefCell<Option<adw::Dialog>>,
    settings: RefCell<Option<adw::PreferencesDialog>>,
    script_running: Cell<bool>,
    _keep: RefCell<Vec<gio::FileMonitor>>,
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn initials_label(name: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(&omatel_theme::initials(name)));
    l.add_css_class("initials");
    l.set_valign(gtk::Align::Center);
    l.set_size_request(44, 44);
    l
}

fn local_now() -> chrono::DateTime<chrono::Local> {
    chrono::Local::now()
}

impl Ui {
    fn build(app: &adw::Application) -> Rc<Ui> {
        let monitor = omatel_theme::install(EXTRA_CSS);
        let settings = Settings::load();
        let region = settings.region(number::default_region);
        let dir = omatel_store::data_dir();
        let store = MessageStore::open(&dir.join("messages.db")).unwrap_or_else(|e| {
            tracing::error!(error = %e, "message store unavailable; showing an empty one");
            MessageStore::open_in_memory().expect("in-memory store")
        });
        let blocked = BlockList::open(&dir.join("blocked.db")).unwrap_or_else(|e| {
            tracing::error!(error = %e, "block list unavailable");
            BlockList::open_in_memory().expect("in-memory block list")
        });
        let book = AddressBook::open(omatel_contacts::default_root(), &region);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Messages")
            .default_width(420)
            .default_height(820)
            .build();
        let toasts = adw::ToastOverlay::new();
        let nav = adw::NavigationView::new();
        toasts.set_child(Some(&nav));
        window.set_content(Some(&toasts));

        let main_list = Rc::new(ConversationList::new(false));
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search messages")
            .hexpand(true)
            .build();
        let search_list = gtk::ListBox::new();
        search_list.set_selection_mode(gtk::SelectionMode::None);
        search_list.set_show_separators(true);

        let list_stack = gtk::Stack::new();
        let ui = Rc::new(Ui {
            app: app.clone(),
            window,
            toasts,
            nav,
            banners: RefCell::new(Vec::new()),
            store,
            blocked,
            book: RefCell::new(book),
            region: RefCell::new(region),
            proxy: RefCell::new(None),
            service: Cell::new(false),
            main_list,
            archived: RefCell::new(None),
            search,
            search_list,
            search_ids: RefCell::new(Vec::new()),
            list_stack,
            thread: RefCell::new(None),
            compose: RefCell::new(None),
            blocked_list: RefCell::new(None),
            sheet: RefCell::new(None),
            settings: RefCell::new(None),
            script_running: Cell::new(false),
            _keep: RefCell::new(monitor.into_iter().collect()),
        });
        ui.build_list_page();
        ui.install_actions();
        ui.watch_contacts();
        ui.connect_bus();
        let weak = Rc::downgrade(&ui);
        ui.window.connect_is_active_notify(move |w| {
            if w.is_active() {
                if let Some(ui) = weak.upgrade() {
                    ui.refresh_all();
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.save_draft();
            }
            glib::Propagation::Proceed
        });
        ui.refresh_list();
        ui
    }

    /// Act on a command line (the first launch or a forwarded one).
    fn handle(self: &Rc<Self>, cli: Cli) {
        if let Some(id) = cli.conversation {
            if self.store.conversation(id).ok().flatten().is_some() {
                self.open_thread(id);
            } else {
                self.toast("That conversation no longer exists");
            }
        } else if let Some(target) = cli.target.as_deref() {
            let (to, body) = if uri::is_sms_uri(target) {
                let u = uri::parse(target).unwrap_or_default();
                if u.recipients.len() > 1 {
                    self.toast(
                        "Group messages arrive with MMS support; writing to the first number",
                    );
                }
                (u.recipients.into_iter().next(), u.body)
            } else {
                (Some(target.to_string()), None)
            };
            match to {
                Some(n) if !n.trim().is_empty() => {
                    self.open_number(&n);
                    if let (Some(b), Some(t)) = (body, self.thread.borrow().as_ref()) {
                        t.buffer.set_text(&b);
                    }
                }
                _ => {
                    self.open_compose();
                    if let Some(c) = self.compose.borrow().as_ref() {
                        *c.body.borrow_mut() = body;
                    }
                }
            }
        } else if cli.compose {
            self.open_compose();
        }
        if let Some(text) = cli.script {
            match script::parse(&text) {
                Ok(steps) => self.run_script(steps),
                Err(e) => tracing::warn!(error = %e, "bad script"),
            }
        }
    }

    // ---- shared bits -------------------------------------------------------

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn banner(&self) -> adw::Banner {
        let b = adw::Banner::new("Messages service isn't running: you can read, not send");
        b.set_revealed(!self.service.get());
        self.banners.borrow_mut().push(b.clone());
        b
    }

    fn set_service(self: &Rc<Self>, up: bool) {
        if self.service.replace(up) != up {
            tracing::info!(up, "messages service");
        }
        self.banners.borrow_mut().retain(|b| {
            b.set_revealed(!up);
            b.root().is_some() || b.parent().is_some()
        });
        if let Some(t) = self.thread.borrow().as_ref() {
            self.update_send(t);
        }
    }

    fn region(&self) -> String {
        self.region.borrow().clone()
    }

    /// The name a conversation goes by: its title, its contacts' names, or
    /// its numbers formatted for reading.
    fn conversation_name(&self, c: &Conversation) -> String {
        if !c.title.is_empty() {
            return c.title.clone();
        }
        let numbers: Vec<&str> = if c.numbers.is_empty() {
            c.address.split(',').collect()
        } else {
            c.numbers.iter().map(String::as_str).collect()
        };
        numbers
            .iter()
            .map(|n| self.name_for(n))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn name_for(&self, n: &str) -> String {
        self.book
            .borrow()
            .name_for(n)
            .unwrap_or_else(|| number::display(n, &self.region()))
    }

    fn primary_number(c: &Conversation) -> String {
        c.numbers
            .first()
            .cloned()
            .unwrap_or_else(|| c.address.split(',').next().unwrap_or("").to_string())
    }

    fn is_blocked(&self, c: &Conversation) -> bool {
        let region = self.region();
        let numbers: Vec<String> = if c.numbers.is_empty() {
            vec![c.address.clone()]
        } else {
            c.numbers.clone()
        };
        numbers
            .iter()
            .any(|n| self.blocked.is_blocked(n, &region).unwrap_or(false))
    }

    fn watch_contacts(self: &Rc<Self>) {
        let root = self.book.borrow().root().to_path_buf();
        let _ = std::fs::create_dir_all(&root);
        let mut dirs = vec![root.clone()];
        if let Ok(rd) = std::fs::read_dir(&root) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
        for d in dirs {
            if let Ok(m) = gio::File::for_path(&d)
                .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            {
                let weak = Rc::downgrade(self);
                m.connect_changed(move |_, _, _, _| {
                    if let Some(ui) = weak.upgrade() {
                        if ui.book.borrow_mut().reload_if_changed() {
                            ui.refresh_all();
                        }
                    }
                });
                self._keep.borrow_mut().push(m);
            }
        }
    }

    fn refresh_all(self: &Rc<Self>) {
        self.book.borrow_mut().reload_if_changed();
        self.refresh_list();
        self.refresh_archived();
        self.refresh_search();
        let thread = self.thread.borrow().clone();
        if let Some(t) = thread {
            self.refresh_thread(&t);
        }
    }

    // ---- D-Bus -------------------------------------------------------------

    fn connect_bus(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            if let Err(e) = Ui::bus_task(weak).await {
                tracing::warn!(error = %e, "no session bus: sending is unavailable");
            }
        });
    }

    async fn bus_task(weak: std::rc::Weak<Ui>) -> zbus::Result<()> {
        let conn = zbus::Connection::session().await?;
        let proxy = Messages1Proxy::builder(&conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let up = dbus
            .name_has_owner(omatel_api::MESSAGES_NAME.try_into()?)
            .await
            .unwrap_or(false);
        let mut owners = proxy.inner().receive_owner_changed().await?;
        let mut changes = proxy.receive_messages_changed().await?;
        {
            let Some(ui) = weak.upgrade() else {
                return Ok(());
            };
            *ui.proxy.borrow_mut() = Some(proxy);
            ui.set_service(up);
        }
        let weak2 = weak.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Some(owner) = owners.next().await {
                let Some(ui) = weak2.upgrade() else { break };
                ui.set_service(owner.is_some());
                if owner.is_some() {
                    ui.refresh_all();
                }
            }
        });
        while let Some(signal) = changes.next().await {
            let Some(ui) = weak.upgrade() else { break };
            if let Ok(args) = signal.args() {
                tracing::debug!(conversation = args.conversation, "messages changed");
            }
            ui.refresh_all();
        }
        Ok(())
    }

    fn proxy(&self) -> Option<Messages1Proxy<'static>> {
        if self.service.get() {
            self.proxy.borrow().clone()
        } else {
            None
        }
    }

    // ---- the conversation list ---------------------------------------------

    fn build_list_page(self: &Rc<Self>) {
        let header = adw::HeaderBar::new();
        let compose = gtk::Button::from_icon_name("list-add-symbolic");
        compose.set_tooltip_text(Some("New message"));
        let weak = Rc::downgrade(self);
        compose.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_compose();
            }
        });
        let menu = gio::Menu::new();
        menu.append(Some("Archived"), Some("win.archived"));
        menu.append(Some("Blocked numbers"), Some("win.blocked"));
        menu.append(Some("Settings"), Some("win.settings"));
        let data = gio::Menu::new();
        data.append(Some("Export messages…"), Some("win.export"));
        data.append(Some("Import messages…"), Some("win.import"));
        menu.append_section(None, &data);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Menu")
            .build();
        header.pack_end(&menu_button);
        header.pack_end(&compose);

        let search_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        search_bar.set_margin_start(12);
        search_bar.set_margin_end(12);
        search_bar.set_margin_bottom(6);
        search_bar.append(&self.search);
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_search();
            }
        });

        let results = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&self.search_list)
            .vexpand(true)
            .build();
        let weak = Rc::downgrade(self);
        self.search_list.connect_row_activated(move |_, row| {
            if let Some(ui) = weak.upgrade() {
                let id = ui.search_ids.borrow().get(row.index() as usize).copied();
                if let Some(id) = id {
                    ui.open_thread(id);
                }
            }
        });
        self.list_stack
            .add_named(&self.main_list.stack, Some("list"));
        self.list_stack.add_named(&results, Some("search"));
        self.connect_list(&self.main_list);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.add_top_bar(&search_bar);
        tv.add_top_bar(&self.banner());
        tv.set_content(Some(&self.list_stack));
        let page = adw::NavigationPage::builder()
            .title("Messages")
            .tag("list")
            .child(&tv)
            .build();
        self.nav.add(&page);
    }

    fn connect_list(self: &Rc<Self>, cl: &ConversationList) {
        let weak = Rc::downgrade(self);
        let archived = cl.archived;
        cl.list.connect_row_activated(move |_, row| {
            if let Some(ui) = weak.upgrade() {
                let id = ui.list_for(archived).and_then(|l| {
                    let ids = l.ids.borrow();
                    ids.get(row.index() as usize).copied()
                });
                if let Some(id) = id {
                    ui.open_thread(id);
                }
            }
        });
    }

    fn list_for(&self, archived: bool) -> Option<Rc<ConversationList>> {
        if archived {
            self.archived.borrow().clone()
        } else {
            Some(self.main_list.clone())
        }
    }

    fn fill_list(self: &Rc<Self>, cl: &ConversationList) {
        let conversations = match self.store.conversations(cl.archived) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "listing conversations failed");
                Vec::new()
            }
        };
        let shown: Vec<Conversation> = conversations
            .into_iter()
            .filter(|c| !self.is_blocked(c))
            .collect();
        while let Some(child) = cl.list.first_child() {
            cl.list.remove(&child);
        }
        let now = local_now();
        for c in &shown {
            let row = self.conversation_row(c, &now);
            cl.list.append(&row);
        }
        *cl.ids.borrow_mut() = shown.iter().map(|c| c.id).collect();
        cl.stack
            .set_visible_child_name(if shown.is_empty() { "empty" } else { "list" });
    }

    fn refresh_list(self: &Rc<Self>) {
        let main = self.main_list.clone();
        self.fill_list(&main);
    }

    fn refresh_archived(self: &Rc<Self>) {
        let a = self.archived.borrow().clone();
        if let Some(a) = a {
            if a.list.root().is_some() {
                self.fill_list(&a);
            }
        }
    }

    fn conversation_row(
        self: &Rc<Self>,
        c: &Conversation,
        now: &chrono::DateTime<chrono::Local>,
    ) -> gtk::ListBoxRow {
        let name = self.conversation_name(c);
        let outer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        outer.add_css_class("conversation-row");
        outer.append(&initials_label(&name));
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name_label = label(&name, &["name"]);
        if c.unread > 0 {
            name_label.add_css_class("unread-name");
        }
        name_label.set_hexpand(true);
        top.append(&name_label);
        if c.pinned {
            let i = gtk::Image::from_icon_name("view-pin-symbolic");
            i.set_tooltip_text(Some("Pinned"));
            i.add_css_class("dim-label");
            top.append(&i);
        }
        if c.muted {
            let i = gtk::Image::from_icon_name("notifications-disabled-symbolic");
            i.set_tooltip_text(Some("Muted"));
            i.add_css_class("dim-label");
            top.append(&i);
        }
        let when = c
            .last
            .as_ref()
            .map(|m| m.at)
            .unwrap_or(c.last_at)
            .max(c.last_at);
        // A conversation with only a draft has no message time yet.
        let time = if when > 0 {
            format::list_time(when, now)
        } else {
            String::new()
        };
        top.append(&label(&time, &["row-meta"]));
        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let (prefix, snip) = format::snippet(c.last.as_ref(), &c.draft, 90);
        if !prefix.is_empty() {
            let p = gtk::Label::new(Some(prefix));
            p.add_css_class(match prefix {
                "Draft: " => "draft-prefix",
                "Not sent: " => "failed-prefix",
                _ => "dim-label",
            });
            bottom.append(&p);
        }
        let s = label(
            &snip,
            if c.unread > 0 {
                &["unread"]
            } else {
                &["dim-label"]
            },
        );
        s.set_hexpand(true);
        bottom.append(&s);
        if c.unread > 0 {
            let badge = gtk::Label::new(Some(&c.unread.min(99).to_string()));
            badge.add_css_class("unread-badge");
            badge.set_valign(gtk::Align::Center);
            bottom.append(&badge);
        }
        text.append(&top);
        text.append(&bottom);
        outer.append(&text);
        let row = gtk::ListBoxRow::builder().child(&outer).build();
        let id = c.id;
        let weak = Rc::downgrade(self);
        let press = gtk::GestureLongPress::new();
        press.connect_pressed(move |g, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            if let Some(ui) = weak.upgrade() {
                ui.row_menu(id);
            }
        });
        row.add_controller(press);
        let weak = Rc::downgrade(self);
        let click = gtk::GestureClick::new();
        click.set_button(3);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(ui) = weak.upgrade() {
                ui.row_menu(id);
            }
        });
        row.add_controller(click);
        row
    }

    /// An action sheet: a dialog that sits at the bottom on a phone.
    fn sheet(self: &Rc<Self>, title: &str, items: Vec<SheetItem>) {
        self.close_sheet();
        let dialog = adw::Dialog::builder()
            .title(title)
            .follows_content_size(true)
            .build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        col.set_margin_start(12);
        col.set_margin_end(12);
        col.set_margin_bottom(12);
        col.set_size_request(300, -1);
        let items: Vec<Rc<dyn Fn()>> = items
            .into_iter()
            .map(|(text, destructive, f)| {
                let b = gtk::Button::with_label(&text);
                b.add_css_class("flat");
                b.add_css_class("sheet-button");
                if destructive {
                    b.add_css_class("destructive-action");
                }
                let f: Rc<dyn Fn()> = Rc::from(f);
                let weak = Rc::downgrade(self);
                let ff = f.clone();
                b.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.close_sheet();
                    }
                    ff();
                });
                col.append(&b);
                f
            })
            .collect();
        drop(items);
        tv.set_content(Some(&col));
        dialog.set_child(Some(&tv));
        dialog.present(Some(&self.window));
        *self.sheet.borrow_mut() = Some(dialog);
    }

    fn close_sheet(&self) {
        // Taken first: closing runs signal handlers that may look again.
        let sheet = self.sheet.borrow_mut().take();
        if let Some(d) = sheet {
            d.close();
        }
    }

    fn row_menu(self: &Rc<Self>, id: i64) {
        let Ok(Some(c)) = self.store.conversation(id) else {
            return;
        };
        let name = self.conversation_name(&c);
        let mut items: Vec<SheetItem> = Vec::new();
        let w = Rc::downgrade(self);
        let act = move |f: fn(&Rc<Ui>, i64)| -> Box<dyn Fn()> {
            let w = w.clone();
            Box::new(move || {
                if let Some(ui) = w.upgrade() {
                    f(&ui, id);
                }
            })
        };
        items.push((
            if c.pinned { "Unpin" } else { "Pin" }.into(),
            false,
            act(|ui, id| ui.toggle(id, |c| c.pinned, MessageStore::set_pinned)),
        ));
        items.push((
            if c.muted {
                "Unmute"
            } else {
                "Mute notifications"
            }
            .into(),
            false,
            act(|ui, id| ui.toggle(id, |c| c.muted, MessageStore::set_muted)),
        ));
        items.push((
            if c.archived { "Unarchive" } else { "Archive" }.into(),
            false,
            act(|ui, id| ui.toggle(id, |c| c.archived, MessageStore::set_archived)),
        ));
        items.push((
            "Block".into(),
            true,
            act(|ui, id| ui.block_conversation(id)),
        ));
        items.push(("Delete".into(), true, act(|ui, id| ui.confirm_delete(id))));
        self.sheet(&name, items);
    }

    fn toggle(self: &Rc<Self>, id: i64, get: fn(&Conversation) -> bool, set: FlagSetter) {
        if let Ok(Some(c)) = self.store.conversation(id) {
            if let Err(e) = set(&self.store, id, !get(&c)) {
                tracing::warn!(error = %e, "updating a conversation failed");
            }
        }
        self.refresh_all();
    }

    fn block_conversation(self: &Rc<Self>, id: i64) {
        let Ok(Some(c)) = self.store.conversation(id) else {
            return;
        };
        let region = self.region();
        let numbers = if c.numbers.is_empty() {
            vec![c.address.clone()]
        } else {
            c.numbers.clone()
        };
        for n in &numbers {
            if let Err(e) = self.blocked.add(n, &region) {
                tracing::warn!(error = %e, "blocking failed");
            }
        }
        self.toast("Blocked: their messages stay hidden and silent");
        if self.thread.borrow().as_ref().map(|t| t.conv) == Some(id) {
            self.nav.pop_to_tag("list");
        }
        self.refresh_all();
    }

    fn confirm_delete(self: &Rc<Self>, id: i64) {
        let dialog = adw::AlertDialog::new(
            Some("Delete this conversation?"),
            Some("Its messages are deleted from this phone."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            if let Err(e) = ui.store.delete_conversation(id) {
                tracing::warn!(error = %e, "deleting a conversation failed");
            }
            if ui.thread.borrow().as_ref().map(|t| t.conv) == Some(id) {
                ui.thread.borrow_mut().take();
                ui.nav.pop_to_tag("list");
            }
            ui.refresh_all();
        });
        dialog.present(Some(&self.window));
    }

    // ---- search --------------------------------------------------------------

    fn refresh_search(self: &Rc<Self>) {
        let text = self.search.text().to_string();
        if text.trim().is_empty() {
            self.list_stack.set_visible_child_name("list");
            return;
        }
        self.list_stack.set_visible_child_name("search");
        while let Some(c) = self.search_list.first_child() {
            self.search_list.remove(&c);
        }
        let hits = self.store.search(&text, 100).unwrap_or_default();
        let now = local_now();
        let mut ids = Vec::new();
        for h in hits {
            let Ok(Some(c)) = self.store.conversation(h.conversation_id) else {
                continue;
            };
            if self.is_blocked(&c) {
                continue;
            }
            let name = self.conversation_name(&c);
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .subtitle(
                    glib::markup_escape_text(&format::snippet(Some(&h.message), "", 120).1)
                        .as_str(),
                )
                .subtitle_lines(2)
                .activatable(true)
                .build();
            row.add_prefix(&initials_label(&name));
            row.add_suffix(&label(
                &format::list_time(h.message.at, &now),
                &["row-meta"],
            ));
            self.search_list.append(&row);
            ids.push(h.conversation_id);
        }
        if ids.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No messages match")
                .activatable(false)
                .build();
            self.search_list.append(&row);
        }
        *self.search_ids.borrow_mut() = ids;
    }

    // ---- threads -------------------------------------------------------------

    /// Open (or start) the conversation with one number.
    fn open_number(self: &Rc<Self>, raw: &str) {
        let raw = raw.trim();
        let address = number::normalize(raw, &self.region());
        if address.is_empty() {
            self.toast("That isn't a number messages can go to");
            return;
        }
        let id = match self.store.find_conversation(&address) {
            Ok(Some(id)) => id,
            _ => match self.store.conversation_for(&address, &[raw]) {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(error = %e, "creating a conversation failed");
                    return;
                }
            },
        };
        self.open_thread(id);
    }

    fn open_thread(self: &Rc<Self>, id: i64) {
        let Ok(Some(c)) = self.store.conversation(id) else {
            return;
        };
        self.save_draft();
        self.close_sheet();
        self.nav.pop_to_tag("list");
        // An archived list stays underneath when that's where we came from.
        let t = self.build_thread(&c);
        *self.thread.borrow_mut() = Some(t.clone());
        self.nav.push(&t.page);
        self.refresh_thread(&t);
        t.view.grab_focus();
    }

    fn build_thread(self: &Rc<Self>, c: &Conversation) -> Rc<Thread> {
        let name = self.conversation_name(c);
        let number = Self::primary_number(c);
        let shown_number = number::display(&number, &self.region());
        let title = adw::WindowTitle::new(
            &name,
            if name == shown_number {
                ""
            } else {
                &shown_number
            },
        );
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let menu = gio::Menu::new();
        menu.append(Some("Call"), Some("thread.call"));
        if self.book.borrow().lookup(&number).is_none() {
            menu.append(Some("Add to contacts"), Some("thread.add-contact"));
        }
        let flags = gio::Menu::new();
        flags.append(Some("Pinned"), Some("thread.pin"));
        flags.append(Some("Muted"), Some("thread.mute"));
        flags.append(Some("Archive"), Some("thread.archive"));
        menu.append_section(None, &flags);
        let danger = gio::Menu::new();
        danger.append(Some("Block"), Some("thread.block"));
        danger.append(Some("Delete conversation"), Some("thread.delete"));
        menu.append_section(None, &danger);
        let call = gtk::Button::from_icon_name("call-start-symbolic");
        call.set_tooltip_text(Some("Call"));
        call.set_action_name(Some("thread.call"));
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .tooltip_text("Conversation")
                .build(),
        );
        header.pack_end(&call);

        let messages = gtk::Box::new(gtk::Orientation::Vertical, 0);
        messages.add_css_class("thread-box");
        messages.set_valign(gtk::Align::End);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&messages)
            .build();

        let buffer = gtk::TextBuffer::new(None);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .hexpand(true)
            .input_hints(
                gtk::InputHints::SPELLCHECK
                    | gtk::InputHints::EMOJI
                    | gtk::InputHints::UPPERCASE_SENTENCES,
            )
            .build();
        let text_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(130)
            .hexpand(true)
            .child(&view)
            .build();
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("text-frame");
        frame.set_hexpand(true);
        frame.append(&text_scroll);
        let counter = gtk::Label::new(None);
        counter.add_css_class("counter");
        counter.set_xalign(1.0);
        counter.set_visible(false);
        frame.append(&counter);
        let attach = gtk::Button::from_icon_name("mail-attachment-symbolic");
        attach.add_css_class("flat");
        attach.add_css_class("circular");
        attach.set_valign(gtk::Align::End);
        attach.set_sensitive(false);
        attach.set_tooltip_text(Some("Pictures and MMS arrive with MMS support"));
        let send = gtk::Button::from_icon_name("go-up-symbolic");
        send.add_css_class("suggested-action");
        send.add_css_class("circular");
        send.add_css_class("send-button");
        send.set_valign(gtk::Align::End);
        send.set_tooltip_text(Some("Send"));
        let composer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        composer.add_css_class("composer");
        composer.append(&attach);
        composer.append(&frame);
        composer.append(&send);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.add_top_bar(&self.banner());
        tv.set_content(Some(&scroller));
        tv.add_bottom_bar(&composer);
        tv.set_bottom_bar_style(adw::ToolbarStyle::Raised);
        let page = adw::NavigationPage::builder()
            .title(&name)
            .tag("thread")
            .child(&tv)
            .build();

        let actions = gio::SimpleActionGroup::new();
        page.insert_action_group("thread", Some(&actions));
        buffer.set_text(&c.draft);

        let t = Rc::new(Thread {
            conv: c.id,
            page,
            title,
            messages,
            scroller,
            buffer,
            view,
            send,
            counter,
            loaded: Cell::new(PAGE),
            more: Cell::new(true),
            stick_bottom: Cell::new(true),
            anchor: Cell::new(None),
            saved_draft: RefCell::new(c.draft.clone()),
            actions,
        });
        self.thread_actions(&t, &number);
        self.wire_thread(&t);
        t
    }

    fn thread_actions(self: &Rc<Self>, t: &Rc<Thread>, number: &str) {
        let id = t.conv;
        let add = |name: &str, f: UiAction| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    f(&ui);
                }
            });
            t.actions.add_action(&a);
        };
        let n = number.to_string();
        add(
            "call",
            Box::new(move |ui| ui.spawn("omaphone", &[&format!("tel:{n}")])),
        );
        let n = number.to_string();
        add(
            "add-contact",
            Box::new(move |ui| ui.spawn("omaphone", &["--contacts", &format!("--add={n}")])),
        );
        add(
            "archive",
            Box::new(move |ui| {
                let _ = ui.store.set_archived(id, true);
                ui.nav.pop_to_tag("list");
                ui.toast("Archived");
                ui.refresh_all();
            }),
        );
        add("block", Box::new(move |ui| ui.block_conversation(id)));
        add("delete", Box::new(move |ui| ui.confirm_delete(id)));
        let flags: [Flag; 2] = [
            ("pin", |c| c.pinned, MessageStore::set_pinned),
            ("mute", |c| c.muted, MessageStore::set_muted),
        ];
        let current = self.store.conversation(id).ok().flatten();
        for (name, get, set) in flags {
            let state = current.as_ref().map(get).unwrap_or(false);
            let a = gio::SimpleAction::new_stateful(name, None, &state.to_variant());
            let weak = Rc::downgrade(self);
            a.connect_activate(move |a, _| {
                let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
                a.set_state(&on.to_variant());
                if let Some(ui) = weak.upgrade() {
                    let _ = set(&ui.store, id, on);
                    ui.refresh_list();
                }
            });
            t.actions.add_action(&a);
        }
    }

    fn wire_thread(self: &Rc<Self>, t: &Rc<Thread>) {
        let adj = t.scroller.vadjustment();
        let wt = Rc::downgrade(t);
        adj.connect_changed(move |adj| {
            let Some(t) = wt.upgrade() else { return };
            if t.stick_bottom.get() {
                adj.set_value(adj.upper() - adj.page_size());
            } else if let Some(d) = t.anchor.take() {
                adj.set_value(adj.upper() - d);
            }
        });
        let wt = Rc::downgrade(t);
        let weak = Rc::downgrade(self);
        adj.connect_value_changed(move |adj| {
            let Some(t) = wt.upgrade() else { return };
            if t.anchor.get().is_some() {
                return;
            }
            t.stick_bottom
                .set(adj.value() >= adj.upper() - adj.page_size() - 8.0);
            if adj.value() <= 0.0 && t.more.get() && adj.upper() > adj.page_size() {
                t.loaded.set(t.loaded.get() + PAGE);
                if let Some(ui) = weak.upgrade() {
                    let t = t.clone();
                    glib::idle_add_local_once(move || ui.refresh_thread(&t));
                }
            }
        });
        let weak = Rc::downgrade(self);
        let wt = Rc::downgrade(t);
        t.buffer.connect_changed(move |_| {
            if let (Some(ui), Some(t)) = (weak.upgrade(), wt.upgrade()) {
                ui.update_send(&t);
            }
        });
        let weak = Rc::downgrade(self);
        let wt = Rc::downgrade(t);
        t.send.connect_clicked(move |_| {
            if let (Some(ui), Some(t)) = (weak.upgrade(), wt.upgrade()) {
                ui.send(&t);
            }
        });
        // Enter sends; Shift+Enter makes a new line.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        let wt = Rc::downgrade(t);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let enter = matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter);
            if enter && !mods.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                if let (Some(ui), Some(t)) = (weak.upgrade(), wt.upgrade()) {
                    ui.send(&t);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        t.view.add_controller(keys);
        // Keep the draft: every few seconds and when the page goes.
        let weak = Rc::downgrade(self);
        let wt = Rc::downgrade(t);
        glib::timeout_add_seconds_local(3, move || match (weak.upgrade(), wt.upgrade()) {
            (Some(ui), Some(t)) if t.page.parent().is_some() => {
                ui.save_thread_draft(&t);
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
        let weak = Rc::downgrade(self);
        let wt = Rc::downgrade(t);
        t.page.connect_hidden(move |_| {
            if let (Some(ui), Some(t)) = (weak.upgrade(), wt.upgrade()) {
                ui.save_thread_draft(&t);
                let current = ui.thread.borrow().as_ref().map(|c| Rc::ptr_eq(c, &t));
                if current == Some(true) && t.page.parent().is_none() {
                    ui.thread.borrow_mut().take();
                }
                ui.refresh_list();
            }
        });
        self.update_send(t);
    }

    fn composer_text(t: &Thread) -> String {
        let (s, e) = t.buffer.bounds();
        t.buffer.text(&s, &e, false).to_string()
    }

    fn update_send(&self, t: &Thread) {
        let text = Self::composer_text(t);
        let has_text = !text.trim().is_empty();
        t.send.set_sensitive(has_text && self.service.get());
        t.send.set_tooltip_text(Some(if self.service.get() {
            "Send"
        } else {
            "The messages service isn't running"
        }));
        match segments::label(&text) {
            Some(l) => {
                t.counter.set_label(&l);
                t.counter.set_visible(true);
            }
            None => t.counter.set_visible(false),
        }
    }

    fn save_thread_draft(&self, t: &Thread) {
        let text = Self::composer_text(t);
        if *t.saved_draft.borrow() != text {
            if let Err(e) = self.store.set_draft(t.conv, &text) {
                tracing::warn!(error = %e, "saving a draft failed");
            }
            *t.saved_draft.borrow_mut() = text;
        }
    }

    fn save_draft(&self) {
        if let Some(t) = self.thread.borrow().as_ref() {
            self.save_thread_draft(t);
        }
    }

    fn send(self: &Rc<Self>, t: &Rc<Thread>) {
        let text = Self::composer_text(t).trim_end().to_string();
        if text.trim().is_empty() {
            return;
        }
        let Some(proxy) = self.proxy() else {
            self.toast("The messages service isn't running");
            return;
        };
        t.buffer.set_text("");
        *t.saved_draft.borrow_mut() = String::new();
        let _ = self.store.set_draft(t.conv, "");
        t.stick_bottom.set(true);
        let conv = t.conv;
        let weak = Rc::downgrade(self);
        let wt = Rc::downgrade(t);
        glib::MainContext::default().spawn_local(async move {
            let result = proxy.send_to_conversation(conv, &text).await;
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(id) => tracing::info!(message = id, "message handed to the service"),
                Err(e) => {
                    tracing::warn!(error = %e, "sending failed");
                    ui.toast("Couldn't send the message");
                    if let Some(t) = wt.upgrade() {
                        if Self::composer_text(&t).is_empty() {
                            t.buffer.set_text(&text);
                        }
                    }
                }
            }
            ui.refresh_all();
        });
    }

    fn mark_read(self: &Rc<Self>, conv: i64) {
        match self.store.mark_read(conv) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "marking read failed"),
        }
        if let Some(proxy) = self.proxy() {
            glib::MainContext::default().spawn_local(async move {
                if let Err(e) = proxy.mark_read(conv).await {
                    tracing::debug!(error = %e, "MarkRead failed");
                }
            });
        }
        self.refresh_list();
    }

    fn refresh_thread(self: &Rc<Self>, t: &Rc<Thread>) {
        let Ok(Some(c)) = self.store.conversation(t.conv) else {
            return;
        };
        let name = self.conversation_name(&c);
        t.title.set_title(&name);
        t.page.set_title(&name);
        let msgs = self
            .store
            .messages(t.conv, t.loaded.get(), None)
            .unwrap_or_default();
        t.more.set(msgs.len() >= t.loaded.get());
        let adj = t.scroller.vadjustment();
        if !t.stick_bottom.get() {
            t.anchor.set(Some(adj.upper() - adj.value()));
        }
        while let Some(child) = t.messages.first_child() {
            t.messages.remove(&child);
        }
        if msgs.is_empty() {
            let hint = gtk::Label::new(Some("No messages yet. Say hello."));
            hint.add_css_class("dim-label");
            hint.set_margin_bottom(24);
            t.messages.append(&hint);
        }
        let now = local_now();
        let tz = chrono::Local;
        for (i, m) in msgs.iter().enumerate() {
            let prev = i.checked_sub(1).map(|p| &msgs[p]);
            let next = msgs.get(i + 1);
            if prev.is_none_or(|p| format::new_day(p.at, m.at, &tz)) {
                let sep = gtk::Label::new(Some(&format::day_separator(m.at, &now)));
                sep.add_css_class("day-separator");
                t.messages.append(&sep);
            }
            let last_of_group =
                next.is_none_or(|n| n.direction != m.direction || n.at - m.at > 10 * 60_000);
            let show_meta = last_of_group
                || (m.direction == Direction::Out
                    && m.state != MessageState::Sent
                    && m.state != MessageState::Delivered);
            t.messages.append(&self.bubble(m, show_meta, &tz));
        }
        if t.page.parent().is_some() && self.window.is_active() || self.script_running.get() {
            self.mark_read(t.conv);
        }
    }

    fn bubble(self: &Rc<Self>, m: &Message, meta: bool, tz: &chrono::Local) -> gtk::Widget {
        let out = m.direction == Direction::Out;
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.add_css_class("bubble-row");
        col.set_halign(if out {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let text = gtk::Label::builder()
            .label(&m.body)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .max_width_chars(26)
            .build();
        text.add_css_class("bubble");
        text.add_css_class(if out { "outgoing" } else { "incoming" });
        if out && m.state == MessageState::Failed {
            text.add_css_class("failed");
        }
        text.set_halign(if out {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        col.append(&text);
        if meta {
            let l = gtk::Label::new(Some(&format::delivery(m, tz)));
            l.add_css_class("bubble-meta");
            if out && m.state == MessageState::Failed {
                l.add_css_class("error");
            }
            l.set_halign(if out {
                gtk::Align::End
            } else {
                gtk::Align::Start
            });
            l.set_margin_start(6);
            l.set_margin_end(6);
            col.append(&l);
        }
        if out && m.state == MessageState::Failed {
            let id = m.id;
            let weak = Rc::downgrade(self);
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.failed_menu(id);
                }
            });
            col.add_controller(click);
        }
        col.upcast()
    }

    fn failed_menu(self: &Rc<Self>, id: i64) {
        let w = Rc::downgrade(self);
        let w2 = w.clone();
        self.sheet(
            "Message not sent",
            vec![
                (
                    "Retry".into(),
                    false,
                    Box::new(move || {
                        if let Some(ui) = w.upgrade() {
                            ui.retry(id);
                        }
                    }),
                ),
                (
                    "Delete".into(),
                    true,
                    Box::new(move || {
                        if let Some(ui) = w2.upgrade() {
                            let _ = ui.store.delete_message(id);
                            ui.refresh_all();
                        }
                    }),
                ),
            ],
        );
    }

    fn retry(self: &Rc<Self>, id: i64) {
        let Some(proxy) = self.proxy() else {
            self.toast("The messages service isn't running");
            return;
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let r = proxy.retry(id).await;
            let Some(ui) = weak.upgrade() else { return };
            if let Err(e) = r {
                tracing::warn!(error = %e, "retry failed");
                ui.toast("Couldn't send the message");
            }
            ui.refresh_all();
        });
    }

    fn spawn(&self, program: &str, args: &[&str]) {
        match std::process::Command::new(program).args(args).spawn() {
            Ok(mut child) => {
                // Reap it in the background so it never lingers as a zombie.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, program, "could not start");
                self.toast("The Phone app isn't installed");
            }
        }
    }

    // ---- compose ---------------------------------------------------------------

    fn open_compose(self: &Rc<Self>) {
        self.save_draft();
        self.close_sheet();
        self.nav.pop_to_tag("list");
        let to = gtk::Entry::builder()
            .placeholder_text("Name or number")
            .hexpand(true)
            .input_purpose(gtk::InputPurpose::FreeForm)
            .build();
        let to_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        to_row.set_margin_start(12);
        to_row.set_margin_end(12);
        to_row.set_margin_top(6);
        to_row.set_margin_bottom(6);
        let to_label = gtk::Label::new(Some("To"));
        to_label.add_css_class("heading");
        to_row.append(&to_label);
        to_row.append(&to);
        let suggestions = gtk::ListBox::new();
        suggestions.set_selection_mode(gtk::SelectionMode::None);
        suggestions.set_show_separators(true);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&suggestions)
            .build();
        let header = adw::HeaderBar::new();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.add_top_bar(&to_row);
        tv.set_content(Some(&scroll));
        let page = adw::NavigationPage::builder()
            .title("New message")
            .tag("compose")
            .child(&tv)
            .build();
        let c = Rc::new(Compose {
            page,
            to,
            suggestions,
            numbers: RefCell::new(Vec::new()),
            body: RefCell::new(None),
        });
        let weak = Rc::downgrade(self);
        c.to.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_suggestions();
            }
        });
        let weak = Rc::downgrade(self);
        c.to.connect_activate(move |e| {
            let Some(ui) = weak.upgrade() else { return };
            let first = ui
                .compose
                .borrow()
                .as_ref()
                .and_then(|c| c.numbers.borrow().first().cloned());
            match first {
                Some(n) => ui.compose_to(&n),
                None if !e.text().trim().is_empty() => ui.compose_to(&e.text()),
                None => {}
            }
        });
        let weak = Rc::downgrade(self);
        c.suggestions.connect_row_activated(move |_, row| {
            let Some(ui) = weak.upgrade() else { return };
            let n = ui
                .compose
                .borrow()
                .as_ref()
                .and_then(|c| c.numbers.borrow().get(row.index() as usize).cloned());
            if let Some(n) = n {
                ui.compose_to(&n);
            }
        });
        *self.compose.borrow_mut() = Some(c.clone());
        self.nav.push(&c.page);
        self.refresh_suggestions();
        c.to.grab_focus();
    }

    fn refresh_suggestions(self: &Rc<Self>) {
        let Some(c) = self.compose.borrow().clone() else {
            return;
        };
        while let Some(child) = c.suggestions.first_child() {
            c.suggestions.remove(&child);
        }
        let text = c.to.text().to_string();
        let region = self.region();
        let mut numbers = Vec::new();
        let dial = number::sanitize_dial_string(&text);
        let looks_like_number = dial.chars().filter(char::is_ascii_digit).count() >= 3
            && !text.chars().any(char::is_alphabetic);
        if looks_like_number {
            let row = adw::ActionRow::builder()
                .title(format!("Send to {}", number::display(&dial, &region)))
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("mail-send-symbolic"));
            c.suggestions.append(&row);
            numbers.push(dial);
        }
        let book = self.book.borrow();
        for contact in book.search(&text).into_iter().take(40) {
            for p in &contact.phones {
                let name = contact.display_name();
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&name).as_str())
                    .subtitle(format!(
                        "{} · {}",
                        p.kind.label(),
                        number::display(&p.number, &region)
                    ))
                    .activatable(true)
                    .build();
                row.add_prefix(&initials_label(&name));
                c.suggestions.append(&row);
                numbers.push(p.number.clone());
            }
        }
        *c.numbers.borrow_mut() = numbers;
    }

    fn compose_to(self: &Rc<Self>, raw: &str) {
        let body = self
            .compose
            .borrow_mut()
            .take()
            .and_then(|c| c.body.borrow_mut().take());
        self.open_number(raw);
        if let (Some(b), Some(t)) = (body, self.thread.borrow().as_ref()) {
            t.buffer.set_text(&b);
        }
    }

    // ---- the window menu: archived, blocked, settings, export, import ------

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<Ui>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    f(&ui);
                }
            });
            self.window.add_action(&a);
        };
        add("archived", Ui::open_archived);
        add("blocked", Ui::open_blocked);
        add("settings", Ui::open_settings);
        add("export", Ui::export);
        add("import", Ui::import);
    }

    fn open_archived(self: &Rc<Self>) {
        self.nav.pop_to_tag("list");
        let cl = Rc::new(ConversationList::new(true));
        *self.archived.borrow_mut() = Some(cl.clone());
        self.connect_list(&cl);
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&cl.stack));
        let page = adw::NavigationPage::builder()
            .title("Archived")
            .tag("archived")
            .child(&tv)
            .build();
        self.nav.push(&page);
        self.fill_list(&cl);
    }

    fn open_blocked(self: &Rc<Self>) {
        self.nav.pop_to_tag("list");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let entry = adw::EntryRow::builder()
            .title("Block a number")
            .show_apply_button(true)
            .input_purpose(gtk::InputPurpose::Phone)
            .build();
        let weak = Rc::downgrade(self);
        entry.connect_apply(move |e| {
            let Some(ui) = weak.upgrade() else { return };
            let text = e.text().to_string();
            if !text.trim().is_empty() {
                if let Err(err) = ui.blocked.add(&text, &ui.region()) {
                    tracing::warn!(error = %err, "blocking failed");
                }
                e.set_text("");
                ui.refresh_blocked();
                ui.refresh_all();
            }
        });
        let add_box = gtk::ListBox::new();
        add_box.add_css_class("boxed-list");
        add_box.set_selection_mode(gtk::SelectionMode::None);
        add_box.append(&entry);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 18);
        col.set_margin_start(12);
        col.set_margin_end(12);
        col.set_margin_top(12);
        col.set_margin_bottom(12);
        let note = gtk::Label::new(Some(
            "Blocked numbers can't call you, and their messages are kept out of sight with no notification. The list is shared with Phone.",
        ));
        note.set_wrap(true);
        note.set_xalign(0.0);
        note.add_css_class("dim-label");
        col.append(&note);
        col.append(&add_box);
        col.append(&list);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&adw::Clamp::builder().child(&col).build())
            .build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&scroll));
        let page = adw::NavigationPage::builder()
            .title("Blocked numbers")
            .tag("blocked")
            .child(&tv)
            .build();
        *self.blocked_list.borrow_mut() = Some(list);
        self.nav.push(&page);
        self.refresh_blocked();
    }

    fn refresh_blocked(self: &Rc<Self>) {
        let Some(list) = self.blocked_list.borrow().clone() else {
            return;
        };
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        let region = self.region();
        let entries = self.blocked.list().unwrap_or_default();
        if entries.is_empty() {
            list.append(&adw::ActionRow::builder().title("Nobody is blocked").build());
        }
        for b in entries {
            let name = self.book.borrow().name_for(&b.raw);
            let shown = number::display(&b.raw, &region);
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(name.as_deref().unwrap_or(&shown)).as_str())
                .subtitle(if name.is_some() {
                    shown.clone()
                } else {
                    String::new()
                })
                .build();
            let unblock = gtk::Button::with_label("Unblock");
            unblock.set_valign(gtk::Align::Center);
            let weak = Rc::downgrade(self);
            let key = b.number.clone();
            unblock.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                if let Err(e) = ui.blocked.remove(&key, &ui.region()) {
                    tracing::warn!(error = %e, "unblocking failed");
                }
                ui.refresh_blocked();
                ui.refresh_all();
            });
            row.add_suffix(&unblock);
            list.append(&row);
        }
    }

    fn open_settings(self: &Rc<Self>) {
        let settings = Rc::new(RefCell::new(Settings::load()));
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Settings");
        let page = adw::PreferencesPage::new();
        let save = {
            let settings = settings.clone();
            let weak = Rc::downgrade(self);
            move || {
                if let Err(e) = settings.borrow().save() {
                    tracing::warn!(error = %e, "saving settings failed");
                    if let Some(ui) = weak.upgrade() {
                        ui.toast("Couldn't save the settings");
                    }
                }
            }
        };
        let save = Rc::new(save);
        let switch = |title: &str,
                      subtitle: &str,
                      get: fn(&Settings) -> bool,
                      set: fn(&mut Settings, bool)| {
            let row = adw::SwitchRow::builder()
                .title(title)
                .subtitle(subtitle)
                .active(get(&settings.borrow()))
                .build();
            let settings = settings.clone();
            let save = save.clone();
            row.connect_active_notify(move |r| {
                set(&mut settings.borrow_mut(), r.is_active());
                save();
            });
            row
        };
        let notifications = adw::PreferencesGroup::builder()
            .title("Notifications")
            .build();
        notifications.add(&switch(
            "New message notifications",
            "",
            |s| s.message_notifications,
            |s, v| s.message_notifications = v,
        ));
        notifications.add(&switch(
            "Sender and text on the lock screen",
            "Off: the lock screen only says “New message”",
            |s| s.lockscreen_details,
            |s, v| s.lockscreen_details = v,
        ));
        page.add(&notifications);

        let sending = adw::PreferencesGroup::builder()
            .title("Sending")
            .description(
                "The region is two letters (US, GB) for numbers written without a country code; \
                 leave it blank to take it from the language.",
            )
            .build();
        sending.add(&switch(
            "Delivery reports",
            "Ask the network to confirm each message arrived",
            |s| s.delivery_reports,
            |s, v| s.delivery_reports = v,
        ));
        let region = adw::EntryRow::builder()
            .title("Region for local numbers")
            .text(settings.borrow().region.clone())
            .show_apply_button(true)
            .build();
        {
            let settings = settings.clone();
            let save = save.clone();
            let weak = Rc::downgrade(self);
            region.connect_apply(move |r| {
                let text = r.text().trim().to_ascii_uppercase();
                if !(text.is_empty()
                    || text.len() == 2 && text.chars().all(|c| c.is_ascii_alphabetic()))
                {
                    if let Some(ui) = weak.upgrade() {
                        ui.toast("A region is two letters, like US or GB");
                    }
                    return;
                }
                settings.borrow_mut().region = text;
                save();
                if let Some(ui) = weak.upgrade() {
                    *ui.region.borrow_mut() = settings.borrow().region(number::default_region);
                    ui.refresh_all();
                }
            });
        }
        sending.add(&region);
        page.add(&sending);

        let mms = adw::PreferencesGroup::builder()
            .title("MMS")
            .description("Pictures, group messages and MMS arrive in a later update.")
            .build();
        for title in ["MMSC", "MMS proxy", "MMS access point (APN)"] {
            let row = adw::EntryRow::builder()
                .title(title)
                .sensitive(false)
                .build();
            mms.add(&row);
        }
        page.add(&mms);
        dialog.add(&page);
        let weak = Rc::downgrade(self);
        dialog.connect_closed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.settings.borrow_mut().take();
            }
        });
        dialog.present(Some(&self.window));
        *self.settings.borrow_mut() = Some(dialog);
    }

    fn export(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Export messages")
            .initial_name("messages.json")
            .build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let Some(ui) = weak.upgrade() else { return };
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            match ui
                .store
                .export_json()
                .map_err(anyhow::Error::from)
                .and_then(|j| write_private(&path, j.as_bytes()).map_err(anyhow::Error::from))
            {
                Ok(()) => ui.toast("Messages exported"),
                Err(e) => {
                    tracing::warn!(error = %e, "export failed");
                    ui.toast("Export failed");
                }
            }
        });
    }

    fn import(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Messages export (JSON)"));
        filter.add_suffix("json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import messages")
            .filters(&filters)
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let Some(ui) = weak.upgrade() else { return };
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            match std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|t| ui.store.import_json(&t).map_err(anyhow::Error::from))
            {
                Ok(n) => {
                    ui.toast(&format!("{n} messages imported"));
                    ui.refresh_all();
                }
                Err(e) => {
                    tracing::warn!(error = %e, "import failed");
                    ui.toast("That file isn't a messages export");
                }
            }
        });
    }

    // ---- the script runner -----------------------------------------------------

    fn run_script(self: &Rc<Self>, steps: Vec<Step>) {
        self.script_running.set(true);
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            for step in steps {
                tracing::info!(?step, "script");
                match step {
                    Step::Wait(d) => glib::timeout_future(d).await,
                    Step::Open(n) => {
                        let id = ui.main_list.ids.borrow().get(n).copied();
                        match id {
                            Some(id) => ui.open_thread(id),
                            None => tracing::warn!(row = n, "no such row"),
                        }
                    }
                    Step::OpenArchived => ui.open_archived(),
                    Step::Compose => ui.open_compose(),
                    Step::Type(text) => {
                        let compose = ui.compose.borrow().clone();
                        let on_compose = compose
                            .as_ref()
                            .is_some_and(|c| ui.nav.visible_page().as_ref() == Some(&c.page));
                        if let (true, Some(c)) = (on_compose, compose) {
                            c.to.set_text(&text);
                        } else if let Some(t) = ui.thread.borrow().as_ref() {
                            t.buffer.set_text(&text);
                        }
                    }
                    Step::Search(text) => {
                        ui.nav.pop_to_tag("list");
                        ui.search.set_text(&text);
                        ui.refresh_search();
                    }
                    Step::Settings => ui.open_settings(),
                    Step::CloseSettings => {
                        let dialog = ui.settings.borrow_mut().take();
                        if let Some(d) = dialog {
                            d.close();
                        }
                    }
                    Step::Blocked => ui.open_blocked(),
                    Step::Back => {
                        let dialog = ui.settings.borrow_mut().take();
                        if ui.sheet.borrow().is_some() {
                            ui.close_sheet();
                        } else if let Some(d) = dialog {
                            d.close();
                        } else {
                            ui.nav.pop();
                        }
                    }
                    Step::Menu(n) => {
                        let id = ui.main_list.ids.borrow().get(n).copied();
                        if let Some(id) = id {
                            ui.row_menu(id);
                        }
                    }
                    Step::CloseMenu => ui.close_sheet(),
                    Step::Shot(path) => {
                        if let Err(e) = ui.shot_after_paint(path).await {
                            tracing::warn!(error = %e, "screenshot failed");
                        }
                    }
                    Step::Quit => {
                        ui.save_draft();
                        ui.app.quit();
                        return;
                    }
                }
            }
        });
    }

    /// Wait for the next painted frame (at most 500 ms), then render.
    async fn shot_after_paint(self: &Rc<Self>, path: PathBuf) -> anyhow::Result<()> {
        let Some(clock) = self.window.frame_clock() else {
            return self.render_png(&path);
        };
        let (tx, rx) = async_channel::bounded::<anyhow::Result<()>>(2);
        let done = Rc::new(Cell::new(false));
        let (ui, p, t, d) = (Rc::downgrade(self), path.clone(), tx.clone(), done.clone());
        let id = clock.connect_after_paint(move |_| {
            if d.replace(true) {
                return;
            }
            if let Some(ui) = ui.upgrade() {
                let _ = t.try_send(ui.render_png(&p));
            }
        });
        self.window.queue_draw();
        let ui = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if done.replace(true) {
                return;
            }
            if let Some(ui) = ui.upgrade() {
                let _ = tx.try_send(ui.render_png(&path));
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
        if w <= 0.0 || h <= 0.0 {
            anyhow::bail!("window not laid out");
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

impl ConversationList {
    fn new(archived: bool) -> ConversationList {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_show_separators(true);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("mail-unread-symbolic")
            .title(if archived {
                "No archived conversations"
            } else {
                "No conversations yet"
            })
            .description(if archived {
                "Archive a conversation to tuck it away here; a new message brings it back."
            } else {
                "Start one with the + button."
            })
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroll, Some("list"));
        stack.add_named(&empty, Some("empty"));
        ConversationList {
            list,
            ids: RefCell::new(Vec::new()),
            stack,
            archived,
        }
    }
}

/// Write a file only its owner can read: exports hold every message.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
