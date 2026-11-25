//! What a plugin sends the program and what comes back: the effects of the call
//! under way, and who waits for which delivery.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use serde_json::Value;

use crate::net::bg::Pending;
use crate::wire::{self, Effect};

type OnAnswer = Box<dyn FnOnce(wire::Answer) + Send>;
type OnEvent = Box<dyn FnMut(&str, &Value) + Send>;
type OnFetched = Box<dyn FnOnce(wire::Fetched) + Send>;

#[derive(Default)]
struct Mail {
    next: u64,
    effects: Vec<Effect>,
    answers: HashMap<u64, OnAnswer>,
    /// `None` while the handler of a stream is out being called.
    streams: HashMap<u64, Option<OnEvent>>,
    fetches: HashMap<u64, OnFetched>,
}

static MAIL: Mutex<Option<Mail>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Mail) -> R) -> R {
    let mut guard: MutexGuard<'_, Option<Mail>> = MAIL.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(Mail::default))
}

fn next() -> u64 {
    with(|m| {
        m.next += 1;
        m.next
    })
}

pub fn push(effect: Effect) {
    with(|m| m.effects.push(effect));
}

/// The effects since the last call of this.
pub fn take() -> Vec<Effect> {
    with(|m| std::mem::take(&mut m.effects))
}

/// Sends `effect`, made with a new number, and reads its answer with `read`.
fn ask<T: Send + 'static>(
    effect: impl FnOnce(u64) -> Effect,
    read: impl FnOnce(wire::Answer) -> T + Send + 'static,
) -> Pending<T> {
    let (pending, filler) = Pending::slot();
    let id = next();
    let effect = effect(id);
    with(|m| {
        m.answers.insert(id, Box::new(move |a| filler.fill(read(a))));
        m.effects.push(effect);
    });
    pending
}

/// A body is the answer only with a status of success, else its `error` is.
fn json_of(a: wire::Answer) -> Result<Value, String> {
    if let Some(e) = a.error {
        return Err(e);
    }
    if (200..300).contains(&a.status) {
        return Ok(a.body);
    }
    let said = a.body.get("error").and_then(Value::as_str).map(str::to_string);
    Err(said.unwrap_or_else(|| format!("Request failed ({})", a.status)))
}

fn request_effect(method: &str, path: &str, body: Option<Value>) -> impl FnOnce(u64) -> Effect {
    let (method, path) = (method.to_string(), path.to_string());
    move |id| Effect::Request(wire::Request { id, method, path, body })
}

pub fn request(method: &str, path: &str, body: Option<Value>) -> Pending<Result<Value, String>> {
    ask(request_effect(method, path, body), json_of)
}

/// A call whose body is the answer whatever the status.
pub fn request_any(method: &str, path: &str, body: Option<Value>) -> Pending<Result<(u16, Value), String>> {
    ask(request_effect(method, path, body), |a| match a.error {
        Some(e) => Err(e),
        None => Ok((a.status, a.body)),
    })
}

pub fn save(patch: Value, replace: bool) -> Pending<Result<Value, String>> {
    ask(|id| Effect::Save(wire::Save { id, patch, replace }), json_of)
}

/// A request to another server. `on_done` runs inside a later call.
pub fn fetch(mut fetch: wire::Fetch, on_done: impl FnOnce(wire::Fetched) + Send + 'static) -> u64 {
    let id = next();
    fetch.id = id;
    with(|m| {
        m.fetches.insert(id, Box::new(on_done));
        m.effects.push(Effect::Fetch(fetch));
    });
    id
}

pub fn cancel_fetch(id: u64) {
    with(|m| {
        if m.fetches.remove(&id).is_some() {
            m.effects.push(Effect::CancelFetch(wire::Id { id }));
        }
    });
}

/// The events of a subscription, for as long as this is kept.
pub struct Stream {
    id: u64,
}

impl Stream {
    pub(crate) fn subscribe(topics: &str, on_event: impl FnMut(&str, &Value) + Send + 'static) -> Stream {
        let id = next();
        let topics = topics.to_string();
        with(|m| {
            m.streams.insert(id, Some(Box::new(on_event)));
            m.effects.push(Effect::Subscribe(wire::Subscribe { id, topics }));
        });
        Stream { id }
    }

    pub fn stop(&mut self) {
        let id = std::mem::take(&mut self.id);
        with(|m| {
            if m.streams.remove(&id).is_some() {
                m.effects.push(Effect::Unsubscribe(wire::Id { id }));
            }
        });
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Hands a delivery to whoever waits for it. No lock is held while it does, so
/// a handler may ask for the next thing.
pub fn deliver(delivery: wire::Delivery) {
    match delivery {
        wire::Delivery::Answer(a) => {
            if let Some(f) = with(|m| m.answers.remove(&a.id)) {
                f(a);
            }
        }
        wire::Delivery::Fetched(f) => {
            if let Some(on) = with(|m| m.fetches.remove(&f.id)) {
                on(f);
            }
        }
        wire::Delivery::Event(e) => {
            let Some(mut on) = with(|m| m.streams.get_mut(&e.id).and_then(Option::take)) else {
                return;
            };
            on(&e.topic, &e.data);
            // Dropped from inside its own handler, it stays dropped.
            with(|m| {
                if let Some(slot) = m.streams.get_mut(&e.id) {
                    *slot = Some(on);
                }
            });
        }
    }
}
