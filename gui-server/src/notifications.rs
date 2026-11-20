use std::{
    collections::HashMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use zbus::{
    MatchRule, Message,
    blocking::{Connection, MessageIterator, connection::Builder},
    message::Type,
    zvariant::Value,
};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

pub(super) enum Event {
    Action(String),
    Token(String),
    Closed(u32),
}

pub(super) struct Notice {
    connection: Connection,
    events: mpsc::Receiver<Event>,
    listener: Option<thread::JoinHandle<()>>,
    id: u32,
}

impl Notice {
    pub(super) fn post(
        icon: &str,
        title: &str,
        body: &str,
        actions: &[&str],
        ttl: i32,
        urgency: Option<u8>,
    ) -> zbus::Result<Self> {
        let connection = Builder::session()?
            .method_timeout(Duration::from_secs(5))
            .build()?;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(SERVICE)?
            .interface(SERVICE)?
            .path(PATH)?
            .build();
        let mut messages = MessageIterator::for_match_rule(rule, &connection, Some(32))?;
        let mut hints = HashMap::from([("suppress-sound", Value::from(true))]);
        if let Some(urgency) = urgency {
            hints.insert("urgency", Value::from(urgency));
        }
        let reply = connection.call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE),
            "Notify",
            &(
                "Docked Browser",
                0_u32,
                icon,
                title,
                body,
                actions,
                hints,
                ttl,
            ),
        )?;
        let id: u32 = reply.body().deserialize()?;
        if id == 0 {
            return Err(zbus::Error::Failure("invalid notification ID".into()));
        }
        let (send, events) = mpsc::channel();
        let listener = thread::spawn(move || {
            for message in (&mut messages).flatten() {
                if let Some(event) = event_for(&message, id) {
                    if send.send(event).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self {
            connection,
            events,
            listener: Some(listener),
            id,
        })
    }

    pub(super) fn next(&self, timeout: Duration) -> Result<Event, mpsc::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }
}

impl Drop for Notice {
    fn drop(&mut self) {
        let _ = self.connection.call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE),
            "CloseNotification",
            &(self.id,),
        );
        let _ = self.connection.clone().close();
        if let Some(handle) = self.listener.take() {
            let _ = handle.join();
        }
    }
}

fn event_for(message: &Message, id: u32) -> Option<Event> {
    match message.header().member()?.as_str() {
        "NotificationClosed" => {
            let (got, reason): (u32, u32) = message.body().deserialize().ok()?;
            (got == id).then_some(Event::Closed(reason))
        }
        name @ ("ActionInvoked" | "ActivationToken") => {
            let (got, text): (u32, String) = message.body().deserialize().ok()?;
            (got == id).then_some(if name == "ActionInvoked" {
                Event::Action(text)
            } else {
                Event::Token(text)
            })
        }
        _ => None,
    }
}

pub(super) fn pause_cancelled(profile: &str, mut focused: impl FnMut() -> bool) -> bool {
    let notice = Notice::post(
        "web-browser",
        &format!("Pausing “{profile}” in 20s"),
        "Click this notification to keep it awake.",
        &["default", "Keep awake", "keep", "Keep awake"],
        20_000,
        Some(1),
    )
    .ok();
    let deadline = Instant::now() + Duration::from_secs(20 + u64::from(notice.is_some()));
    loop {
        if focused() {
            return true;
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(400));
        if wait.is_zero() {
            return false;
        }
        if let Some(notice) = &notice {
            match notice.next(wait) {
                Ok(Event::Action(_) | Event::Token(_) | Event::Closed(2)) => return true,
                Err(mpsc::RecvTimeoutError::Disconnected) => thread::sleep(wait),
                _ => {}
            }
        } else {
            thread::sleep(wait);
        }
    }
}

use super::{
    CHECK_INTERVAL_S, chrome_check, habit, now_f64, paths, py_float, sleep_poll, sleep_sensors,
    sleep_state, tray, truthy,
};
use serde_json::{Value as Json, json};
use std::sync::atomic::{AtomicBool, Ordering};

