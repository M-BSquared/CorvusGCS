//! What the Video page says and sends (`src/js/setup-video.js`): the words for
//! a camera's state, the line under its name, the editor's draft, why it
//! cannot be saved yet, and the request bodies. The rules are the web page's;
//! the tests hold the same cases.

use serde_json::{json, Map, Value};

use crate::data::units::{js_number, js_round};

/// How often the camera list is re-read while the page is open.
pub const POLL_MS: f64 = 2000.0;

/// The most cameras the backend keeps when it does not say.
pub const MAX_STREAMS: usize = 8;

/// What a row says for each state (`STATE_TEXT`).
pub fn state_text(state: &str) -> &'static str {
    match state {
        "starting" => "Connecting",
        "live" => "Live",
        "stalled" => "No picture",
        "error" => "Offline",
        "unavailable" => "No decoder",
        _ => "Not open",
    }
}

/// The dot's level for a state (`STATE_LEVEL`): `off`, `warning`, `healthy`
/// or `critical`.
pub fn state_level(state: &str) -> &'static str {
    match state {
        "starting" | "stalled" => "warning",
        "live" => "healthy",
        "error" | "unavailable" => "critical",
        _ => "off",
    }
}

/// A state the page knows, else `idle`.
pub fn known_state(state: &str) -> &str {
    match state {
        "idle" | "starting" | "live" | "stalled" | "error" | "unavailable" => state,
        _ => "idle",
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `detailLine`: the picture when it is live, what went wrong when it is not,
/// and the address otherwise.
pub fn detail_line(s: &Value) -> String {
    let state = text(s, "state");
    let (w, h) = (number(s, "width"), number(s, "height"));
    if state == "live" && w != 0.0 && h != 0.0 {
        let fps = number(s, "fps");
        let fps = if fps > 0.0 {
            format!(", {} fps", js_number(js_round(fps)))
        } else {
            String::new()
        };
        return format!("{} \u{d7} {}{fps}", js_number(w), js_number(h));
    }
    let message = text(s, "message");
    if (state == "error" || state == "stalled") && !message.is_empty() {
        return message.to_string();
    }
    let address = text(s, "address");
    if address.is_empty() {
        text(s, "url").to_string()
    } else {
        address.to_string()
    }
}

/// The picture a window reports for a camera: state, message and size.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Local {
    pub state: String,
    pub message: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

/// `shownState`: an RTSP camera's state is the backend's; a WebRTC camera's is
/// only known to its window, and one with no window open is not open.
pub fn shown_state(s: &Value, local: Option<&Local>) -> Value {
    if text(s, "kind") != "webrtc" {
        return s.clone();
    }
    let mut out: Map<String, Value> = s.as_object().cloned().unwrap_or_default();
    let (state, message, width, height, fps) = match local {
        Some(l) => (l.state.as_str(), l.message.as_str(), l.width, l.height, l.fps),
        None => ("idle", "", 0, 0, 0.0),
    };
    out.insert("state".into(), json!(state));
    out.insert("message".into(), json!(message));
    out.insert("width".into(), json!(width));
    out.insert("height".into(), json!(height));
    out.insert("fps".into(), json!(fps));
    Value::Object(out)
}

/// Which kind of camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rtsp,
    Webrtc,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        if s == "webrtc" {
            Kind::Webrtc
        } else {
            Kind::Rtsp
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Kind::Rtsp => "rtsp",
            Kind::Webrtc => "webrtc",
        }
    }

    /// `KIND_TEXT[kind].placeholder`.
    pub fn placeholder(self) -> &'static str {
        match self {
            Kind::Rtsp => "rtsp://192.168.144.25:8554/main.264",
            Kind::Webrtc => "http://192.168.144.25:8889/cam/whep",
        }
    }

    /// `KIND_TEXT[kind].hint`: the address field's hint.
    pub fn address_hint(self) -> &'static str {
        match self {
            Kind::Rtsp => {
                "The camera's RTSP address, from its manual or its web page. udp://, srt:// and http:// streams work too. A user and password written into the address are moved to the fields below."
            }
            Kind::Webrtc => {
                "The WHEP address of the stream on a media server such as MediaMTX (http://host:8889/<stream>/whep), go2rtc or Janus. The video goes straight from there to the window; Corvus only opens the connection."
            }
        }
    }

    /// `KIND_TEXT[kind].password`: the password field's hint.
    pub fn password_hint(self) -> &'static str {
        match self {
            Kind::Rtsp => "Kept by Corvus on this computer and never shown again.",
            Kind::Webrtc => {
                "Kept by Corvus on this computer and never shown again. For a token, leave User empty and put the token here."
            }
        }
    }
}

/// The editor's draft of one camera. Nothing is saved until Save.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub url: String,
    pub username: String,
    pub password: String,
    pub password_touched: bool,
    pub has_password: bool,
    pub transport: String,
}

