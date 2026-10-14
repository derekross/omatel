// SPDX-License-Identifier: GPL-3.0-or-later
//! Pages pushed over the tabs: a contact, the contact editor, the call
//! details for a number, and the block list.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use omatel_contacts::{number, Contact, Email, Phone, PhoneKind};

use crate::app::Ui;
use crate::util;
use crate::views::{avatar, icon_button, scrolled_column};

fn page(title: &str, content: &gtk::Box, header: &adw::HeaderBar) -> adw::NavigationPage {
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(header);
    tv.set_content(Some(&scrolled_column(content)));
    adw::NavigationPage::new(&tv, title)
}

fn heading_block(name: Option<&str>, title: &str, subtitle: Option<&str>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.set_margin_top(12);
    b.set_margin_bottom(12);
    b.append(&avatar(name, 96));
    b.append(
        &gtk::Label::builder()
            .label(title)
            .css_classes(["title-1"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build(),
    );
    if let Some(s) = subtitle {
        b.append(
            &gtk::Label::builder()
                .label(s)
                .css_classes(["dim-label"])
                .build(),
        );
    }
    b
}

/// A row of big labelled round buttons (Call, Message, ...).
/// An icon, a label and what the button does.
type Action<'a> = (&'a str, &'a str, Rc<dyn Fn()>);

fn action_bar(items: &[Action]) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .homogeneous(true)
        .margin_bottom(12)
        .build();
    for (icon, label, f) in items {
        let b = gtk::Button::builder()
            .icon_name(*icon)
            .css_classes(["circular", "incall-toggle"])
            .tooltip_text(*label)
            .halign(gtk::Align::Center)
            .build();
        let f = f.clone();
        b.connect_clicked(move |_| f());
        let v = gtk::Box::new(gtk::Orientation::Vertical, 4);
        v.append(&b);
        v.append(
            &gtk::Label::builder()
                .label(*label)
                .css_classes(["incall-label"])
                .build(),
        );
        row.append(&v);
    }
    row
}

/// A full-width button for the end of a page (block, delete).
fn wide_button(label: &str, destructive: bool) -> gtk::Button {
    let b = gtk::Button::builder()
        .label(label)
        .height_request(48)
        .css_classes(["pill"])
        .build();
    if destructive {
        b.add_css_class("destructive-action");
    }
    b
}

impl Ui {
    pub fn show_contact(self: &Rc<Self>, uid: &str) {
        let Some(c) = self.book.borrow().get(uid).cloned() else {
            return;
        };
        *self.open_contact.borrow_mut() = Some(uid.to_string());
        let name = c.display_name();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.append(&heading_block(Some(&name), &name, None));

        if let Some(first) = c.phones.first() {
            let (ui1, n1) = (self.clone(), first.number.clone());
            let (ui2, n2) = (self.clone(), first.number.clone());
            content.append(&action_bar(&[
                (
                    "call-start-symbolic",
                    "Call",
                    Rc::new(move || ui1.dial(&n1)),
                ),
                (
                    "chat-message-new-symbolic",
                    "Message",
                    Rc::new(move || ui2.message(&n2)),
                ),
            ]));
        }

        if !c.phones.is_empty() {
            let g = adw::PreferencesGroup::builder().title("Phone").build();
            for p in &c.phones {
                let row = adw::ActionRow::builder()
                    .title(number::display(&p.number, &self.region))
                    .subtitle(p.kind.label())
                    .activatable(true)
                    .build();
                let msg = icon_button("chat-message-new-symbolic", "Message");
                let ui = self.clone();
                let n = p.number.clone();
                msg.connect_clicked(move |_| ui.message(&n));
                let call = icon_button("call-start-symbolic", "Call");
                let ui = self.clone();
                let n = p.number.clone();
                call.connect_clicked(move |_| ui.dial(&n));
                row.add_suffix(&msg);
                row.add_suffix(&call);
                let ui = self.clone();
                let n = p.number.clone();
                row.connect_activated(move |_| ui.show_details(&n));
                g.add(&row);
            }
            content.append(&g);
        }
        if !c.emails.is_empty() {
            let g = adw::PreferencesGroup::builder().title("Email").build();
            for e in &c.emails {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&e.address).as_str())
                    .subtitle(if e.kind.is_empty() { "Email" } else { &e.kind })
                    .build();
                g.add(&row);
            }
            content.append(&g);
        }
        if !c.note.is_empty() {
            let g = adw::PreferencesGroup::builder().title("Note").build();
            g.add(
                &gtk::Label::builder()
                    .label(&c.note)
                    .wrap(true)
                    .xalign(0.0)
                    .selectable(true)
                    .build(),
            );
            content.append(&g);
        }
        let blocked = c.phones.iter().any(|p| self.is_blocked(&p.number));
        let block = wide_button(
            if blocked {
                "Unblock numbers"
            } else {
                "Block numbers"
            },
            false,
        );
        let ui = self.clone();
        let numbers: Vec<String> = c.phones.iter().map(|p| p.number.clone()).collect();
        let uid_s = uid.to_string();
        block.connect_clicked(move |_| {
            for n in &numbers {
                ui.set_blocked(n, !blocked);
            }
            ui.nav.pop();
            ui.show_contact(&uid_s);
        });
        let delete = wide_button("Delete contact", true);
        let ui = self.clone();
        let uid_s = uid.to_string();
        delete.connect_clicked(move |_| ui.confirm_delete(&uid_s));
        content.append(&block);
        content.append(&delete);

        let header = adw::HeaderBar::new();
        let star = gtk::ToggleButton::builder()
            .icon_name(if c.starred() {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            })
            .active(c.starred())
            .tooltip_text("Favourite")
            .build();
        let ui = self.clone();
        let uid_s = uid.to_string();
        star.connect_toggled(move |b| {
            b.set_icon_name(if b.is_active() {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
            let c = ui.book.borrow().get(&uid_s).cloned();
            if let Some(mut c) = c {
                c.set_starred(b.is_active());
                let r = ui.book.borrow_mut().save(&c);
                if let Err(e) = r {
                    tracing::warn!(error = %e, "cannot save contact");
                    ui.toast("Couldn't save the contact");
                }
                ui.refresh_contacts_everywhere();
            }
        });
        let edit = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Edit")
            .build();
        let ui = self.clone();
        let uid_s = uid.to_string();
        edit.connect_clicked(move |_| ui.edit_contact(Some(uid_s.clone()), None));
        header.pack_end(&edit);
        header.pack_end(&star);
        let p = page(&name, &content, &header);
        self.nav.push(&p);
    }

    fn confirm_delete(self: &Rc<Self>, uid: &str) {
        let dialog = adw::AlertDialog::new(
            Some("Delete this contact?"),
            Some("Its card is removed from the address book."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let ui = self.clone();
        let uid = uid.to_string();
        dialog.connect_response(None, move |_, r| {
            if r != "delete" {
                return;
            }
            let res = ui.book.borrow_mut().delete(&uid);
            match res {
                Ok(()) => {
                    ui.nav.pop_to_tag("root");
                    ui.refresh_contacts_everywhere();
                    ui.toast("Contact deleted");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot delete contact");
                    ui.toast("Couldn't delete the contact");
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    /// The editor: for a contact (`uid`), or a new one, optionally with a
    /// number filled in.
    pub fn edit_contact(self: &Rc<Self>, uid: Option<String>, with_number: Option<String>) {
        let existing = uid
            .as_ref()
            .and_then(|u| self.book.borrow().get(u).cloned());
        let mut contact = existing.clone().unwrap_or_default();
        if let Some(n) = with_number {
            contact.phones.push(Phone {
                number: n,
                kind: PhoneKind::Mobile,
            });
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let name_group = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder().title("Name").build();
        name.set_text(&contact.name);
        name_group.add(&name);
        let star = adw::SwitchRow::builder()
            .title("Favourite")
            .active(contact.starred())
            .build();
        name_group.add(&star);
        content.append(&name_group);

        // Numbers
        let phones_group = adw::PreferencesGroup::builder().title("Phone").build();
        let phone_rows: Rc<RefCell<Vec<(adw::EntryRow, gtk::DropDown)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let labels: Vec<&str> = PhoneKind::ALL.iter().map(|k| k.label()).collect();
        let add_phone = {
            let group = phones_group.clone();
            let rows = phone_rows.clone();
            let labels = labels.clone();
            Rc::new(move |p: Option<&Phone>| {
                let row = adw::EntryRow::builder()
                    .title("Number")
                    .input_purpose(gtk::InputPurpose::Phone)
                    .build();
                if let Some(p) = p {
                    row.set_text(&p.number);
                }
                let kind = gtk::DropDown::from_strings(&labels);
                kind.set_valign(gtk::Align::Center);
                kind.add_css_class("flat");
                let idx = p
                    .and_then(|p| PhoneKind::ALL.iter().position(|k| *k == p.kind))
                    .unwrap_or(0);
                kind.set_selected(idx as u32);
                let remove = icon_button("list-remove-symbolic", "Remove");
                row.add_suffix(&kind);
                row.add_suffix(&remove);
                let g = group.clone();
                let rws = rows.clone();
                let r2 = row.clone();
                remove.connect_clicked(move |_| {
                    g.remove(&r2);
                    rws.borrow_mut().retain(|(r, _)| r != &r2);
                });
                group.add(&row);
                rows.borrow_mut().push((row, kind));
            })
        };
        for p in &contact.phones {
            add_phone(Some(p));
        }
        if contact.phones.is_empty() {
            add_phone(None);
        }
        content.append(&phones_group);
        let add_btn = gtk::Button::builder()
            .label("Add number")
            .css_classes(["pill"])
            .halign(gtk::Align::Center)
            .build();
        let ap = add_phone.clone();
        add_btn.connect_clicked(move |_| ap(None));
        content.append(&add_btn);

        // Emails
        let email_group = adw::PreferencesGroup::builder().title("Email").build();
        let email_rows: Rc<RefCell<Vec<adw::EntryRow>>> = Rc::new(RefCell::new(Vec::new()));
        let add_email = {
            let group = email_group.clone();
            let rows = email_rows.clone();
            Rc::new(move |e: Option<&Email>| {
                let row = adw::EntryRow::builder()
                    .title("Email")
                    .input_purpose(gtk::InputPurpose::Email)
                    .build();
                if let Some(e) = e {
                    row.set_text(&e.address);
                }
                group.add(&row);
                rows.borrow_mut().push(row);
            })
        };
        for e in &contact.emails {
            add_email(Some(e));
        }
        add_email(None);
        content.append(&email_group);

        let note_group = adw::PreferencesGroup::builder().title("Note").build();
        let note = adw::EntryRow::builder().title("Note").build();
        note.set_text(&contact.note);
        note_group.add(&note);
        content.append(&note_group);

        let header = adw::HeaderBar::new();
        let save = gtk::Button::builder()
            .label("Save")
            .css_classes(["suggested-action"])
            .build();
        header.pack_end(&save);
        let title = if existing.is_some() {
            "Edit contact"
        } else {
            "New contact"
        };
        let p = page(title, &content, &header);
        let ui = self.clone();
        save.connect_clicked(move |_| {
            let mut c: Contact = existing.clone().unwrap_or_else(|| Contact::new(""));
            c.set_name(&name.text());
            c.phones = phone_rows
                .borrow()
                .iter()
                .filter_map(|(r, k)| {
                    let n = r.text().trim().to_string();
                    (!n.is_empty()).then(|| Phone {
                        number: n,
                        kind: PhoneKind::ALL
                            .get(k.selected() as usize)
                            .copied()
                            .unwrap_or(PhoneKind::Other),
                    })
                })
                .collect();
            let old_kinds: Vec<(String, String)> = c
                .emails
                .iter()
                .map(|e| (e.address.clone(), e.kind.clone()))
                .collect();
            c.emails = email_rows
                .borrow()
                .iter()
                .filter_map(|r| {
                    let a = r.text().trim().to_string();
                    (!a.is_empty()).then(|| Email {
                        kind: old_kinds
                            .iter()
                            .find(|(x, _)| *x == a)
                            .map(|(_, k)| k.clone())
                            .unwrap_or_default(),
                        address: a,
                    })
                })
                .collect();
            c.note = note.text().to_string();
            c.set_starred(star.is_active());
            if c.name.is_empty() && c.phones.is_empty() {
                ui.toast("Add a name or a number");
                return;
            }
            let r = ui.book.borrow_mut().save(&c);
            match r {
                Ok(()) => {
                    tracing::info!("contact saved");
                    ui.nav.pop();
                    ui.refresh_contacts_everywhere();
                    if let Some(u) = ui.open_contact.borrow().clone() {
                        if u == c.uid {
                            // Re-open the contact page with the new data.
                            ui.nav.pop();
                            ui.show_contact(&u);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot save contact");
                    ui.toast("Couldn't save the contact");
                }
            }
        });
        self.nav.push(&p);
    }

    pub fn is_blocked(&self, raw: &str) -> bool {
        self.blocked
            .as_ref()
            .and_then(|b| b.is_blocked(raw, &self.region).ok())
            .unwrap_or(false)
    }

    pub fn set_blocked(&self, raw: &str, on: bool) {
        let Some(b) = &self.blocked else {
            self.toast("The block list isn't available");
            return;
        };
        let r = if on {
            b.add(raw, &self.region).map(|_| ())
        } else {
            b.remove(raw, &self.region).map(|_| ())
        };
        match r {
            Ok(()) => self.toast(if on {
                "Number blocked"
            } else {
                "Number unblocked"
            }),
            Err(e) => {
                tracing::warn!(error = %e, "block list write failed");
                self.toast("Couldn't change the block list");
            }
        }
    }

    /// Calls with one number, and what to do with it.
    pub fn show_details(self: &Rc<Self>, raw: &str) {
        let key = number::normalize(raw, &self.region);
        let name = self.book.borrow().name_for(raw);
        let contact = self.book.borrow().lookup(raw);
        let shown = number::display(raw, &self.region);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.append(&heading_block(
            name.as_deref(),
            name.as_deref().unwrap_or(&shown),
            name.is_some().then_some(shown.as_str()),
        ));
        let blocked = self.is_blocked(raw);
        let (u1, n1) = (self.clone(), raw.to_string());
        let (u2, n2) = (self.clone(), raw.to_string());
        let (u3, n3) = (self.clone(), raw.to_string());
        let (u4, n4) = (self.clone(), raw.to_string());
        let contact_action: Action = match &contact {
            Some(m) => {
                let uid = m.uid.clone();
                (
                    "avatar-default-symbolic",
                    "Contact",
                    Rc::new(move || u3.show_contact(&uid)),
                )
            }
            None => (
                "contact-new-symbolic",
                "Add contact",
                Rc::new(move || u3.edit_contact(None, Some(n3.clone()))),
            ),
        };
        content.append(&action_bar(&[
            ("call-start-symbolic", "Call", Rc::new(move || u1.dial(&n1))),
            (
                "chat-message-new-symbolic",
                "Message",
                Rc::new(move || u2.message(&n2)),
            ),
            contact_action,
            (
                "action-unavailable-symbolic",
                if blocked { "Unblock" } else { "Block" },
                Rc::new(move || {
                    u4.set_blocked(&n4, !blocked);
                    u4.nav.pop();
                    u4.show_details(&n4);
                }),
            ),
        ]));

        let calls = self
            .log
            .as_ref()
            .and_then(|l| l.for_number(&key).ok())
            .unwrap_or_default();
        if !calls.is_empty() {
            let g = adw::PreferencesGroup::builder().title("Calls").build();
            for c in &calls {
                let (icon, warn, word) = util::outcome_look(c.direction, c.outcome);
                let mut sub = util::full_time(c.started_at);
                if c.duration > 0 {
                    sub.push_str(&format!(" · {}", util::format_duration(c.duration)));
                }
                let row = adw::ActionRow::builder().title(word).subtitle(sub).build();
                let img = gtk::Image::from_icon_name(icon);
                if warn {
                    img.add_css_class("missed");
                }
                row.add_prefix(&img);
                g.add(&row);
            }
            content.append(&g);
            let del = wide_button("Delete from call history", true);
            let ui = self.clone();
            del.connect_clicked(move |_| {
                if let Some(log) = &ui.log {
                    let _ = log.delete_number(&key);
                }
                ui.nav.pop();
                ui.refresh_recents();
                ui.refresh_favourites();
            });
            content.append(&del);
        }
        let header = adw::HeaderBar::new();
        let p = page("Call details", &content, &header);
        self.nav.push(&p);
    }

    pub fn show_blocked(self: &Rc<Self>) {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let list = self
            .blocked
            .as_ref()
            .and_then(|b| b.list().ok())
            .unwrap_or_default();
        if list.is_empty() {
            content.append(
                &adw::StatusPage::builder()
                    .icon_name("action-unavailable-symbolic")
                    .title("No blocked numbers")
                    .description(
                        "Calls from blocked numbers are hung up at once, and their messages are hidden.",
                    )
                    .build(),
            );
        } else {
            let g = adw::PreferencesGroup::builder()
                .title("Blocked numbers")
                .description("Calls are hung up at once; messages are kept but hidden.")
                .build();
            for b in list {
                let title = self
                    .book
                    .borrow()
                    .name_for(&b.raw)
                    .unwrap_or_else(|| number::display(&b.raw, &self.region));
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&title).as_str())
                    .build();
                let un = gtk::Button::builder()
                    .label("Unblock")
                    .valign(gtk::Align::Center)
                    .build();
                let ui = self.clone();
                let raw = b.number.clone();
                un.connect_clicked(move |_| {
                    ui.set_blocked(&raw, false);
                    ui.nav.pop();
                    ui.show_blocked();
                });
                row.add_suffix(&un);
                g.add(&row);
            }
            content.append(&g);
        }
        let p = page("Blocked numbers", &content, &adw::HeaderBar::new());
        self.nav.push(&p);
    }
}
