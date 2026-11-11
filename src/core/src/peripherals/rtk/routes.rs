//! `routes/rtk.py`: the status the page polls, the settings form and Restart
//! survey.

use serde_json::{json, Value};

use super::config::{self, DEFAULT_SURVEY_ACCURACY_M, DEFAULT_SURVEY_DURATION_S};
use crate::core::Core;
use crate::module::Registry;
use crate::peripherals::Peripherals;
use crate::request::Request;
use crate::response::Response;

pub fn register(reg: &mut Registry) {
    reg.config_block("rtk", |v| v.as_object().map(|_| config::settings(v)));
    reg.get("/api/rtk/status", api_status);
    reg.post("/api/rtk/settings", api_settings);
    reg.post("/api/rtk/restart", api_restart);
}

/// The answer while there is no service: off, with the reason on it.
fn unavailable_status() -> Value {
    json!({
        "enabled": false,
        "state": super::service::STATE_OFF,
        "message": "RTK corrections are unavailable in this build",
        "error": "", "warning": "",
        "source": "usb", "mode": "survey", "device": "", "baud": 0,
        "receiver": null, "receiver_label": "",
        "survey": null, "survey_progress": 0,
        "survey_target": {"accuracy": DEFAULT_SURVEY_ACCURACY_M, "duration": DEFAULT_SURVEY_DURATION_S},
        "frames": 0, "frame_bytes": 0, "crc_errors": 0,
        "source_age": null, "uptime": null, "messages": {},
        "injected": {"bytes": 0, "messages": 0, "dropped": 0, "age": null},
        "link_ready": false,
        "vehicle": {"fix": "", "satellites": 0, "hdop": 0},
        "ports": [],
        "settings": config::defaults(),
        "defaults": config::defaults(),
    })
}

fn api_status(core: &Core, _req: &Request) -> Response {
    match core.service::<Peripherals>().and_then(|p| p.rtk()) {
        Some(rtk) => Response::json(rtk.status()),
        None => Response::json(unavailable_status()),
    }
}

fn api_settings(core: &Core, req: &Request) -> Response {
    let stored = config::settings(core.settings().get().get("rtk").unwrap_or(&Value::Null));
    let mut incoming = req.payload().clone();
    if let Some(Value::Object(ntrip)) = incoming.get("ntrip") {
        let blank = ntrip
            .get("password")
            .map(|p| match p {
                Value::Null => true,
                Value::String(s) => crate::peripherals::util::py_strip(s).is_empty(),
                other => !crate::protocol::common::py_truthy(other) || other.to_string().trim().is_empty(),
            })
            .unwrap_or(true);
        if blank {
            let mut ntrip = ntrip.clone();
            let kept = stored
                .get("ntrip")
                .and_then(|n| n.get("password"))
                .cloned()
                .unwrap_or(Value::from(""));
            ntrip.insert("password".into(), kept);
            incoming.insert("ntrip".into(), Value::Object(ntrip));
        }
    }
    let resolved = config::settings(&Value::Object(incoming));

    // Refused before anything is stored, so a form that cannot work is not
    // saved and then reported as broken on every restart.
    let source = resolved.get("source").and_then(Value::as_str).unwrap_or("usb");
    let mode = resolved.get("mode").and_then(Value::as_str).unwrap_or("survey");
    if source == "ntrip" {
        let ntrip = resolved
            .get("ntrip")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let problem = config::ntrip_problem(&ntrip);
        if !problem.is_empty() {
            return Response::not_ok(400, problem);
        }
    }
    if source == "usb" && mode == "fixed" {
        let fixed = resolved
            .get("fixed")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let problem = config::fixed_position_problem(&fixed);
        if !problem.is_empty() {
            return Response::not_ok(400, problem);
        }
    }

    let mut warning = String::new();
    let block = resolved.clone();
    if core
        .settings()
        .update(core, |cfg| cfg.set("rtk", Some(Value::Object(block))))
        .is_err()
    {
        warning = "set for this session but not saved".into();
    }

    let Some(rtk) = core.service::<Peripherals>().and_then(|p| p.rtk()) else {
        return Response::json(json!({
            "ok": true,
            "warning": if warning.is_empty() { "RTK corrections are unavailable in this build".to_string() } else { warning },
        }));
    };
    rtk.apply_settings(&Value::Object(resolved));
    let mut answer = json!({"ok": true, "status": rtk.status()});
    if !warning.is_empty() {
        answer["warning"] = Value::from(warning);
    }
    Response::json(answer)
}

fn api_restart(core: &Core, _req: &Request) -> Response {
    let Some(rtk) = core.service::<Peripherals>().and_then(|p| p.rtk()) else {
        return Response::not_ok(503, "RTK corrections are unavailable");
    };
    rtk.restart_survey();
    Response::json(json!({"ok": true, "status": rtk.status()}))
}
