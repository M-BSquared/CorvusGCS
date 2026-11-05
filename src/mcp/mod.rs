//! This window's half of the MCP server (`python/src/js/mcp.js`).
//!
//! The server runs in the backend, but some of its jobs need a person:
//!
//! * ASKING. A tool the operator set to "ask first" waits for an answer. The
//!   question arrives on the `mcp` event topic and is a dialog here: what the
//!   AI client wants, in its own words, Allow or Refuse. It closes by itself
//!   when the backend gives up waiting, or when another window answered.
//! * THE INDICATOR. While an AI client is connected and may change something,
//!   a pill sits on the map saying so, stronger while the client flies the
//!   vehicle directly. Its Stop takes every permission that changes anything
//!   away at once and puts a vehicle under direct control in hold.
//! * THE LINE. What the server says it did goes to the notification board
//!   when it refused something.
//!
//! PLUGIN TOOLS (`mcpTools`). Every 10 s, and at once when they change, this
//! window tells the backend it is open and which tools its plugins offer; a
//! `call` event addressed to this window is handed to the plugin
//! (`take_calls`) and its answer goes back through `reply`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::http::{self, Server};
use crate::net::stream::Stream;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::modal::{self, Size};
use crate::ui::style::{self, Weight};

const HEARTBEAT: Duration = Duration::from_secs(10);

/// What the pill says.
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    pub level: &'static str,
    pub text: String,
}

/// `badgeState`: what the pill says for a status, or None for no pill.
pub fn badge_state(s: &Value) -> Option<Badge> {
    if s.get("enabled") != Some(&Value::Bool(true)) {
        return None;
    }
    if let Some(c) = s.get("control").filter(|c| c.is_object()) {
        let what = if c.get("kind").and_then(Value::as_str) == Some("manual") {
            "sticks"
        } else {
            "velocity"
        };
        return Some(Badge {
            level: "control",
            text: format!("AI client flying ({what})"),
        });
    }
    if s.get("pending")
        .and_then(Value::as_array)
        .is_some_and(|p| !p.is_empty())
    {
        return Some(Badge {
            level: "ask",
            text: "AI client is asking".into(),
        });
    }
    let active = s.get("active") == Some(&Value::Bool(true));
    let writes = s.get("writes") == Some(&Value::Bool(true));
    (active && writes).then(|| Badge {
        level: "on",
        text: "AI client connected".into(),
    })
}

/// A request waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub id: String,
    pub client: String,
    pub what: String,
    pub permission: String,
    /// When it refuses itself.
    pub deadline: Instant,
}

impl Question {
    pub fn from_event(e: &Value, now: Instant) -> Option<Question> {
        let id = e.get("id").and_then(Value::as_str).filter(|i| !i.is_empty())?;
        let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("");
        let what = if s("summary").is_empty() {
            s("tool")
        } else {
            s("summary")
        };
        let perm = if s("permission_label").is_empty() {
            s("permission")
        } else {
            s("permission_label")
        };
        let secs = e.get("expires_in_s").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
        Some(Question {
            id: id.to_string(),
            client: if s("client").is_empty() {
                "An AI client".into()
            } else {
                s("client").into()
            },
            what: what.to_string(),
            permission: perm.to_string(),
            deadline: now + Duration::from_secs_f64(secs),
        })
    }

