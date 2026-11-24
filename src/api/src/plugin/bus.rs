//! How plugins may talk to each other without knowing each other.
//!
//! A plugin never links against another one. It can only put a value under a
//! name of its own ([`Host::publish`](super::Host::publish)), read what another
//! plugin published ([`Host::read`](super::Host::read)), ask whether another
//! plugin is running ([`Host::present`](super::Host::present)) and drop a
//! message into its mailbox ([`Host::send`](super::Host::send)). Every one of
//! these is a quiet no-op when the other side is not there, so two plugins can
//! cooperate when both are installed and switched on, and each of them works
//! alone when the other is missing, off or stopped after an error.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// A topic holds at most this much once printed, so a plugin cannot park an
/// unbounded value in the registry.
pub const MAX_TOPIC_BYTES: usize = 1 << 20;
/// Messages waiting for one plugin; the oldest are dropped past this.
pub const MAX_MAIL: usize = 64;

#[derive(Debug, Clone, PartialEq)]
struct Topic {
    rev: u64,
    value: Value,
}

/// What the registry keeps between the plugins.
#[derive(Debug, Default)]
pub struct Bus {
    topics: HashMap<String, Topic>,
    mail: HashMap<String, Vec<(String, Value)>>,
    live: HashSet<String>,
    rev: u64,
}

impl Bus {
    /// The plugins that are running right now, set by the registry each frame.
    pub fn set_live<'a>(&mut self, ids: impl Iterator<Item = &'a str>) {
        self.live = ids.map(str::to_string).collect();
    }

    pub fn is_live(&self, id: &str) -> bool {
        self.live.contains(id)
    }

    /// Stores `value` under `<owner>/<name>`. False when it is too large.
    pub fn publish(&mut self, owner: &str, name: &str, value: Value) -> bool {
        let key = format!("{owner}/{name}");
        if self.topics.get(&key).is_some_and(|t| t.value == value) {
            return true;
        }
        if value.to_string().len() > MAX_TOPIC_BYTES {
            return false;
        }
        self.rev += 1;
        self.topics.insert(key, Topic { rev: self.rev, value });
        true
    }

    /// Takes `<owner>/<name>` away; whoever reads it sees it vanish.
    pub fn retract(&mut self, owner: &str, name: &str) {
        if self.topics.remove(&format!("{owner}/{name}")).is_some() {
            self.rev += 1;
        }
    }

    /// Everything a plugin published goes: it stopped, or it was switched off.
    pub fn retract_all(&mut self, owner: &str) {
        let prefix = format!("{owner}/");
        let before = self.topics.len();
        self.topics.retain(|k, _| !k.starts_with(&prefix));
        self.mail.remove(owner);
        if self.topics.len() != before {
            self.rev += 1;
        }
    }

    /// The value of `topic` (`<owner>/<name>`), only while its owner runs.
    pub fn read(&self, topic: &str) -> Option<&Value> {
        let t = self.topics.get(topic)?;
        topic
            .split_once('/')
            .is_some_and(|(owner, _)| self.is_live(owner))
            .then_some(&t.value)
    }

    /// A number that changes whenever `topic` is published, retracted or its
    /// owner stops; 0 while there is nothing to read. Compare it to the last
    /// one seen instead of cloning the value every frame.
    pub fn rev(&self, topic: &str) -> u64 {
        if self.read(topic).is_some() {
            self.topics.get(topic).map_or(0, |t| t.rev)
        } else {
            0
        }
    }

    /// Puts a message into `to`'s mailbox. False when `to` is not running.
    pub fn send(&mut self, from: &str, to: &str, msg: Value) -> bool {
        if !self.is_live(to) || from == to {
            return false;
        }
        let q = self.mail.entry(to.to_string()).or_default();
        if q.len() >= MAX_MAIL {
            q.remove(0);
        }
        q.push((from.to_string(), msg));
        true
    }

    /// The messages waiting for `id`, oldest first.
    pub fn take_mail(&mut self, id: &str) -> Vec<(String, Value)> {
        self.mail.remove(id).unwrap_or_default()
    }

    pub fn has_mail(&self, id: &str) -> bool {
        self.mail.get(id).is_some_and(|q| !q.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn bus(live: &[&str]) -> Bus {
        let mut b = Bus::default();
        b.set_live(live.iter().copied());
        b
    }

    #[test]
    fn a_topic_is_readable_only_while_its_owner_runs() {
        let mut b = bus(&["a"]);
        assert!(b.publish("a", "list", json!([1])));
        assert_eq!(b.read("a/list"), Some(&json!([1])));
        b.set_live(std::iter::empty());
        assert_eq!(b.read("a/list"), None);
        assert_eq!(b.rev("a/list"), 0);
    }

    #[test]
    fn the_revision_moves_only_when_the_value_does() {
        let mut b = bus(&["a"]);
        b.publish("a", "list", json!([1]));
        let first = b.rev("a/list");
        assert!(first > 0);
        b.publish("a", "list", json!([1]));
        assert_eq!(b.rev("a/list"), first);
        b.publish("a", "list", json!([2]));
        assert!(b.rev("a/list") > first);
        b.retract("a", "list");
        assert_eq!(b.rev("a/list"), 0);
    }

    #[test]
    fn a_message_goes_only_to_a_running_plugin() {
        let mut b = bus(&["a", "b"]);
        assert!(b.send("a", "b", json!({"op": "x"})));
        assert!(!b.send("a", "c", json!({})));
        assert!(!b.send("a", "a", json!({})));
        assert!(b.has_mail("b"));
        assert_eq!(b.take_mail("b"), vec![("a".to_string(), json!({"op": "x"}))]);
        assert!(!b.has_mail("b"));
    }

    #[test]
    fn a_mailbox_and_a_topic_have_a_cap() {
        let mut b = bus(&["a", "b"]);
        for i in 0..(MAX_MAIL + 10) {
            b.send("a", "b", json!(i));
        }
        let mail = b.take_mail("b");
        assert_eq!(mail.len(), MAX_MAIL);
        assert_eq!(mail[0].1, json!(10));
        let big = Value::String("x".repeat(MAX_TOPIC_BYTES + 1));
        assert!(!b.publish("a", "big", big));
        assert_eq!(b.rev("a/big"), 0);
    }

    #[test]
    fn a_stopped_plugin_leaves_nothing_behind() {
        let mut b = bus(&["a", "b"]);
        b.publish("a", "list", json!(1));
        b.send("b", "a", json!(1));
        b.retract_all("a");
        assert_eq!(b.read("a/list"), None);
        assert!(!b.has_mail("a"));
    }
}
