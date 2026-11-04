//! What the PID Tuning page knows apart from drawing it
//! (`python/src/js/setup-tuning.js`): the description the backend sends, when an
//! autotune can start and what stands in its way, the autotune's phase, and
//! the sample buffers behind the graphs. Every rule has the web page's
//! expected text or value in a test.

use serde_json::Value;

use crate::setup::schema_form::{parse_fields, Field, Kind};

/// Rate the setpoint messages are asked for while the page is open: fast
/// enough that a step response is visible, slow enough for a telemetry radio.
pub const STREAM_HZ: u32 = 20;
/// `MAX_POINTS` and `REDRAW_MIN_MS` of setup-shared.js.
pub const MAX_POINTS: usize = 600;

pub const REBOOT_TEXT_OFF: &str = "The autotune module is switched off on this autopilot. Switch it on in the autotune settings below, then reboot the autopilot before starting a tune.";
pub const REBOOT_TEXT_PENDING: &str =
    "The autotune module was switched on. Reboot the autopilot now: it only starts after a restart.";
pub const DEFAULT_RUNNING_TEXT: &str =
    "The autopilot is tuning. Keep your hands off the sticks unless the vehicle misbehaves.";
pub const DEFAULT_DONE_TEXT: &str = "The tune finished. Test the new gains gently before flying normally.";
pub const FAILED_TEXT: &str = "The tune did not finish. The console has the autopilot's reason. If the module was only just switched on, reboot the autopilot and try again.";

