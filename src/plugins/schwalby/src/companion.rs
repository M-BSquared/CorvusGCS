//! The companion computer (`paintCompanion`, `startProbing`, `powerCompanion`,
//! `openCompanionDialog` of `schwalby.js`): pinged from this computer while
//! the plugin is open, with a popup to set it and a power menu.

use serde_json::{json, Value};

use super::model::{
    companion_entry, companion_state, host_error, new_connection_body, new_connection_error, normalize_companion,
    session_name, Companion, NewConn, State, DEFAULT_INTERVAL_S, DEFAULT_TIMEOUT_S, NEW_CONNECTION, PLUGIN_ID,
};
use super::{Rep, Schwalby};
use corvus_api::net::bg::Pending;
use corvus_api::plugin::ssh::{reply_of, ssh_needs};
use corvus_api::plugin::Host;

/// The ping loop: one probe at a time, `interval_s` apart.
#[derive(Default)]
pub(crate) struct Probe {
    pub started_at: f64,
    pub last_ok_at: Option<f64>,
    pub rtt: Option<f64>,
    pub error: String,
    /// The state last noted on the console.
    shown: Option<State>,
    next_at: f64,
    inflight: Option<(Rep, f64)>,
    active: bool,
}

/// Reboot or shut the companion down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Power {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub note: &'static str,
    pub done: &'static str,
}

pub(crate) const POWER_ACTIONS: [Power; 2] = [
    Power {
        id: "reboot",
        label: "Reboot",
        icon: "rotate-cw",
        note: "Restart the companion computer",
        done: "Reboot sent",
    },
    Power {
        id: "shutdown",
        label: "Shutdown",
        icon: "power",
        note: "Power the companion computer off",
        done: "Shutdown sent",
    },
];

enum PowerStage {
    Wait(Rep),
    Setup(u64),
}

pub(crate) struct PowerRun {
    action: Power,
    stage: PowerStage,
    retried: bool,
}

/// The popup of the companion strip.
pub(crate) struct CompDialog {
    /// The segment: `false` pings an address, `true` goes over SSH.
    pub ssh: bool,
    pub name: String,
    pub host: String,
    pub interval: u32,
    pub timeout: u32,
    /// The connection select's value.
    pub choice: String,
    pub conn: NewConn,
    pub saving: Option<Rep>,
    pub message: Option<(String, &'static str)>,
}

impl CompDialog {
    pub fn new(companion: Option<&Companion>, connections: &[Value]) -> CompDialog {
        let draft = companion.cloned().unwrap_or(Companion {
            name: String::new(),
            host: String::new(),
            interval_s: DEFAULT_INTERVAL_S,
            timeout_s: DEFAULT_TIMEOUT_S,
            connection: String::new(),
        });
        let names: Vec<&str> = connections
            .iter()
            .filter_map(|c| c.get("name").and_then(Value::as_str))
            .collect();
        // a name this computer lacks stays on the list, as an option
        let choice = if !draft.connection.is_empty() {
            draft.connection.clone()
        } else if let Some(first) = names.first() {
            (*first).to_string()
        } else {
            NEW_CONNECTION.to_string()
        };
        CompDialog {
            ssh: !draft.connection.is_empty(),
            name: draft.name,
            host: draft.host,
            interval: draft.interval_s,
            timeout: draft.timeout_s,
            choice,
            conn: NewConn::blank(),
            saving: None,
            message: None,
        }
    }

    pub fn picked_new(&self) -> bool {
        self.ssh && self.choice == NEW_CONNECTION
    }

    /// The address that is pinged: typed, or the picked connection's host.
    pub fn ping_host(&self, connections: &[Value]) -> String {
        if !self.ssh {
            return self.host.trim().to_string();
        }
        if self.picked_new() {
            return self.conn.host.trim().to_string();
        }
        connections
            .iter()
            .find(|c| c.get("name").and_then(Value::as_str) == Some(self.choice.as_str()))
            .and_then(|c| c.get("host").and_then(Value::as_str))
            .map_or_else(|| self.host.trim().to_string(), |h| h.trim().to_string())
    }

    /// Why Save is not offered yet, or empty.
    pub fn problem(&self, connections: &[Value]) -> String {
        let mut p = host_error(&self.ping_host(connections)).to_string();
        if self.picked_new() {
            let taken: Vec<String> = connections
                .iter()
                .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
                .collect();
            let e = new_connection_error(&self.conn, &taken);
            if !e.is_empty() {
                p = e;
            }
        }
        p
    }

