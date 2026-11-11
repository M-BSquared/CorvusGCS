//! `routes/video.py`: the status the page polls, the camera list, the frame
//! long poll, the WebRTC signalling and the ffmpeg path.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::model::*;
use super::service::{Frame, FrameError, VideoService, WhepResult};
use crate::core::Core;
use crate::module::Registry;
use crate::peripherals::util::py_strip;
use crate::peripherals::Peripherals;
use crate::request::Request;
use crate::response::Response;

pub fn register(reg: &mut Registry) {
    reg.config_block("video", config_block);
    reg.get("/api/video/status", api_status);
    reg.get("/api/video/frame", api_frame);
    reg.post("/api/video/streams", api_streams_upsert);
    reg.post("/api/video/streams/remove", api_streams_remove);
    reg.post("/api/video/webrtc/offer", api_webrtc_offer);
    reg.post("/api/video/webrtc/close", api_webrtc_close);
    reg.post("/api/video/settings", api_settings);
}

fn service(core: &Core) -> Option<Arc<VideoService>> {
    core.service::<Peripherals>().and_then(|p| p.video())
}

fn stored(core: &Core) -> Value {
    settings(core.settings().get().get("video").unwrap_or(&Value::Null))
}

/// The page's whole state, or an unavailable one without a service.
fn video_status(core: &Core) -> Value {
    match service(core) {
        Some(svc) => svc.status(),
        None => {
            let streams: Vec<Value> = stored(core)["streams"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|s| {
                    let mut p = public_stream(s).as_object().cloned().unwrap_or_default();
                    p.insert("state".into(), json!(STATE_UNAVAILABLE));
                    p.insert("message".into(), json!(""));
                    Value::Object(p)
                })
                .collect();
            json!({
                "available": false,
                "reason": "Video is unavailable in this build.",
                "ffmpeg": {"path": "", "source": "", "version": "", "configured": ""},
                "streams": streams,
                "max_streams": MAX_STREAMS,
                "schemes": SCHEMES,
                "transports": TRANSPORTS,
            })
        }
    }
}

fn api_status(core: &Core, _req: &Request) -> Response {
    Response::json(video_status(core))
}

/// `int(text)` as the frame number a window sends, 0 when it is not one.
fn parse_after(text: &str) -> u64 {
    let t = py_strip(text).replace('_', "");
    t.parse::<i64>().map(|n| n.max(0) as u64).unwrap_or(0)
}

fn api_frame(core: &Core, req: &Request) -> Response {
    if req
        .headers
        .get("Sec-Fetch-Site")
        .is_some_and(|v| v.to_lowercase() == "cross-site")
    {
        return Response::error(403, "cross-site request forbidden");
    }
    let id = req.query.get("id").unwrap_or("").to_string();
    let after = parse_after(req.query.get("after").unwrap_or("0"));
    let Some(svc) = service(core) else {
        return Response::json_status(
            json!({"state": STATE_UNAVAILABLE, "seq": 0, "message": "Video is unavailable in this build."}),
            503,
        );
    };
    let stopping = || core.is_stopping();
    match svc.frame(&id, after, svc.timing().frame_wait, &stopping) {
        Err(FrameError::NoSuchCamera) => Response::error(404, "no such camera"),
        Err(FrameError::WrongKind(m)) => Response::error(409, &m),
        Ok(Frame::State { state, message, seq }) => {
            Response::json(json!({"state": state, "message": message, "seq": seq}))
        }
        Ok(Frame::Jpeg { seq, data }) => {
            if data.is_empty() {
                return Response::json(json!({"state": STATE_IDLE, "message": "", "seq": seq}));
            }
            Response::bytes(200, "image/jpeg", data.as_ref().clone())
                .with_header("Cache-Control", "no-store")
                .with_header("X-Video-Seq", &seq.to_string())
        }
    }
}

fn save(core: &Core, video: &Value) -> Value {
    let block = video.clone();
    let _ = core.settings().update(core, |cfg| cfg.set("video", Some(block)));
    video.clone()
}

fn apply(core: &Core, video: &Value) {
    if let Some(svc) = service(core) {
        svc.apply_settings(video);
    }
}

/// Held across a whole read-modify-write of the camera list, so two saves
/// cannot interleave (`_config_write_lock`).
static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn config_guard(_core: &Core) -> std::sync::MutexGuard<'static, ()> {
    CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn str_field<'a>(payload: &'a Map<String, Value>, key: &str) -> Result<&'a str, ()> {
    match payload.get(key) {
        None => Ok(""),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(()),
    }
}