/// How a loop is covered by the autotune: `run` starts this loop's tune,
/// `with` starts the run it is tuned in, `manual` no autotune covers it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tune {
    pub mode: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChartSpec {
    pub id: String,
    pub title: String,
    pub unit: String,
    pub actual: String,
    pub setpoint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Section {
    pub title: String,
    pub fields: Vec<Field>,
}

/// Which axes the next tune covers.
#[derive(Debug, Clone, Default)]
pub struct Axes {
    pub selectable: bool,
    pub label: String,
    pub labels: Vec<String>,
    pub reason: String,
    pub field: Option<Field>,
}

#[derive(Debug, Clone, Default)]
pub struct Group {
    pub id: String,
    pub title: String,
    pub hint: String,
    pub autotune: bool,
    pub tune: Tune,
    pub charts: Vec<ChartSpec>,
    pub sections: Vec<Section>,
    pub label: String,
    pub enabled: Option<bool>,
    pub enable_param: String,
    pub axes: Option<Axes>,
    pub progress: bool,
    pub running_text: String,
    pub done_text: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub connected: bool,
    pub error: String,
    pub received: i64,
    pub groups: Vec<Group>,
}

pub fn parse_doc(v: &Value) -> Doc {
    let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let groups = v
        .get("groups")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|g| {
                    let tune = g.get("tune").cloned().unwrap_or(Value::Null);
                    let axes = g.get("axes").filter(|a| a.is_object()).map(|a| Axes {
                        selectable: a.get("selectable") == Some(&Value::Bool(true)),
                        label: s(a, "label"),
                        labels: a
                            .get("labels")
                            .and_then(Value::as_array)
                            .map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        reason: s(a, "reason"),
                        field: a
                            .get("field")
                            .filter(|f| f.is_object())
                            .and_then(|f| parse_fields(&Value::Array(vec![f.clone()])).into_iter().next()),
                    });
                    Group {
                        id: s(g, "id"),
                        title: s(g, "title"),
                        hint: s(g, "hint"),
                        autotune: g.get("kind").and_then(Value::as_str) == Some("autotune"),
                        tune: Tune {
                            mode: if s(&tune, "mode").is_empty() {
                                "manual".into()
                            } else {
                                s(&tune, "mode")
                            },
                            text: s(&tune, "text"),
                        },
                        charts: g
                            .get("charts")
                            .and_then(Value::as_array)
                            .map(|c| {
                                c.iter()
                                    .map(|c| ChartSpec {
                                        id: s(c, "id"),
                                        title: s(c, "title"),
                                        unit: s(c, "unit"),
                                        actual: s(c, "actual"),
                                        setpoint: c
                                            .get("setpoint")
                                            .and_then(Value::as_str)
                                            .filter(|x| !x.is_empty())
                                            .map(String::from),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        sections: g
                            .get("sections")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .map(|sec| Section {
                                        title: s(sec, "title"),
                                        fields: parse_fields(sec.get("fields").unwrap_or(&Value::Null)),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        label: if s(g, "label").is_empty() {
                            "Autotune".into()
                        } else {
                            s(g, "label")
                        },
                        enabled: g.get("enabled").and_then(Value::as_bool),
                        enable_param: s(g, "enable_param"),
                        axes,
                        progress: g.get("progress") != Some(&Value::Bool(false)),
                        running_text: s(g, "running_text"),
                        done_text: s(g, "done_text"),
                        steps: g
                            .get("steps")
                            .and_then(Value::as_array)
                            .map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Doc {
        connected: v.get("connected") == Some(&Value::Bool(true)),
        error: s(v, "error"),
        received: v.get("received").and_then(Value::as_i64).unwrap_or(0),
        groups,
    }
}

impl Doc {
    /// Every parameter a check reads back.
    pub fn check_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for g in &self.groups {
            let axes = g.axes.iter().filter_map(|a| a.field.as_ref());
            for f in g.sections.iter().flat_map(|s| s.fields.iter()).chain(axes) {
                if !names.contains(&f.param) {
                    names.push(f.param.clone());
                }
            }
        }
        names
    }
}

/// True where PX4 will accept an autotune: armed, and not reported as sitting
/// on the ground. An unknown landed state (0, no EXTENDED_SYS_STATE) is not
/// grounded; the backend applies the same rule and PX4 remains the authority.
pub fn is_flying(connected: bool, armed: bool, landed: i64) -> bool {
    connected && armed && landed != 1
}

/// The flight chip's word.
pub fn flight_label(armed: bool, landed: i64) -> &'static str {
    if !armed {
        "Disarmed"
    } else if landed == 1 {
        "Armed, on the ground"
    } else if landed == 2 {
        "Flying"
    } else {
        "Armed"
    }
}

/// `AUTOTUNE_PHASE_TEXT`.
pub fn phase_text(phase: &str) -> String {
    match phase {
        "" => "Idle".into(),
        "running" => "Tuning".into(),
        "done" => "Complete".into(),
        "failed" => "Failed".into(),
        other => other.into(),
    }
}

/// True when the axis selection names none of the axes it offers, which would
/// start a tune with nothing to tune.
pub fn no_axis_selected(field: Option<&Field>) -> bool {
    let Some(f) = field else { return false };
    let Kind::Bitmask(bits) = &f.kind else { return false };
    let value = f.value.max(0.0) as u64;
    let offered = bits.iter().fold(0u64, |m, (b, _)| m | (1u64 << b));
    offered != 0 && value & offered == 0
}

/// "0:42", "12:05".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).floor() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The reboot the autotune is waiting for, or empty when none is.
pub fn reboot_reason(restart_pending: bool, g: &Group) -> &'static str {
    if restart_pending {
        REBOOT_TEXT_PENDING
    } else if g.enabled == Some(false) && !g.enable_param.is_empty() {
        REBOOT_TEXT_OFF
    } else {
        ""
    }
}

/// Why the tune cannot start right now, or empty when it can. The gate names
/// the missing precondition: "cannot autotune" would tell an operator standing
/// in a field nothing they can act on.
pub fn gate_text(connected: bool, armed: bool, landed: i64, no_axis: bool) -> &'static str {
    if !connected {
        "No link to the vehicle. Connect before autotuning."
    } else if no_axis {
        if armed {
            "No axis is selected. Land and disarm to tick at least one."
        } else {
            "No axis is selected. Tick at least one above before taking off."
        }
    } else if !armed {
        "The autotune runs in flight. Arm the vehicle and take off first."
    } else if landed == 1 {
        "The vehicle is still on the ground. Hold a stable hover, then start the tune."
    } else {
        ""
    }
}

/// What the run strip says for a phase.
pub fn run_message(phase: &str, g: &Group) -> String {
    match phase {
        "running" => {
            if g.running_text.is_empty() {
                DEFAULT_RUNNING_TEXT.into()
            } else {
                g.running_text.clone()
            }
        }
        "done" => {
            if g.done_text.is_empty() {
                DEFAULT_DONE_TEXT.into()
            } else {
                g.done_text.clone()
            }
        }
        "failed" => FAILED_TEXT.into(),
        _ => "No tune is running.".into(),
    }
}

/// What the bar shows: its share, whether it only says "running" (no figure
/// to invent), and the percent text.
pub fn bar_share(phase: &str, has_progress: bool, progress: f64) -> (f64, bool, String) {
    let running = phase == "running";
    let p = progress.clamp(0.0, 100.0);
    let share = if running {
        if has_progress {
            p
        } else {
            100.0
        }
    } else if phase == "done" {
        100.0
    } else {
        0.0
    };
    let text = if running && has_progress {
        format!("{}%", crate::data::units::js_number(crate::data::units::js_round(p)))
    } else {
        String::new()
    };
    (share, running && !has_progress, text)
}

/// The side card's status word.
pub fn side_status(phase: &str, has_progress: bool, progress: f64) -> String {
    match phase {
        "running" => {
            if has_progress {
                format!(
                    "Tuning {}%",
                    crate::data::units::js_number(crate::data::units::js_round(progress.clamp(0.0, 100.0)))
                )
            } else {
                "Tuning".into()
            }
        }
        "done" => "Complete".into(),
        "failed" => "Failed".into(),
        _ => String::new(),
    }
}

/// A graph's samples: time, setpoint and response, the last `MAX_POINTS`.
#[derive(Debug, Clone, Default)]
pub struct Buf {
    pub t: Vec<f64>,
    pub sp: Vec<Option<f64>>,
    pub actual: Vec<f64>,
}

impl Buf {
    /// Adds one telemetry frame; a frame without a finite response is skipped,
    /// a missing setpoint is a gap.
    pub fn push(&mut self, spec: &ChartSpec, state: &Value, t: f64) {
        let n = |k: &str| state.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
        let Some(actual) = n(&spec.actual) else { return };
        self.t.push(t);
        self.actual.push(actual);
        self.sp.push(spec.setpoint.as_deref().and_then(n));
        if self.t.len() > MAX_POINTS {
            self.t.remove(0);
            self.actual.remove(0);
            self.sp.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_autotune_needs_the_air_and_an_unknown_landed_state_is_not_the_ground() {
        assert!(is_flying(true, true, 0));
        assert!(is_flying(true, true, 2));
        assert!(!is_flying(true, true, 1));
        assert!(!is_flying(true, false, 2));
        assert!(!is_flying(false, true, 2));
        assert_eq!(flight_label(false, 0), "Disarmed");
        assert_eq!(flight_label(true, 1), "Armed, on the ground");
        assert_eq!(flight_label(true, 2), "Flying");
        assert_eq!(flight_label(true, 0), "Armed");
    }

    #[test]
    fn the_gate_names_the_missing_precondition_in_the_web_pages_order() {
        assert_eq!(
            gate_text(false, true, 2, true),
            "No link to the vehicle. Connect before autotuning."
        );
        assert_eq!(
            gate_text(true, false, 0, true),
            "No axis is selected. Tick at least one above before taking off."
        );
        assert_eq!(
            gate_text(true, true, 2, true),
            "No axis is selected. Land and disarm to tick at least one."
        );
        assert_eq!(
            gate_text(true, false, 0, false),
            "The autotune runs in flight. Arm the vehicle and take off first."
        );
        assert_eq!(
            gate_text(true, true, 1, false),
            "The vehicle is still on the ground. Hold a stable hover, then start the tune."
        );
        assert_eq!(gate_text(true, true, 2, false), "");
    }

    fn axis_field(value: f64) -> Field {
        Field {
            param: "FW_AT_AXES".into(),
            label: "Axes".into(),
            kind: Kind::Bitmask(vec![(0, "Roll".into()), (1, "Pitch".into()), (2, "Yaw".into())]),
            value,
            unit: String::new(),
            hint: String::new(),
            min: None,
            max: None,
            also: vec![],
        }
    }

    #[test]
    fn a_selection_of_no_axis_is_caught_and_a_fixed_selection_never_is() {
        assert!(no_axis_selected(Some(&axis_field(0.0))));
        assert!(
            no_axis_selected(Some(&axis_field(8.0))),
            "a bit the field does not offer is no axis"
        );
        assert!(!no_axis_selected(Some(&axis_field(2.0))));
        assert!(!no_axis_selected(None));
    }

    #[test]
    fn the_reboot_the_module_waits_for_comes_before_any_other_gate() {
        let mut g = Group {
            enabled: Some(false),
            enable_param: "MC_AT_EN".into(),
            ..Group::default()
        };
        assert_eq!(reboot_reason(false, &g), REBOOT_TEXT_OFF);
        assert_eq!(reboot_reason(true, &g), REBOOT_TEXT_PENDING);
        g.enabled = Some(true);
        assert_eq!(reboot_reason(false, &g), "");
        g.enabled = None;
        assert_eq!(
            reboot_reason(false, &g),
            "",
            "a stack with no module switch has nothing to reboot for"
        );
    }

    #[test]
    fn the_bar_claims_no_share_the_autopilot_did_not_give() {
        assert_eq!(bar_share("running", true, 42.4), (42.4, false, "42%".into()));
        assert_eq!(bar_share("running", false, 0.0), (100.0, true, String::new()));
        assert_eq!(bar_share("done", true, 0.0), (100.0, false, String::new()));
        assert_eq!(bar_share("", true, 55.0), (0.0, false, String::new()));
        assert_eq!(bar_share("running", true, 180.0).0, 100.0);
        assert_eq!(side_status("running", true, 7.6), "Tuning 8%");
        assert_eq!(side_status("running", false, 7.6), "Tuning");
        assert_eq!(side_status("done", true, 0.0), "Complete");
        assert_eq!(side_status("", true, 0.0), "");
        assert_eq!(phase_text(""), "Idle");
        assert_eq!(phase_text("done"), "Complete");
        assert_eq!(clock(42.9), "0:42");
        assert_eq!(clock(725.0), "12:05");
        assert_eq!(clock(-3.0), "0:00");
    }

    #[test]
    fn the_run_strip_says_what_the_stack_says_or_the_default() {
        let g = Group {
            running_text: "Exciting each axis.".into(),
            ..Group::default()
        };
        assert_eq!(run_message("running", &g), "Exciting each axis.");
        assert_eq!(run_message("running", &Group::default()), DEFAULT_RUNNING_TEXT);
        assert_eq!(run_message("done", &Group::default()), DEFAULT_DONE_TEXT);
        assert_eq!(run_message("failed", &g), FAILED_TEXT);
        assert_eq!(run_message("", &g), "No tune is running.");
    }

    #[test]
    fn the_description_is_read_loops_charts_and_the_autotune() {
        let d = parse_doc(&json!({
        "connected": true, "received": 47,
        "groups": [
            {"id": "rate", "title": "Rate Controller", "hint": "h", "kind": "fields",
             "tune": {"mode": "run", "text": "t"},
             "charts": [{"id": "rollrate", "title": "Roll rate", "unit": "deg/s", "actual": "rollspeed", "setpoint": "rollspeed_sp"},
                        {"id": "alt", "title": "Height", "unit": "m", "actual": "altitude_agl", "setpoint": null}],
             "sections": [{"id": "roll", "title": "Roll", "fields": [{"param": "MC_ROLLRATE_P", "kind": "number", "value": 0.15}]}]},
            {"id": "autotune", "title": "Autotune", "kind": "autotune", "label": "Multicopter autotune",
             "enabled": false, "enable_param": "MC_AT_EN", "progress": true, "steps": ["a", "b"],
             "axes": {"selectable": false, "label": "Axes to tune", "labels": ["Roll", "Pitch"], "reason": "r"},
             "sections": [{"id": "settings", "title": "Autotune settings", "fields": [{"param": "MC_AT_EN", "kind": "enum", "value": 0, "options": []}]}]}
        ]}));
        assert!(d.connected);
        assert_eq!(d.groups.len(), 2);
        assert_eq!(d.groups[0].tune.mode, "run");
        assert_eq!(d.groups[0].charts[1].setpoint, None, "no setpoint, no second line");
        assert_eq!(d.groups[0].charts[0].setpoint.as_deref(), Some("rollspeed_sp"));
        assert!(d.groups[1].autotune);
        assert_eq!(d.groups[1].enabled, Some(false));
        assert_eq!(d.groups[1].axes.as_ref().unwrap().labels, ["Roll", "Pitch"]);
        assert_eq!(d.groups[1].steps.len(), 2);
        assert_eq!(d.groups[1].tune.mode, "manual", "no tune object means by hand");
        assert_eq!(d.check_names(), ["MC_ROLLRATE_P", "MC_AT_EN"]);
    }

    #[test]
    fn a_graph_keeps_the_last_600_samples_and_a_missing_setpoint_is_a_gap() {
        let spec = ChartSpec {
            id: "r".into(),
            title: "Roll".into(),
            unit: "deg".into(),
            actual: "roll".into(),
            setpoint: Some("roll_sp".into()),
        };
        let mut b = Buf::default();
        b.push(&spec, &json!({"roll": 1.5}), 0.0);
        assert_eq!((b.t.len(), b.sp[0]), (1, None));
        b.push(&spec, &json!({"roll": 2.5, "roll_sp": 3.0}), 0.05);
        assert_eq!(b.sp[1], Some(3.0));
        b.push(&spec, &json!({"pitch": 9.0}), 0.1);
        assert_eq!(b.t.len(), 2, "no response, no sample");
        for i in 0..700 {
            b.push(&spec, &json!({"roll": f64::from(i)}), f64::from(i));
        }
        assert_eq!(b.t.len(), MAX_POINTS);
        assert_eq!(*b.actual.last().unwrap(), 699.0);
        assert_eq!(b.actual[0], 100.0);
    }
}