    /// The companion this draft would save under `connection`.
    pub fn companion(&self, connections: &[Value], connection: &str) -> Option<Companion> {
        normalize_companion(&json!({
            "name": if connection.is_empty() { self.name.as_str() } else { connection },
            "host": self.ping_host(connections),
            "interval_s": self.interval,
            "timeout_s": self.timeout,
            "connection": connection,
        }))
    }
}

impl Schwalby {
    /// Start over: forget the old answers and probe the saved companion.
    pub(crate) fn start_probing(&mut self, now: f64) {
        self.probe = Probe {
            started_at: now,
            next_at: now,
            active: self.companion.is_some(),
            ..Default::default()
        };
    }

    pub(crate) fn stop_probing(&mut self) {
        self.probe.active = false;
        self.probe.inflight = None;
    }

    pub(crate) fn companion_now(&self, now: f64) -> State {
        let Some(c) = &self.companion else {
            return State::Unset;
        };
        companion_state(
            &c.host,
            self.probe.started_at,
            self.probe.last_ok_at,
            !self.probe.error.is_empty(),
            now,
            f64::from(c.timeout_s),
        )
    }

    /// One step of the ping loop and the console line on a change.
    pub(crate) fn run_probe(&mut self, h: &mut Host) {
        let Some(c) = self.companion.clone() else {
            return;
        };
        if let Some((rep, began)) = self.probe.inflight.take() {
            match rep.take() {
                None => self.probe.inflight = Some((rep, began)),
                Some(r) => {
                    match r {
                        Ok((s, v)) if s < 400 => {
                            if v.get("ok") == Some(&json!(true)) {
                                self.probe.error.clear();
                                if v.get("reachable") == Some(&json!(true)) {
                                    self.probe.last_ok_at = Some(h.now);
                                    self.probe.rtt = v.get("rtt_ms").and_then(Value::as_f64);
                                }
                            } else {
                                self.probe.error = v
                                    .get("error")
                                    .and_then(Value::as_str)
                                    .filter(|e| !e.is_empty())
                                    .unwrap_or("The ping failed.")
                                    .to_string();
                            }
                        }
                        Ok((s, v)) => {
                            self.probe.error = v
                                .get("error")
                                .and_then(Value::as_str)
                                .filter(|e| !e.is_empty())
                                .map_or(format!("Request failed ({s})"), str::to_string);
                        }
                        Err(e) => {
                            self.probe.error = if e.is_empty() {
                                "Could not reach the backend".into()
                            } else {
                                e
                            };
                        }
                    }
                    self.probe.next_at = began + f64::from(c.interval_s);
                }
            }
        }
        if self.probe.active && self.probe.inflight.is_none() && h.now >= self.probe.next_at {
            let wait = super::model::probe_wait_ms(c.interval_s);
            self.probe.inflight = Some((
                h.post_any("/api/local/ping", json!({"host": c.host, "wait_ms": wait})),
                h.now,
            ));
        }
        let state = self.companion_now(h.now);
        if self.probe.shown != Some(state) {
            let was = self.probe.shown.replace(state);
            let who = format!("{} ({})", c.display_name(), c.host);
            if was.is_some() || state != State::Checking {
                match state {
                    State::Online => h.console(&format!("{PLUGIN_ID}: {who} is online"), "success"),
                    State::Offline => h.console(
                        &format!("{PLUGIN_ID}: {who} is offline, no reply for {} s", c.timeout_s),
                        "error",
                    ),
                    _ => {}
                }
            }
        }
        // Online and checking both end on their own when nothing answers.
        let due = match state {
            State::Online => self.probe.last_ok_at.map(|t| t + f64::from(c.timeout_s)),
            State::Checking => Some(self.probe.started_at + f64::from(c.timeout_s)),
            _ => None,
        };
        let next = (self.probe.next_at - h.now).max(0.05);
        let mut wake = if self.probe.inflight.is_some() { 1.0 } else { next };
        if let Some(d) = due {
            wake = wake.min((d - h.now).max(0.0) + 0.05);
        }
        h.ctx
            .request_repaint_after(std::time::Duration::from_secs_f64(wake.max(0.05)));
    }

    /// Asks before a reboot or a shutdown.
    pub(crate) fn power_asked(&mut self, p: Power) {
        let Some(c) = &self.companion else {
            return;
        };
        if c.connection.is_empty() {
            return;
        }
        let mut lines = vec!["Everything running on it stops.".to_string()];
        if p.id == "shutdown" {
            lines.push("It has to be switched on by hand again.".into());
        }
        self.confirm = Some(super::Confirm {
            title: format!("{} {}?", p.label, c.display_name()),
            lines,
            label: p.label.into(),
            icon: p.icon,
            danger: true,
            then: super::After::Power(p),
        });
    }

    pub(crate) fn start_power(&mut self, p: Power, retried: bool, h: &mut Host) {
        let Some(c) = self.companion.clone().filter(|c| !c.connection.is_empty()) else {
            return;
        };
        self.status = Some((format!("{}: {} ...", c.display_name(), p.label.to_lowercase()), "info"));
        self.power = Some(PowerRun {
            action: p,
            stage: PowerStage::Wait(h.post_any("/api/ssh/power", json!({"name": c.connection, "action": p.id}))),
            retried,
        });
    }