static UPDATE_PENDING: AtomicBool = AtomicBool::new(false);

fn run_action(action: &str, profile: Option<&str>, token: &str) {
    let mut cmd = tray::command(&paths().script);
    cmd.arg(action);
    if let Some(profile) = profile {
        cmd.arg(profile);
    }
    let token = token.trim();
    if !token.is_empty() && token.len() <= 512 && !token.chars().any(char::is_control) {
        cmd.env("DOCKED_ACTIVATION_TOKEN", token);
    }
    tray::launch(cmd, true);
}

fn suggest(profile: &str, date: &str, slot: &str) {
    let notice = match Notice::post(
        "web-browser",
        &format!("Open “{profile}”?"),
        "You usually use this profile around this time.",
        &["default", "Open", "open", "Open"],
        300_000,
        None,
    ) {
        Ok(notice) => notice,
        Err(error) => {
            let _ = habit::clear_offer(date, slot);
            sleep_poll::log(format!("habit notification failed: {error}"));
            return;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(300);
    while let Some(timeout) = deadline.checked_duration_since(Instant::now()) {
        match notice.next(timeout) {
            Ok(Event::Token(token)) if !token.trim().is_empty() => {
                run_action("activate", Some(profile), &token);
                break;
            }
            Ok(Event::Action(action)) if matches!(action.as_str(), "default" | "open") => {
                run_action("activate", Some(profile), "");
                break;
            }
            Ok(Event::Closed(_)) | Err(_) => break,
            _ => {}
        }
    }
}

fn chrome_update_notice() {
    let info = chrome_check(false);
    if !truthy(info.get("update_available"))
        || now_f64()
            - info
                .get("last_notified_at")
                .and_then(py_float)
                .unwrap_or_default()
            < CHECK_INTERVAL_S
    {
        return;
    }
    let latest = info["latest_version"].as_str().unwrap_or_default();
    let installed = info["installed_version"].as_str().unwrap_or_default();
    let title = if latest.is_empty() {
        "New Chrome update available".into()
    } else {
        format!("Chrome {latest} is available")
    };
    let suffix = if installed.is_empty() {
        String::new()
    } else {
        format!(" (installed: {installed})")
    };
    let body = format!("A new version of Google Chrome is available{suffix}.");
    let Ok(notice) = Notice::post(
        "software-update-available",
        &title,
        &body,
        &["default", "Open", "update", "Update"],
        0,
        None,
    ) else {
        return;
    };
    let _ = super::with_chrome_update_state(|path, mut state| {
        state.insert("last_notified_at".into(), json!(now_f64()));
        sleep_state::persist(path, &Json::Object(state))
    });
    loop {
        match notice.next(Duration::from_secs(CHECK_INTERVAL_S as u64)) {
            Ok(Event::Action(action)) if action == "default" => {
                tray::open_web();
                break;
            }
            Ok(Event::Action(action)) if action == "update" => {
                run_action("build", None, "");
                break;
            }
            Ok(Event::Closed(_)) | Err(_) => break,
            _ => {}
        }
    }
}

pub(super) fn background(snapshot: &Json) {
    let exact = snapshot["focused"].as_str().unwrap_or_default();
    if !exact.is_empty() {
        let _ = habit::log_use(exact);
    }
    let focused = if exact.is_empty() {
        sleep_sensors::focused_class_profile()
    } else {
        exact.into()
    };
    match habit::offer(&focused) {
        Ok(Some((profile, date, slot))) => {
            sleep_poll::log(format!("suggest {profile} {slot}"));
            thread::spawn(move || suggest(&profile, &date, &slot));
        }
        Ok(None) => {}
        Err(error) => sleep_poll::log(format!("habit suggestion: {error}")),
    }
    if !UPDATE_PENDING.swap(true, Ordering::AcqRel) {
        thread::spawn(|| {
            chrome_update_notice();
            UPDATE_PENDING.store(false, Ordering::Release);
        });
    }
}