    /// "Refused by itself in 12 s."
    pub fn left_text(&self, now: Instant) -> String {
        let s = self.deadline.saturating_duration_since(now).as_secs_f64().round() as u64;
        format!("Refused by itself in {s} s.")
    }

    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

#[derive(Default)]
struct Shared {
    /// This window's id: a call is for it only when it names it.
    window: String,
    calls: Vec<(String, String, String, Value)>,
    status: Value,
    questions: Vec<Question>,
    closed: Vec<String>,
    notes: Vec<(String, String)>,
}

/// The MCP half of the window.
pub struct Mcp {
    server: Server,
    shared: Arc<Mutex<Shared>>,
    stream: Option<Stream>,
    window: String,
    tools: Vec<Value>,
    last_beat: Option<Instant>,
    stopping: bool,
    stop_answer: Option<crate::net::bg::Pending<Result<Value, String>>>,
    errors: Vec<String>,
}

fn take_event(shared: &Mutex<Shared>, e: &Value) {
    let Ok(mut g) = shared.lock() else { return };
    match e.get("type").and_then(Value::as_str) {
        Some("status") => {
            g.status = e.clone();
            let now = Instant::now();
            let pending: Vec<Question> = e
                .get("pending")
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(|q| Question::from_event(q, now)).collect())
                .unwrap_or_default();
            for q in pending {
                if !g.questions.iter().any(|k| k.id == q.id) {
                    g.questions.push(q);
                }
            }
        }
        Some("confirm") => {
            if let Some(q) = Question::from_event(e, Instant::now()) {
                if !g.questions.iter().any(|k| k.id == q.id) {
                    g.questions.push(q);
                }
            }
        }
        Some("confirm_done") => {
            if let Some(id) = e.get("id").and_then(Value::as_str) {
                g.questions.retain(|q| q.id != id);
                g.closed.push(id.to_string());
            }
        }
        Some("call") => {
            let for_us = e.get("window").and_then(Value::as_str) == Some(g.window.as_str());
            let text = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
            if let (true, Some(id), Some(plugin), Some(tool)) = (for_us, text("id"), text("plugin"), text("tool")) {
                g.calls
                    .push((id, plugin, tool, e.get("args").cloned().unwrap_or_else(|| json!({}))));
            }
        }
        Some("log") => {
            let text = e.get("text").and_then(Value::as_str).unwrap_or("");
            // A warning, never a critical: a refused call is news, not an
            // emergency that pins itself over the panel.
            if e.get("notify").and_then(Value::as_bool) == Some(true) && !text.is_empty() {
                g.notes.push(("warning".into(), text.to_string()));
            }
        }
        _ => {}
    }
}