impl Draft {
    /// The editor for a camera that does not exist yet.
    pub fn blank() -> Draft {
        Draft {
            id: String::new(),
            name: String::new(),
            kind: Kind::Rtsp,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            password_touched: false,
            has_password: false,
            transport: "tcp".into(),
        }
    }

    /// The editor for a camera of GET /api/video/status.
    pub fn from_stream(c: &Value) -> Draft {
        Draft {
            id: text(c, "id").into(),
            name: text(c, "name").into(),
            kind: Kind::parse(text(c, "kind")),
            url: text(c, "url").into(),
            username: text(c, "username").into(),
            password: String::new(),
            password_touched: false,
            has_password: c.get("has_password").and_then(Value::as_bool).unwrap_or(false),
            transport: if text(c, "transport") == "udp" { "udp" } else { "tcp" }.into(),
        }
    }

    /// `saveBody`: the password goes only when one was typed, because the page
    /// never had the stored one to send back.
    pub fn save_body(&self) -> Value {
        let mut body = json!({
            "name": self.name.trim(),
            "kind": self.kind.id(),
            "url": self.url.trim(),
            "username": self.username.trim(),
            "transport": if self.transport == "udp" { "udp" } else { "tcp" },
        });
        if !self.id.is_empty() {
            body["id"] = json!(self.id);
        }
        if self.password_touched {
            body["password"] = json!(self.password);
        }
        body
    }

    /// `draftProblem`: why the draft cannot be saved yet, or empty.
    pub fn problem(&self) -> &'static str {
        let url = self.url.trim();
        if url.is_empty() {
            return "Enter the camera's address.";
        }
        if self.kind == Kind::Webrtc {
            if !starts_with_ci(url, "http://") && !starts_with_ci(url, "https://") {
                return "A WebRTC camera is its WHEP address, for example http://192.168.144.25:8889/cam/whep.";
            }
            return "";
        }
        if !has_scheme(url) {
            return "The address starts with rtsp://, for example rtsp://192.168.144.25:8554/main.264.";
        }
        ""
    }
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `/^[a-z][a-z0-9+.-]*:\/\//i`.
fn has_scheme(url: &str) -> bool {
    let mut chars = url.char_indices();
    match chars.next() {
        Some((_, c)) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for (i, c) in chars {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-') {
            continue;
        }
        return c == ':' && url[i..].starts_with("://");
    }
    false
}

/// Whether the decoder is older than the page may allow passwords through:
/// the web page's `oldDecoder`.
pub fn old_decoder(status: &Value) -> bool {
    let ff = status.get("ffmpeg").unwrap_or(&Value::Null);
    !text(ff, "path").is_empty() && ff.get("hides_password") == Some(&Value::Bool(false))
}

/// The version line of the decoder card.
pub fn ffmpeg_version(status: &Value) -> String {
    let ff = status.get("ffmpeg").unwrap_or(&Value::Null);
    if text(ff, "path").is_empty() {
        "not found".into()
    } else if text(ff, "version").is_empty() {
        "found".into()
    } else {
        text(ff, "version").into()
    }
}

/// The body of POST /api/video/settings.
pub fn settings_body(path: &str) -> Value {
    json!({ "ffmpeg": path.trim() })
}

/// What the notice says after the decoder path is saved.
pub fn settings_notice(path: &str) -> &'static str {
    if path.trim().is_empty() {
        "Corvus finds ffmpeg by itself now."
    } else {
        "Corvus decodes with that ffmpeg now."
    }
}

/// Whether the camera list needs the decoder at all: an empty list, or any
/// camera that is not WebRTC.
pub fn needs_decoder(streams: &[Value]) -> bool {
    streams.is_empty() || streams.iter().any(|s| text(s, "kind") != "webrtc")
}