    pub(crate) fn run_power(&mut self, h: &mut Host) {
        let Some(mut run) = self.power.take() else {
            return;
        };
        let name = self
            .companion
            .as_ref()
            .map_or("Companion computer".to_string(), |c| c.display_name().to_string());
        match run.stage {
            PowerStage::Wait(rep) => {
                let Some(r) = rep.take() else {
                    run.stage = PowerStage::Wait(rep);
                    self.power = Some(run);
                    return;
                };
                let res = reply_of(r);
                if !run.retried && res.get("needs").is_some() {
                    if let Some(need) = ssh_needs(&res) {
                        self.tag += 1;
                        h.ssh_setup(need, self.tag);
                        run.stage = PowerStage::Setup(self.tag);
                        self.power = Some(run);
                        return;
                    }
                }
                if res.get("ok") != Some(&json!(true)) {
                    let said = |k: &str| res.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
                    let text = {
                        let e = said("error");
                        let raw = if e.is_empty() { said("stderr") } else { e };
                        raw.split('\n').next().unwrap_or("").trim().to_string()
                    };
                    let text = if text.is_empty() {
                        "The command failed.".to_string()
                    } else {
                        text
                    };
                    self.status = Some((format!("{name}: {text}"), "err"));
                    h.console(
                        &format!(
                            "{PLUGIN_ID}: {name}: {} failed, {text}",
                            run.action.label.to_lowercase()
                        ),
                        "error",
                    );
                } else {
                    self.status = Some((format!("{name}: {}", run.action.done), "ok"));
                    h.console(
                        &format!("{PLUGIN_ID}: {name}: {}", run.action.done.to_lowercase()),
                        "success",
                    );
                }
            }
            PowerStage::Setup(tag) => {
                let Some(i) = self.setups.iter().position(|s| s.0 == tag) else {
                    self.power = Some(run);
                    return;
                };
                let (_, saved) = self.setups.remove(i);
                if saved {
                    // the web asks again; the dialog's answer is the operator's yes
                    self.start_power(run.action, true, h);
                }
            }
        }
    }

    /// The popup's Save: pings an address, or saves a typed connection first.
    pub(crate) fn save_dialog(&mut self, h: &mut Host) {
        let Some(d) = &mut self.dialog else {
            return;
        };
        if !d.ssh {
            self.finish_dialog(String::new(), h);
        } else if !d.picked_new() {
            let c = d.choice.clone();
            self.finish_dialog(c, h);
        } else {
            let body = new_connection_body(&d.conn);
            d.saving = Some(h.post_any("/api/ssh/connections", body));
        }
    }

    fn finish_dialog(&mut self, connection: String, h: &mut Host) {
        let Some(d) = &self.dialog else {
            return;
        };
        let Some(next) = d.companion(&self.connections, &connection) else {
            return;
        };
        if let Some(before) = self.companion_button() {
            let changed = next.connection.is_empty() || next.connection != before.connection;
            if changed && self.live.contains(&session_name(&before.id)) {
                self.close_session(&before, h);
            }
        }
        self.companion = Some(next);
        self.persist(h);
        self.dialog = None;
        self.start_probing(h.now);
    }

    /// The popup's Clear: forget the companion and stop pinging it.
    pub(crate) fn clear_companion(&mut self, h: &mut Host) {
        if let Some(before) = self.companion_button() {
            if self.live.contains(&session_name(&before.id)) {
                self.close_session(&before, h);
            }
        }
        self.companion = None;
        self.persist(h);
        self.dialog = None;
        self.start_probing(h.now);
    }

    /// The answer of the popup's connection save.
    pub(crate) fn poll_dialog(&mut self, h: &mut Host) {
        let Some(d) = &mut self.dialog else {
            return;
        };
        let Some(r) = d.saving.as_ref().and_then(Pending::take) else {
            return;
        };
        d.saving = None;
        let res = reply_of(r);
        if res.get("ok") == Some(&json!(true)) {
            d.message = None;
            let name = new_connection_body(&d.conn)["name"].as_str().unwrap_or("").to_string();
            let list = res.get("connections").cloned().unwrap_or(Value::Null);
            self.adopt_connections(&list);
            self.finish_dialog(name, h);
        } else {
            d.message = Some((
                res.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("The connection could not be saved.")
                    .to_string(),
                "err",
            ));
        }
    }

    /// The companion's entry, for the arrow beside the status.
    pub(crate) fn companion_arrow(&self) -> Option<super::model::Button> {
        self.companion.as_ref().and_then(companion_entry)
    }
}