impl Mcp {
    pub fn start(server: Server, ctx: &egui::Context) -> Mcp {
        let window = format!("n{:x}", std::process::id() as u64 * 7919 + 13);
        let shared: Arc<Mutex<Shared>> = Arc::new(Mutex::new(Shared {
            window: window.clone(),
            ..Default::default()
        }));
        let (s, c) = (shared.clone(), ctx.clone());
        let stream = Stream::start(server.clone(), "/api/events?topics=mcp".into(), move |ev| {
            if ev.name == "mcp" {
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                    take_event(&s, &v);
                    c.request_repaint();
                }
            }
        });
        let (s, srv, c) = (shared.clone(), server.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Ok(d) = http::get_json(&srv, "/api/mcp") {
                if let Some(st) = d.get("status") {
                    let mut e = st.clone();
                    if let Some(o) = e.as_object_mut() {
                        o.insert("type".into(), json!("status"));
                    }
                    take_event(&s, &e);
                }
            }
            c.request_repaint();
        });
        Mcp {
            server,
            shared,
            stream: Some(stream),
            window,
            tools: Vec::new(),
            last_beat: None,
            stopping: false,
            stop_answer: None,
            errors: Vec::new(),
        }
    }

    pub fn badge(&self) -> Option<Badge> {
        self.shared.lock().ok().and_then(|g| badge_state(&g.status))
    }

    /// The plugins' tools as they stand now. A change is announced at once.
    pub fn set_tools(&mut self, tools: Vec<Value>) {
        if tools != self.tools {
            self.tools = tools;
            self.last_beat = None;
        }
    }

    /// The calls an AI client made for this window: `(id, plugin, tool, args)`.
    pub fn take_calls(&mut self) -> Vec<(String, String, String, Value)> {
        self.shared
            .lock()
            .map(|mut g| std::mem::take(&mut g.calls))
            .unwrap_or_default()
    }

    /// A plugin's answer to a call.
    pub fn reply(&self, id: &str, result: Result<Value, String>) {
        let (server, id) = (self.server.clone(), id.to_string());
        std::thread::spawn(move || {
            let body = match result {
                Ok(v) => json!({"id": id, "ok": true, "result": v, "error": null}),
                Err(e) => json!({"id": id, "ok": false, "result": null, "error": e}),
            };
            let _ = http::post_json(&server, "/api/mcp/result", &body);
        });
    }

    /// What the server refused, for the board.
    pub fn take_notes(&mut self) -> Vec<(String, String)> {
        let mut out = self
            .shared
            .lock()
            .map(|mut g| std::mem::take(&mut g.notes))
            .unwrap_or_default();
        out.extend(
            std::mem::take(&mut self.errors)
                .into_iter()
                .map(|e| ("critical".to_string(), e)),
        );
        out
    }

    fn answer(&self, id: &str, allow: bool) {
        let (server, id) = (self.server.clone(), id.to_string());
        std::thread::spawn(move || {
            let _ = http::post_json(&server, "/api/mcp/confirm", &json!({"id": id, "allow": allow}));
        });
    }

    /// One step: the heartbeat, the dialog, the pill.
    pub fn show(&mut self, ctx: &egui::Context, host: Option<Rect>, below: f32) {
        let now = Instant::now();
        let enabled = self
            .shared
            .lock()
            .map(|g| g.status.get("enabled") == Some(&Value::Bool(true)))
            .unwrap_or(false);
        if enabled && self.last_beat.is_none_or(|t| now.duration_since(t) >= HEARTBEAT) {
            self.last_beat = Some(now);
            let (server, id, tools) = (self.server.clone(), self.window.clone(), self.tools.clone());
            std::thread::spawn(move || {
                let _ = http::post_json(&server, "/api/mcp/window", &json!({"id": id, "tools": tools}));
            });
        }
        if let Some(r) = self.stop_answer.as_ref().and_then(crate::net::bg::Pending::take) {
            self.stop_answer = None;
            self.stopping = false;
            match r {
                Ok(v) => {
                    if let (Some(st), Ok(mut g)) = (v.get("status"), self.shared.lock()) {
                        g.status = st.clone();
                    }
                }
                Err(e) => self.errors.push(format!("Stop failed: {e}")),
            }
        }
        // The oldest question; one at a time.
        let question = {
            let Ok(mut g) = self.shared.lock() else { return };
            g.questions.retain(|q| !q.expired(now));
            g.questions.first().cloned()
        };
        if let Some(q) = question {
            self.ask(ctx, &q, now);
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if let (Some(b), Some(host)) = (self.badge(), host) {
            self.pill(ctx, host, below, &b);
        }
    }

    fn ask(&mut self, ctx: &egui::Context, q: &Question, now: Instant) {
        let mut hit = None;
        modal::show(ctx, "mcp-ask", Size::Sm, "AI client request", |ui, x, y, w| {
            let mut y = y;
            let who = kit::wrapped(ui, &format!("{} asks to:", q.client), 12.0, style::text_2(), w);
            ui.painter().galley(Pos2::new(x, y), who.clone(), style::text_2());
            y += who.size().y + 8.0;
            let what = kit::wrapped_weight(ui, &q.what, 14.0, Weight::W600, style::text_1(), w, 1.4);
            ui.painter().galley(Pos2::new(x, y), what.clone(), style::text_1());
            y += what.size().y + 8.0;
            let why = kit::wrapped(
                ui,
                &format!("Permission: {}. Nothing is sent until you allow it.", q.permission),
                12.0,
                style::text_2(),
                w,
            );
            ui.painter().galley(Pos2::new(x, y), why.clone(), style::text_2());
            y += why.size().y + 8.0;
            let left = kit::wrapped(ui, &q.left_text(now), 12.0, style::text_2(), w);
            ui.painter().galley(Pos2::new(x, y), left.clone(), style::text_2());
            y += left.size().y + 8.0;
            let (h, bottom) = modal::actions(
                ui,
                x,
                y + 8.0,
                w,
                vec![
                    Button::new("Refuse", Variant::Secondary).icon("x"),
                    Button::new("Allow", Variant::Primary).icon("check"),
                ],
            );
            hit = h;
            bottom
        });
        // The dialog cannot be dismissed: only an answer, or the backend
        // giving up, closes it.
        if let Some(i) = hit {
            self.answer(&q.id, i == 1);
            if let Ok(mut g) = self.shared.lock() {
                g.questions.retain(|k| k.id != q.id);
            }
        }
    }

    /// `.mcp-badge`: a pill under the flight bar with a dot, the words and
    /// Stop.
    fn pill(&mut self, ctx: &egui::Context, host: Rect, below: f32, b: &Badge) {
        let id = egui::Id::NULL.with("mcp-badge");
        egui::Area::new(id)
            .order(egui::Order::Middle)
            .fixed_pos(host.min)
            .show(ctx, |ui| {
                let g = style::galley(ui, &b.text, 12.0, Weight::W600, style::text_1(), 0.0);
                let stop = Button::new("Stop", Variant::Danger).icon("octagon-x").small().enabled(!self.stopping);
                let (sw, sh) = (stop.width(ui), stop.height());
                let w = 12.0 + 8.0 + 8.0 + g.size().x + 8.0 + sw + 4.0;
                let h = sh + 8.0;
                let top = host.top() + below + 4.0;
                let r = Rect::from_min_size(Pos2::new(host.center().x - w / 2.0, top), Vec2::new(w, h));
                let painter = ui.painter().clone();
                let border = if b.level == "control" { style::critical() } else { style::warning() };
                painter.add(style::shadow_pop().as_shape(r, 255));
                painter.rect_filled(r, 255, style::glass(0.8));
                painter.rect_stroke(r, 255, Stroke::new(1.0, border), egui::StrokeKind::Inside);
                let t = ui.input(|i| i.time) as f32;
                let pulse = if b.level == "on" { 1.0 } else { 0.675 + 0.325 * (t * std::f32::consts::TAU).cos() };
                let dot = if b.level == "control" { style::critical() } else { style::warning() };
                painter.circle_filled(Pos2::new(r.left() + 12.0 + 4.0, r.center().y), 4.0, dot.gamma_multiply(pulse));
                if b.level != "on" {
                    ui.ctx().request_repaint();
                }
                painter.galley(Pos2::new(r.left() + 12.0 + 8.0 + 8.0, r.center().y - g.size().y / 2.0), g, style::text_1());
                let sr = Rect::from_min_size(Pos2::new(r.right() - 4.0 - sw, r.center().y - sh / 2.0), Vec2::new(sw, sh));
                if stop.title("Take every permission that changes the vehicle away from the AI client, and hold a vehicle it flies directly")
                    .show(ui, sr, "mcpstop")
                {
                    self.stopping = true;
                    let s = self.server.clone();
                    self.stop_answer = Some(crate::net::bg::Pending::spawn(ui.ctx(), move || {
                        http::post_json(&s, "/api/mcp/stop", &json!({}))
                    }));
                }
            });
    }

    pub fn shut_down(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_follows_what_the_client_may_do() {
        assert_eq!(
            badge_state(&json!({"enabled": false, "active": true, "writes": true})),
            None
        );
        assert_eq!(
            badge_state(&json!({"enabled": true, "active": true, "writes": false})),
            None
        );
        assert_eq!(
            badge_state(&json!({"enabled": true, "active": true, "writes": true})),
            Some(Badge {
                level: "on",
                text: "AI client connected".into()
            })
        );
        assert_eq!(
            badge_state(&json!({"enabled": true, "pending": [{"id": "a"}]})),
            Some(Badge {
                level: "ask",
                text: "AI client is asking".into()
            })
        );
        assert_eq!(
            badge_state(&json!({"enabled": true, "control": {"kind": "manual"}, "pending": [{"id": "a"}]})),
            Some(Badge {
                level: "control",
                text: "AI client flying (sticks)".into()
            })
        );
        assert_eq!(
            badge_state(&json!({"enabled": true, "control": {"kind": "velocity"}})).map(|b| b.text),
            Some("AI client flying (velocity)".to_string())
        );
    }

    #[test]
    fn a_question_says_who_what_and_how_long() {
        let now = Instant::now();
        let q = Question::from_event(
            &json!({"id": "c1", "client": "OpenCode", "summary": "Arm the vehicle", "tool": "arm", "permission": "arm", "permission_label": "Arm and disarm", "expires_in_s": 30}),
            now,
        )
        .expect("a question");
        assert_eq!(
            (q.client.as_str(), q.what.as_str(), q.permission.as_str()),
            ("OpenCode", "Arm the vehicle", "Arm and disarm")
        );
        assert_eq!(q.left_text(now), "Refused by itself in 30 s.");
        assert!(!q.expired(now) && q.expired(now + Duration::from_secs(31)));
        let bare =
            Question::from_event(&json!({"id": "c2", "tool": "land", "permission": "land"}), now).expect("a question");
        assert_eq!(
            (bare.client.as_str(), bare.what.as_str(), bare.permission.as_str()),
            ("An AI client", "land", "land")
        );
        assert!(Question::from_event(&json!({"tool": "x"}), now).is_none());
    }

    #[test]
    fn events_keep_the_status_the_questions_and_the_refusals() {
        let shared = Mutex::new(Shared::default());
        take_event(
            &shared,
            &json!({"type": "status", "enabled": true, "pending": [{"id": "a", "tool": "arm"}]}),
        );
        take_event(&shared, &json!({"type": "confirm", "id": "a", "tool": "arm"}));
        take_event(&shared, &json!({"type": "confirm", "id": "b", "tool": "land"}));
        {
            let g = shared.lock().expect("shared");
            assert_eq!(g.questions.len(), 2, "one question is asked once");
            assert_eq!(g.status["enabled"], true);
        }
        take_event(&shared, &json!({"type": "confirm_done", "id": "a"}));
        take_event(&shared, &json!({"type": "log", "text": "refused arm", "notify": true}));
        take_event(&shared, &json!({"type": "log", "text": "just a line"}));
        let g = shared.lock().expect("shared");
        assert_eq!(g.questions.len(), 1);
        assert_eq!(g.notes, vec![("warning".to_string(), "refused arm".to_string())]);
    }
}