/// The name on a row.
pub fn row_name(s: &Value) -> String {
    let n = text(s, "name");
    if !n.is_empty() {
        return n.into();
    }
    let a = text(s, "address");
    if a.is_empty() {
        "Camera".into()
    } else {
        a.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_words_and_levels() {
        assert_eq!(state_text("idle"), "Not open");
        assert_eq!(state_text("starting"), "Connecting");
        assert_eq!(state_text("live"), "Live");
        assert_eq!(state_text("stalled"), "No picture");
        assert_eq!(state_text("error"), "Offline");
        assert_eq!(state_text("unavailable"), "No decoder");
        assert_eq!(state_level("idle"), "off");
        assert_eq!(state_level("starting"), "warning");
        assert_eq!(state_level("live"), "healthy");
        assert_eq!(state_level("unavailable"), "critical");
        assert_eq!(known_state("nonsense"), "idle");
    }

    #[test]
    fn the_detail_line() {
        let live = json!({"state": "live", "width": 1280, "height": 720, "fps": 29.6, "address": "a"});
        assert_eq!(detail_line(&live), "1280 \u{d7} 720, 30 fps");
        let nofps = json!({"state": "live", "width": 640, "height": 480, "fps": 0});
        assert_eq!(detail_line(&nofps), "640 \u{d7} 480");
        let err = json!({"state": "error", "message": "Refused", "address": "rtsp://x"});
        assert_eq!(detail_line(&err), "Refused");
        let idle = json!({"state": "idle", "message": "ignored", "address": "rtsp://x"});
        assert_eq!(detail_line(&idle), "rtsp://x");
        let live_no_size = json!({"state": "live", "width": 0, "height": 0, "url": "rtsp://u"});
        assert_eq!(detail_line(&live_no_size), "rtsp://u");
        assert_eq!(detail_line(&json!({})), "");
    }

    #[test]
    fn a_webrtc_camera_reports_from_its_window() {
        let cam = json!({"id": "a", "kind": "webrtc", "state": "live", "address": "h"});
        let none = shown_state(&cam, None);
        assert_eq!(none["state"], "idle");
        assert_eq!(none["address"], "h");
        let local = Local {
            state: "error".into(),
            message: "No".into(),
            width: 0,
            height: 0,
            fps: 0.0,
        };
        assert_eq!(shown_state(&cam, Some(&local))["message"], "No");
        let rtsp = json!({"id": "b", "kind": "rtsp", "state": "live"});
        assert_eq!(shown_state(&rtsp, Some(&local)), rtsp);
    }

    #[test]
    fn the_save_body() {
        let mut d = Draft::blank();
        d.name = "  Gimbal ".into();
        d.url = " rtsp://1.2.3.4/x ".into();
        d.username = " me ".into();
        assert_eq!(
            d.save_body(),
            json!({"name": "Gimbal", "kind": "rtsp", "url": "rtsp://1.2.3.4/x", "username": "me", "transport": "tcp"})
        );
        d.id = "cam1".into();
        d.kind = Kind::Webrtc;
        d.transport = "udp".into();
        d.password = "pw".into();
        assert!(d.save_body().get("password").is_none());
        d.password_touched = true;
        let b = d.save_body();
        assert_eq!(b["id"], "cam1");
        assert_eq!(b["kind"], "webrtc");
        assert_eq!(b["password"], "pw");
        assert_eq!(b["transport"], "udp");
        d.password.clear();
        assert_eq!(d.save_body()["password"], "");
    }

    #[test]
    fn why_a_draft_cannot_be_saved() {
        let mut d = Draft::blank();
        assert_eq!(d.problem(), "Enter the camera's address.");
        d.url = "192.168.1.5/main".into();
        assert!(d.problem().starts_with("The address starts with rtsp://"));
        d.url = "RTSP://192.168.1.5/main".into();
        assert_eq!(d.problem(), "");
        d.url = "srt://host:9000".into();
        assert_eq!(d.problem(), "");
        d.url = "1rtsp://x".into();
        assert!(!d.problem().is_empty());
        d.kind = Kind::Webrtc;
        assert!(d.problem().starts_with("A WebRTC camera is its WHEP"));
        d.url = "HTTP://h:8889/cam/whep".into();
        assert_eq!(d.problem(), "");
        d.url = "https://h/whep".into();
        assert_eq!(d.problem(), "");
        d.url = "rtsp://h".into();
        assert!(!d.problem().is_empty());
    }

    #[test]
    fn a_draft_from_a_camera() {
        let c = json!({"id": "x", "name": "N", "kind": "webrtc", "url": "http://h/w", "username": "u",
            "has_password": true, "transport": "udp"});
        let d = Draft::from_stream(&c);
        assert_eq!(d.kind, Kind::Webrtc);
        assert!(d.has_password && !d.password_touched);
        assert_eq!(d.transport, "udp");
        assert_eq!(
            Draft::from_stream(&json!({"id": "y", "transport": "x"})).transport,
            "tcp"
        );
    }

    #[test]
    fn the_decoder_card() {
        let none = json!({"ffmpeg": {"path": ""}});
        assert_eq!(ffmpeg_version(&none), "not found");
        assert!(!old_decoder(&none));
        let old = json!({"ffmpeg": {"path": "/x/ffmpeg", "version": "4.4", "hides_password": false}});
        assert!(old_decoder(&old));
        assert_eq!(ffmpeg_version(&old), "4.4");
        let ok = json!({"ffmpeg": {"path": "/x/ffmpeg", "version": "", "hides_password": true}});
        assert!(!old_decoder(&ok));
        assert_eq!(ffmpeg_version(&ok), "found");
        assert_eq!(settings_body(" /opt/ff "), json!({"ffmpeg": "/opt/ff"}));
        assert_eq!(settings_notice(""), "Corvus finds ffmpeg by itself now.");
        assert_eq!(settings_notice("/x"), "Corvus decodes with that ffmpeg now.");
    }

    #[test]
    fn the_decoder_need_and_a_rows_name() {
        assert!(needs_decoder(&[]));
        assert!(!needs_decoder(&[json!({"kind": "webrtc"})]));
        assert!(needs_decoder(&[json!({"kind": "webrtc"}), json!({"kind": "rtsp"})]));
        assert_eq!(row_name(&json!({"name": "", "address": "a"})), "a");
        assert_eq!(row_name(&json!({})), "Camera");
    }
}
