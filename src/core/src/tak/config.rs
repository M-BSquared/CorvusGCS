//! The `tak` block of the config file: who Corvus is on a TAK network and which
//! connections it keeps. Nothing touches the network until the operator
//! switches a connection on, and the aircraft's position is shared only when
//! `share_position` says so.

use std::net::Ipv4Addr;

use serde_json::{json, Map, Value};

pub const KINDS: [&str; 4] = ["multicast", "udp", "tcp", "tls"];
pub const TEAMS: [&str; 14] = [
    "White",
    "Yellow",
    "Orange",
    "Magenta",
    "Red",
    "Maroon",
    "Purple",
    "Dark Blue",
    "Blue",
    "Cyan",
    "Teal",
    "Green",
    "Dark Green",
    "Brown",
];
pub const ROLES: [&str; 8] = [
    "Team Member",
    "Team Lead",
    "HQ",
    "Sniper",
    "Medic",
    "Forward Observer",
    "RTO",
    "K9",
];
pub const MAX_CONNECTIONS: usize = 8;
pub const DEFAULT_MULTICAST: &str = "239.2.3.1";
pub const DEFAULT_PORT: u16 = 6969;
pub const MIN_SHARE_S: f64 = 1.0;
pub const MAX_SHARE_S: f64 = 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub enabled: bool,
    /// Whether anything Corvus makes goes out on it; off, it only listens.
    pub send: bool,
    /// PEM files for `tls`: the server's CA (the system's own are used too),
    /// and the client certificate and key a TAK server asks for.
    pub ca_file: String,
    pub cert_file: String,
    pub key_file: String,
}

impl Connection {
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "kind": self.kind, "host": self.host, "port": self.port,
            "enabled": self.enabled, "send": self.send,
            "ca_file": self.ca_file, "cert_file": self.cert_file, "key_file": self.key_file,
        })
    }

    /// `raw` laid over `base`: what a settings call changes.
    pub fn parse(raw: &Value, base: Option<&Connection>) -> Result<Connection, String> {
        let o = raw.as_object().ok_or("a connection is an object")?;
        let text = |k: &str, max: usize, fallback: &str| -> Result<String, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(fallback.to_string()),
                Some(Value::String(s)) if s.chars().count() <= max && !s.chars().any(char::is_control) => {
                    Ok(s.trim().to_string())
                }
                Some(_) => Err(format!("{k} must be text of at most {max} characters")),
            }
        };
        let flag = |k: &str, fallback: bool| match o.get(k) {
            Some(Value::Bool(b)) => Ok(*b),
            None | Some(Value::Null) => Ok(fallback),
            Some(_) => Err(format!("{k} must be true or false")),
        };
        let b = base.cloned().unwrap_or_else(|| Connection {
            id: String::new(),
            name: String::new(),
            kind: "multicast".into(),
            host: DEFAULT_MULTICAST.into(),
            port: DEFAULT_PORT,
            enabled: false,
            send: true,
            ca_file: String::new(),
            cert_file: String::new(),
            key_file: String::new(),
        });
        let kind = text("kind", 12, &b.kind)?;
        if !KINDS.contains(&kind.as_str()) {
            return Err(format!("kind must be one of {}", KINDS.join(", ")));
        }
        let host = text("host", 253, &b.host)?;
        if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c)) {
            return Err("host must be a name or an address".into());
        }
        if kind == "multicast" && !host.parse::<Ipv4Addr>().is_ok_and(|a| a.is_multicast()) {
            return Err("a multicast connection needs a group address such as 239.2.3.1".into());
        }
        let port = match o.get("port") {
            None | Some(Value::Null) => b.port,
            Some(v) => v
                .as_u64()
                .filter(|p| (1..=65535).contains(p))
                .map(|p| p as u16)
                .ok_or("port must be 1 to 65535")?,
        };
        let id = text("id", 32, &b.id)?;
        if !id.is_empty() && !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err("id may hold letters, digits, - and _".into());
        }
        let name = text("name", 48, &b.name)?;
        Ok(Connection {
            id,
            name: if name.is_empty() {
                format!("{kind} {host}:{port}")
            } else {
                name
            },
            kind,
            host,
            port,
            enabled: flag("enabled", b.enabled)?,
            send: flag("send", b.send)?,
            ca_file: text("ca_file", 500, &b.ca_file)?,
            cert_file: text("cert_file", 500, &b.cert_file)?,
            key_file: text("key_file", 500, &b.key_file)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub callsign: String,
    /// Empty until the service makes one the first time it runs.
    pub uid: String,
    pub team: String,
    pub role: String,
    pub share_position: bool,
    pub share_interval_s: f64,
    pub connections: Vec<Connection>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            callsign: "Corvus".into(),
            uid: String::new(),
            team: "Cyan".into(),
            role: "Team Member".into(),
            share_position: false,
            share_interval_s: 2.0,
            connections: vec![Connection {
                id: "lan".into(),
                name: "Local network".into(),
                kind: "multicast".into(),
                host: DEFAULT_MULTICAST.into(),
                port: DEFAULT_PORT,
                enabled: false,
                send: true,
                ca_file: String::new(),
                cert_file: String::new(),
                key_file: String::new(),
            }],
        }
    }
}

