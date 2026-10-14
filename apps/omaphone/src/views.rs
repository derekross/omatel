// SPDX-License-Identifier: GPL-3.0-or-later
//! The four tabs: Favourites, Recents, Contacts and Keypad.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use omatel_contacts::number;

use crate::app::{build_dialpad, Ui};
use crate::util;

/// A list page: a scrolled, clamped column.
pub fn scrolled_column(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(600)
        .child(child)
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build()
}

pub fn avatar(name: Option<&str>, size: i32) -> adw::Avatar {
    adw::Avatar::new(size, name, name.is_some())
}

pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .width_request(48)
        .height_request(48)
        .build()
}

pub fn clear_box(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

pub fn clear_list(l: &gtk::ListBox) {
    l.remove_all();
}

impl Ui {
    pub fn build_tabs(self: &Rc<Self>) {
        // Favourites
        let fav = scrolled_column(&self.favourites);
        self.views
            .add_titled_with_icon(&fav, Some("favourites"), "Favourites", "starred-symbolic");
        // Recents
        let rec = scrolled_column(&self.recents);
        self.views.add_titled_with_icon(
            &rec,
            Some("recents"),
            "Recents",
            "document-open-recent-symbolic",
        );
        // Contacts
        let contacts = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let search_clamp = adw::Clamp::builder()
            .maximum_size(600)
            .child(&self.contacts_search)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .build();
        contacts.append(&search_clamp);
        self.contacts_stack
            .add_named(&scrolled_column(&self.contacts_list), Some("list"));
        self.contacts_stack
            .add_named(&self.contacts_empty, Some("empty"));
        self.contacts_stack.set_vexpand(true);
        contacts.append(&self.contacts_stack);
        self.views.add_titled_with_icon(
            &contacts,
            Some("contacts"),
            "Contacts",
            "avatar-default-symbolic",
        );
        let ui = self.clone();
        self.contacts_search
            .connect_search_changed(move |_| ui.refresh_contacts());
        // Keypad
        let keypad = self.build_keypad();
        self.views.add_titled_with_icon(
            &keypad,
            Some("keypad"),
            "Keypad",
            "input-dialpad-symbolic",
        );
    }

    fn build_keypad(self: &Rc<Self>) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
        page.set_margin_start(12);
        page.set_margin_end(12);
        page.set_margin_top(8);
        page.set_margin_bottom(12);

        let warn = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        warn.add_css_class("emergency-banner");
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        icon.set_valign(gtk::Align::Start);
        let text = gtk::Label::builder()
            .label(omatel_api::EMERGENCY_WARNING)
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["caption"])
            .build();
        warn.append(&icon);
        warn.append(&text);
        page.append(&warn);

        let sugg = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&self.suggestions)
            .vexpand(true)
            .propagate_natural_height(false)
            .build();
        page.append(&sugg);
        page.append(&self.display);

        let ui = self.clone();
        let pad = build_dialpad(Rc::new(move |d| ui.press_key(d)), true);
        page.append(&pad);

        let bottom = gtk::CenterBox::new();
        bottom.set_margin_top(8);
        bottom.set_center_widget(Some(&self.call_button));
        let back = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Delete")
            .css_classes(["flat", "circular"])
            .width_request(64)
            .height_request(64)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        let back_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        back_wrap.set_width_request(88);
        back_wrap.set_halign(gtk::Align::Center);
        back_wrap.append(&back);
        bottom.set_end_widget(Some(&back_wrap));
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        left.set_width_request(88);
        bottom.set_start_widget(Some(&left));
        page.append(&bottom);

        let long = Rc::new(std::cell::Cell::new(false));
        let g = gtk::GestureLongPress::new();
        g.set_propagation_phase(gtk::PropagationPhase::Capture);
        let ui = self.clone();
        let l = long.clone();
        g.connect_pressed(move |_, _, _| {
            l.set(true);
            ui.set_dial_text("");
        });
        back.add_controller(g);
        let ui = self.clone();
        back.connect_clicked(move |_| {
            if long.replace(false) {
                return;
            }
            let mut t = ui.dial_text.borrow().clone();
            t.pop();
            ui.set_dial_text(&t);
        });
        let ui = self.clone();
        self.call_button.connect_clicked(move |_| {
            let t = ui.dial_text.borrow().clone();
            ui.dial(&t);
        });
        page
    }

    pub fn press_key(self: &Rc<Self>, d: char) {
        let mut t = self.dial_text.borrow().clone();
        if t.len() < 64 {
            t.push(d);
        }
        self.set_dial_text(&t);
    }

    pub fn set_dial_text(self: &Rc<Self>, text: &str) {
        *self.dial_text.borrow_mut() = text.to_string();
        let (main, post) = number::dial_part(text);
        // Format only a number that looks complete (ten digits, or an
        // international one), so "55501" isn't shown as "+1 55501".
        let digits = main.chars().filter(char::is_ascii_digit).count();
        let complete = digits >= 10 || (main.starts_with('+') && digits >= 8);
        let shown = if post.is_empty() && complete && number::to_e164(main, &self.region).is_some()
        {
            number::display(main, &self.region)
        } else {
            text.to_string()
        };
        self.display.set_text(&shown);
        self.display.set_css_classes(if shown.chars().count() > 16 {
            &["dial-display", "dial-display-small"]
        } else {
            &["dial-display"]
        });
        self.update_suggestions();
    }

    pub fn update_suggestions(self: &Rc<Self>) {
        clear_list(&self.suggestions);
        let digits = self.dial_text.borrow().clone();
        let found = self.book.borrow().smart_dial(&digits, 4);
        self.suggestions.set_visible(!found.is_empty());
        for s in found {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&s.name).as_str())
                .subtitle(format!(
                    "{} · {}",
                    s.kind.label(),
                    number::display(&s.number, &self.region)
                ))
                .activatable(true)
                .build();
            row.add_prefix(&avatar(Some(&s.name), 36));
            let call = icon_button("call-start-symbolic", "Call");
            let ui = self.clone();
            let n = s.number.clone();
            call.connect_clicked(move |_| ui.dial(&n));
            row.add_suffix(&call);
            let ui = self.clone();
            let n = s.number.clone();
            row.connect_activated(move |_| ui.set_dial_text(&number::sanitize_dial_string(&n)));
            self.suggestions.append(&row);
        }
    }

    pub fn update_missed_badge(&self) {
        let missed = self
            .log
            .as_ref()
            .and_then(|l| l.unseen_missed().ok())
            .unwrap_or(0);
        if let Some(child) = self.views.child_by_name("recents") {
            let page = self.views.page(&child);
            page.set_badge_number(missed as u32);
            page.set_needs_attention(missed > 0);
        }
    }

    pub fn refresh_favourites(self: &Rc<Self>) {
        clear_box(&self.favourites);
        let book = self.book.borrow();
        let mut starred = book.starred();
        if let Some(log) = &self.log {
            if let Ok(order) = log.favourites_order() {
                starred
                    .sort_by_key(|c| order.iter().position(|u| *u == c.uid).unwrap_or(usize::MAX));
            }
        }
        if !starred.is_empty() {
            self.favourites.append(
                &gtk::Label::builder()
                    .label("Favourites")
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .build(),
            );
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .max_children_per_line(3)
                .min_children_per_line(3)
                .homogeneous(true)
                .row_spacing(6)
                .column_spacing(6)
                .build();
            for c in starred {
                let v = gtk::Box::new(gtk::Orientation::Vertical, 6);
                v.append(&avatar(Some(&c.display_name()), 64));
                v.append(
                    &gtk::Label::builder()
                        .label(c.display_name())
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .max_width_chars(10)
                        .build(),
                );
                let tile = gtk::Button::builder()
                    .child(&v)
                    .css_classes(["flat", "favourite-tile"])
                    .tooltip_text("Call")
                    .build();
                let ui = self.clone();
                let first = c.phones.first().map(|p| p.number.clone());
                let uid = c.uid.clone();
                tile.connect_clicked(move |_| match &first {
                    Some(n) => ui.dial(n),
                    None => ui.show_contact(&uid),
                });
                // A long press opens the contact instead.
                let g = gtk::GestureLongPress::new();
                let ui = self.clone();
                let uid = c.uid.clone();
                g.connect_pressed(move |g, _, _| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    ui.show_contact(&uid);
                });
                tile.add_controller(g);
                flow.append(&tile);
            }
            self.favourites.append(&flow);
        }
        let frequent = self
            .log
            .as_ref()
            .and_then(|l| l.frequent(6).ok())
            .unwrap_or_default();
        if !frequent.is_empty() {
            self.favourites.append(
                &gtk::Label::builder()
                    .label("Frequent")
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .build(),
            );
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            for (_, raw, _) in frequent {
                let name = book.name_for(&raw);
                let title = name
                    .clone()
                    .unwrap_or_else(|| number::display(&raw, &self.region));
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&title).as_str())
                    .activatable(true)
                    .build();
                if name.is_some() {
                    row.set_subtitle(&number::display(&raw, &self.region));
                }
                row.add_prefix(&avatar(name.as_deref(), 40));
                let call = icon_button("call-start-symbolic", "Call");
                let ui = self.clone();
                let n = raw.clone();
                call.connect_clicked(move |_| ui.dial(&n));
                row.add_suffix(&call);
                let ui = self.clone();
                row.connect_activated(move |_| ui.show_details(&raw));
                list.append(&row);
            }
            self.favourites.append(&list);
        }
        if self.favourites.first_child().is_none() {
            self.favourites.append(
                &adw::StatusPage::builder()
                    .icon_name("starred-symbolic")
                    .title("No favourites yet")
                    .description("Star a contact to call them with one tap.")
                    .vexpand(true)
                    .build(),
            );
        }
    }

    pub fn refresh_recents(self: &Rc<Self>) {
        clear_box(&self.recents);
        let calls = self
            .log
            .as_ref()
            .and_then(|l| l.recent(200).ok())
            .unwrap_or_default();
        let mut numbers = Vec::new();
        if calls.is_empty() {
            self.recents.append(
                &adw::StatusPage::builder()
                    .icon_name("document-open-recent-symbolic")
                    .title("No calls yet")
                    .description("Calls you make and receive show here.")
                    .vexpand(true)
                    .build(),
            );
        } else {
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            let now = omatel_store::now_ms();
            let book = self.book.borrow();
            for c in calls {
                let name = book.name_for(&c.raw_number);
                let title = name
                    .clone()
                    .unwrap_or_else(|| number::display(&c.raw_number, &self.region));
                let (icon, warn, word) = util::outcome_look(c.direction, c.outcome);
                let mut sub = format!("{word} · {}", util::relative_time(c.started_at, now));
                if c.duration > 0 {
                    sub.push_str(&format!(" · {}", util::format_duration(c.duration)));
                }
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&title).as_str())
                    .subtitle(sub)
                    .activatable(true)
                    .build();
                let img = gtk::Image::from_icon_name(icon);
                if warn {
                    img.add_css_class("missed");
                    row.add_css_class("missed-row");
                }
                row.add_prefix(&img);
                let call = icon_button("call-start-symbolic", "Call back");
                let ui = self.clone();
                let n = c.raw_number.clone();
                call.connect_clicked(move |_| ui.dial(&n));
                row.add_suffix(&call);
                let ui = self.clone();
                let n = c.raw_number.clone();
                row.connect_activated(move |_| ui.show_details(&n));
                numbers.push(c.raw_number.clone());
                list.append(&row);
            }
            self.recents.append(&list);
        }
        *self.recent_numbers.borrow_mut() = numbers;
        self.update_missed_badge();
    }

    pub fn refresh_contacts(self: &Rc<Self>) {
        clear_list(&self.contacts_list);
        let q = self.contacts_search.text().to_string();
        let book = self.book.borrow();
        let found = book.search(&q);
        let mut uids = Vec::new();
        let mut last_letter: Option<char> = None;
        for c in &found {
            let name = c.display_name();
            let letter = name
                .chars()
                .next()
                .map(|ch| ch.to_uppercase().next().unwrap_or(ch))
                .unwrap_or('#');
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .activatable(true)
                .build();
            if q.is_empty() && last_letter != Some(letter) {
                last_letter = Some(letter);
                row.set_widget_name(&format!("letter-{letter}"));
            }
            if let Some(p) = c.phones.first() {
                row.set_subtitle(&number::display(&p.number, &self.region));
            }
            row.add_prefix(&avatar(Some(&name), 40));
            if c.starred() {
                row.add_suffix(&gtk::Image::from_icon_name("starred-symbolic"));
            }
            let ui = self.clone();
            let uid = c.uid.clone();
            row.connect_activated(move |_| ui.show_contact(&uid));
            uids.push(c.uid.clone());
            self.contacts_list.append(&row);
        }
        // Letter headers between groups when not searching.
        if q.is_empty() {
            self.contacts_list.set_header_func(|row, _before| {
                let n = row.widget_name();
                match n.strip_prefix("letter-") {
                    Some(l) => row.set_header(Some(
                        &gtk::Label::builder()
                            .label(l)
                            .xalign(0.0)
                            .margin_start(12)
                            .margin_top(6)
                            .margin_bottom(6)
                            .css_classes(["heading", "dim-label"])
                            .build(),
                    )),
                    None => row.set_header(None::<&gtk::Widget>),
                }
            });
        } else {
            self.contacts_list.unset_header_func();
        }
        self.contacts_stack
            .set_visible_child_name(if found.is_empty() { "empty" } else { "list" });
        if found.is_empty() && !q.is_empty() {
            self.contacts_empty.set_title("No matches");
            self.contacts_empty.set_description(None);
        } else {
            self.contacts_empty.set_title("No contacts");
            self.contacts_empty.set_description(Some(
                "Add one with the button above, or import a .vcf file from the menu.",
            ));
        }
        *self.contact_uids.borrow_mut() = uids;
    }
}
