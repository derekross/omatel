// SPDX-License-Identifier: GPL-3.0-or-later
//! Test doubles for omatel, never shipped in the package:
//!
//! - [`demo`]: fictional contacts, calls and messages for screenshots;
//! - [`bus`]: a private dbus-daemon for tests (never the machine's buses);
//! - [`mm`]: a scripted ModemManager (calls, SMS, delivery reports);
//! - [`notifications`]: an org.freedesktop.Notifications server that
//!   records what it is sent and can answer with actions and replies.

pub mod demo;