fn valid_uid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

impl Settings {
    /// The settings in force for what a file or a call holds: what is well
    /// formed is taken, the rest stays as it was in `self`.
    pub fn merged(&self, raw: &Value) -> Result<Settings, String> {
        let Some(o) = raw.as_object() else {
            return Ok(self.clone());
        };
        let mut s = self.clone();
        match o.get("callsign") {
            None | Some(Value::Null) => {}
            Some(Value::String(c))
                if !c.trim().is_empty() && c.chars().count() <= 32 && !c.chars().any(char::is_control) =>
            {
                s.callsign = c.trim().to_string();
            }
            Some(_) => return Err("callsign must be 1 to 32 characters".into()),
        }
        match o.get("uid") {
            None | Some(Value::Null) => {}
            Some(Value::String(u)) if u.is_empty() => {}
            Some(Value::String(u)) if valid_uid(u) => s.uid = u.clone(),
            Some(_) => return Err("uid may hold letters, digits, . - and _ (64 at most)".into()),
        }
        for (key, list, target) in [("team", &TEAMS[..], &mut s.team), ("role", &ROLES[..], &mut s.role)] {
            match o.get(key) {
                None | Some(Value::Null) => {}
                Some(Value::String(v)) if list.contains(&v.as_str()) => *target = v.clone(),
                Some(_) => return Err(format!("{key} must be one of {}", list.join(", "))),
            }
        }
        match o.get("share_position") {
            None | Some(Value::Null) => {}
            Some(Value::Bool(b)) => s.share_position = *b,
            Some(_) => return Err("share_position must be true or false".into()),
        }
        match o.get("share_interval_s") {
            None | Some(Value::Null) => {}
            Some(v) => match v.as_f64().filter(|n| (MIN_SHARE_S..=MAX_SHARE_S).contains(n)) {
                Some(n) => s.share_interval_s = n,
                None => return Err(format!("share_interval_s must be {MIN_SHARE_S} to {MAX_SHARE_S}")),
            },
        }
        if let Some(list) = o.get("connections") {
            let list = list.as_array().ok_or("connections must be a list")?;
            if list.len() > MAX_CONNECTIONS {
                return Err(format!("at most {MAX_CONNECTIONS} connections"));
            }
            let mut out: Vec<Connection> = Vec::new();
            for item in list {
                let base = item
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| self.connections.iter().find(|c| c.id == id));
                let c = Connection::parse(item, base)?;
                out.push(c);
            }
            s.connections = number_ids(out)?;
        }
        Ok(s)
    }

    pub fn to_value(&self) -> Value {
        json!({
            "callsign": self.callsign, "uid": self.uid, "team": self.team, "role": self.role,
            "share_position": self.share_position, "share_interval_s": self.share_interval_s,
            "connections": self.connections.iter().map(Connection::to_value).collect::<Vec<_>>(),
        })
    }

    /// Inserts or changes one connection by id (a new id makes a new one).
    pub fn with_connection(&self, raw: &Value) -> Result<(Settings, Connection), String> {
        let id = raw.get("id").and_then(Value::as_str).unwrap_or("");
        let existing = self.connections.iter().position(|c| c.id == id);
        if existing.is_none() && !id.is_empty() {
            return Err(format!("there is no connection {id}"));
        }
        let c = Connection::parse(raw, existing.map(|i| &self.connections[i]))?;
        let mut list = self.connections.clone();
        match existing {
            Some(i) => list[i] = c,
            None => {
                if list.len() >= MAX_CONNECTIONS {
                    return Err(format!("at most {MAX_CONNECTIONS} connections"));
                }
                list.push(c);
            }
        }
        let list = number_ids(list)?;
        let changed = match existing {
            Some(i) => list[i].clone(),
            None => list[list.len() - 1].clone(),
        };
        Ok((
            Settings {
                connections: list,
                ..self.clone()
            },
            changed,
        ))
    }

    pub fn without_connection(&self, id: &str) -> Result<Settings, String> {
        if !self.connections.iter().any(|c| c.id == id) {
            return Err(format!("there is no connection {id}"));
        }
        Ok(Settings {
            connections: self.connections.iter().filter(|c| c.id != id).cloned().collect(),
            ..self.clone()
        })
    }
}

