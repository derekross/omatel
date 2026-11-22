// SPDX-License-Identifier: GPL-3.0-or-later
//! Test doubles for omatel, never shipped in the package:
//!
//! - [`demo`]: fictional contacts, calls and messages for screenshots;
//! - [`bus`]: a private dbus-daemon for tests (never the machine's buses);
//! - [`mm`]: a scripted ModemManager (calls, SMS, delivery reports);
//! - [`mmsd`]: a scripted mmsd-tng (MMS in and out, group MMS);
//! - [`notifications`]: an org.freedesktop.Notifications server that
//!   records what it is sent and can answer with actions and replies.

pub mod bus;
pub mod demo;
pub mod mm;
pub mod mmsd;
pub mod notifications;