fn api_streams_upsert(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let (Ok(name), Ok(url), Ok(username)) = (
        str_field(payload, "name"),
        str_field(payload, "url"),
        str_field(payload, "username"),
    ) else {
        return Response::not_ok(400, "name, url and username must be strings");
    };
    if py_strip(name).chars().count() > MAX_NAME_LEN {
        return Response::not_ok(400, "The name is too long.");
    }
    let kind = match payload.get("kind") {
        None => KIND_RTSP,
        Some(Value::String(k)) if KINDS.contains(&k.as_str()) => k.as_str(),
        Some(_) => return Response::not_ok(400, "kind must be rtsp or webrtc"),
    };
    let (clean_url, url_user, url_password) = split_credentials(url);
    let problem = url_problem(&clean_url, kind);
    if !problem.is_empty() {
        return Response::not_ok(400, &problem);
    }
    let transport = match payload.get("transport") {
        None => DEFAULT_TRANSPORT,
        Some(Value::String(t)) if TRANSPORTS.contains(&t.as_str()) => t.as_str(),
        Some(_) => return Response::not_ok(400, "transport must be tcp or udp"),
    };
    let stream_id = payload.get("id").and_then(Value::as_str).unwrap_or("").to_string();

    let (entry, warning, video) = {
        let _held = config_guard(core);
        let current = stored(core);
        let mut streams: Vec<Value> = current["streams"].as_array().cloned().unwrap_or_default();
        let existing = if stream_id.is_empty() {
            None
        } else {
            streams.iter().find(|s| s["id"] == json!(stream_id)).cloned()
        };
        if !stream_id.is_empty() && existing.is_none() {
            return Response::not_ok(404, "That camera is no longer saved.");
        }
        if existing.is_none() && streams.len() >= MAX_STREAMS {
            return Response::not_ok(400, &format!("At most {MAX_STREAMS} cameras can be saved."));
        }
        let mut warning = String::new();
        let mut password = if payload.contains_key("password") {
            payload["password"].as_str().unwrap_or("").to_string()
        } else {
            let mut kept = existing
                .as_ref()
                .and_then(|e| e["password"].as_str())
                .unwrap_or("")
                .to_string();
            if !kept.is_empty()
                && !same_origin(
                    existing.as_ref().and_then(|e| e["url"].as_str()).unwrap_or(""),
                    &clean_url,
                )
            {
                kept.clear();
                warning = "The camera moved to another host, so its password was not kept. Enter it again if the new one needs it.".into();
            }
            kept
        };
        let mut username = username.to_string();
        if !url_user.is_empty() {
            username = url_user.clone();
            if !url_password.is_empty() {
                password = url_password.clone();
            }
        }
        let id = existing
            .as_ref()
            .and_then(|e| e["id"].as_str())
            .map(str::to_string)
            .unwrap_or_else(new_stream_id);
        let Some(entry) = coerce_stream(&json!({
            "id": id,
            "name": name,
            "url": clean_url,
            "username": py_strip(&username),
            "password": password,
            "transport": transport,
            "kind": kind,
        })) else {
            return Response::not_ok(400, "That camera cannot be saved.");
        };
        if existing.is_some() {
            for s in streams.iter_mut() {
                if s["id"] == entry["id"] {
                    *s = entry.clone();
                }
            }
        } else {
            streams.push(entry.clone());
        }
        let video = save(
            core,
            &settings(&json!({"streams": streams, "ffmpeg": current["ffmpeg"]})),
        );
        (entry, warning, video)
    };
    apply(core, &video);
    let mut answer = json!({"ok": true, "id": entry["id"], "status": video_status(core)});
    if !warning.is_empty() {
        answer["warning"] = json!(warning);
    }
    Response::json(answer)
}

fn api_streams_remove(core: &Core, req: &Request) -> Response {
    let id = match req.payload().get("id") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::not_ok(400, "id must be a non-empty string"),
    };
    let video = {
        let _held = config_guard(core);
        let current = stored(core);
        let streams: Vec<Value> = current["streams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s["id"] != json!(id))
            .collect();
        save(
            core,
            &settings(&json!({"streams": streams, "ffmpeg": current["ffmpeg"]})),
        )
    };
    apply(core, &video);
    Response::json(json!({"ok": true, "status": video_status(core)}))
}

fn api_webrtc_offer(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let (Some(Value::String(id)), Some(Value::String(sdp))) = (payload.get("id"), payload.get("sdp")) else {
        return Response::not_ok(400, "id and sdp must be strings");
    };
    if id.is_empty() {
        return Response::not_ok(400, "id and sdp must be strings");
    }
    let Some(svc) = service(core) else {
        return Response::not_ok(503, "Video is unavailable in this build.");
    };
    match svc.whep_offer(id, sdp) {
        WhepResult::NoSuchCamera => Response::not_ok(404, "no such camera"),
        WhepResult::WrongKind(m) => Response::not_ok(409, &m),
        WhepResult::Failed { status, error } => Response::not_ok(status, &error),
        WhepResult::Ok { sdp, session } => Response::json(json!({"ok": true, "sdp": sdp, "session": session})),
    }
}

fn api_webrtc_close(core: &Core, req: &Request) -> Response {
    let token = match req.payload().get("session") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::not_ok(400, "session must be a non-empty string"),
    };
    let closed = service(core).is_some_and(|s| s.whep_close(&token));
    Response::json(json!({"ok": true, "closed": closed}))
}

fn api_settings(core: &Core, req: &Request) -> Response {
    let path = match req.payload().get("ffmpeg") {
        None => String::new(),
        Some(Value::String(s)) => py_strip(s).to_string(),
        Some(_) => return Response::not_ok(400, "ffmpeg must be a string"),
    };
    if !path.is_empty() && super::ffmpeg::resolve_ffmpeg(&path).0.is_empty() {
        return Response::not_ok(400, "There is no program at that path that can be run.");
    }
    let video = {
        let _held = config_guard(core);
        let current = stored(core);
        save(core, &settings(&json!({"streams": current["streams"], "ffmpeg": path})))
    };
    apply(core, &video);
    Response::json(json!({"ok": true, "status": video_status(core)}))
}