/// Gives every connection without an id the next free `c<n>`, and refuses two
/// with the same id.
fn number_ids(mut list: Vec<Connection>) -> Result<Vec<Connection>, String> {
    let mut taken: Vec<String> = list.iter().map(|c| c.id.clone()).filter(|i| !i.is_empty()).collect();
    let before = taken.len();
    taken.sort();
    taken.dedup();
    if taken.len() != before {
        return Err("two connections share an id".into());
    }
    let mut n = 1;
    for c in list.iter_mut().filter(|c| c.id.is_empty()) {
        while taken.contains(&format!("c{n}")) {
            n += 1;
        }
        c.id = format!("c{n}");
        taken.push(c.id.clone());
    }
    Ok(list)
}

/// The block as the config file keeps it: what is well formed, and the defaults
/// for what is not. `None` when the block is not an object.
pub fn coerce_settings(raw: &Value) -> Option<Map<String, Value>> {
    raw.as_object()?;
    let base = Settings::default();
    // One bad field of a hand edited file costs that field, not the block.
    let mut s = base.clone();
    for (key, value) in raw.as_object()? {
        if key == "connections" {
            let mut list = Vec::new();
            for item in value.as_array().map(Vec::as_slice).unwrap_or_default() {
                if let Ok(c) = Connection::parse(item, None) {
                    list.push(c);
                }
                if list.len() >= MAX_CONNECTIONS {
                    break;
                }
            }
            if let Ok(l) = number_ids(list) {
                s.connections = l;
            }
        } else if let Ok(next) = s.merged(&json!({ key: value })) {
            s = next;
        }
    }
    s.to_value().as_object().cloned()
}

pub fn effective(raw: Option<&Value>) -> Settings {
    raw.and_then(coerce_settings)
        .and_then(|m| Settings::default().merged(&Value::Object(m)).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_on_by_default() {
        let s = Settings::default();
        assert!(!s.share_position);
        assert!(s.connections.iter().all(|c| !c.enabled));
        assert_eq!(effective(None), s);
        assert_eq!(effective(Some(&json!("nonsense"))), s);
    }

    #[test]
    fn a_damaged_file_keeps_what_is_good() {
        let raw = json!({
            "callsign": "Raven", "team": "Plaid", "share_interval_s": 500, "share_position": "yes",
            "connections": [
                {"name": "ok", "kind": "tcp", "host": "tak.example", "port": 8087, "enabled": true},
                {"kind": "tcp", "host": "bad host", "port": 1},
                {"kind": "multicast", "host": "10.0.0.1"},
                {"kind": "tcp", "host": "x", "port": 70000},
                7
            ],
        });
        let s = effective(Some(&raw));
        assert_eq!(s.callsign, "Raven");
        assert_eq!(s.team, "Cyan");
        assert_eq!(s.share_interval_s, 2.0);
        assert!(!s.share_position);
        assert_eq!(s.connections.len(), 1);
        assert_eq!(s.connections[0].id, "c1");
        assert!(s.connections[0].enabled && s.connections[0].send);
        assert_eq!(effective(Some(&s.to_value())), s);
    }

    #[test]
    fn a_call_is_refused_with_a_reason() {
        let s = Settings::default();
        for (patch, why) in [
            (json!({"callsign": ""}), "callsign"),
            (json!({"team": "Plaid"}), "team"),
            (json!({"share_interval_s": 0.1}), "share_interval_s"),
            (json!({"share_position": 1}), "share_position"),
            (json!({"uid": "a b"}), "uid"),
            (json!({"connections": [{"kind": "ftp"}]}), "kind"),
            (
                json!({"connections": [{"kind": "tcp", "host": "h", "port": 0}]}),
                "port",
            ),
            (
                json!({"connections": [{"kind": "multicast", "host": "192.168.1.1"}]}),
                "multicast",
            ),
            (json!({"connections": [{"id": "a"}, {"id": "a"}]}), "share an id"),
        ] {
            let e = s.merged(&patch).unwrap_err();
            assert!(e.contains(why), "{patch}: {e}");
        }
        let many: Vec<Value> = (0..9).map(|_| json!({"kind": "tcp", "host": "h"})).collect();
        assert!(s.merged(&json!({"connections": many})).is_err());
    }

    #[test]
    fn connections_are_changed_one_at_a_time() {
        let s = Settings::default();
        let (s, on) = s.with_connection(&json!({"id": "lan", "enabled": true})).unwrap();
        assert!(on.enabled && on.kind == "multicast" && on.port == DEFAULT_PORT);
        let (s, added) = s
            .with_connection(&json!({"name": "Server", "kind": "tls", "host": "tak.example", "port": 8089}))
            .unwrap();
        assert_eq!(added.id, "c1");
        assert!(!added.enabled);
        assert_eq!(s.connections.len(), 2);
        assert!(s.with_connection(&json!({"id": "nope"})).is_err());
        let s = s.without_connection("lan").unwrap();
        assert_eq!(s.connections.len(), 1);
        assert!(s.without_connection("lan").is_err());
    }
}
