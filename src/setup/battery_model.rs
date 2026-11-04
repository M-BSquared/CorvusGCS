//! What the Battery & Power page knows apart from drawing it
//! (`python/src/js/setup-battery.js`): the description the backend sends, the
//! estimator settings and their defaults, where a failsafe level sits on the
//! pack, the flags that label those levels, and the readout under the
//! drawing. Every rule has the web page's expected value in a test.

use serde_json::{json, Value};

use crate::data::units::{js_round, js_to_fixed, Units};
use crate::setup::schema_form::{parse_fields, Field};

/// Above this many cells the per cell labels stop being readable.
pub const MAX_LABELLED_CELLS: usize = 8;
pub const FIGURE_WIDTH: f32 = 260.0;
pub const FIGURE_HEIGHT: f32 = 132.0;

/// Threshold label geometry in drawing units (`FLAG` of the web page).
pub const FLAG_GAP: f32 = 3.0;
pub const FLAG_PAD: f32 = 2.0;
pub const FLAG_PITCH: f32 = 10.0;
pub const FLAG_CAP: f32 = 6.5;

/// The estimator settings this page owns and what an unset one means.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub estimate: bool,
    pub chemistry: String,
    pub cells: f64,
    pub full_cell: f64,
    pub empty_cell: f64,
    pub resistance: f64,
    pub capacity_mah: f64,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            estimate: false,
            chemistry: "lipo".into(),
            cells: 0.0,
            full_cell: 0.0,
            empty_cell: 0.0,
            resistance: 0.0,
            capacity_mah: 0.0,
        }
    }
}

impl Settings {
    /// `Object.assign({}, SETTING_DEFAULTS, saved)`.
    pub fn from_value(v: &Value) -> Settings {
        let mut s = Settings::default();
        let n = |k: &str| v.get(k).and_then(Value::as_f64);
        if let Some(b) = v.get("estimate") {
            s.estimate = truthy(b);
        }
        if let Some(c) = v.get("chemistry").and_then(Value::as_str) {
            s.chemistry = c.to_string();
        }
        s.cells = n("cells").unwrap_or(s.cells);
        s.full_cell = n("full_cell").unwrap_or(s.full_cell);
        s.empty_cell = n("empty_cell").unwrap_or(s.empty_cell);
        s.resistance = n("resistance").unwrap_or(s.resistance);
        s.capacity_mah = n("capacity_mah").unwrap_or(s.capacity_mah);
        s
    }

    /// One number setting by its key.
    pub fn number(&self, key: &str) -> f64 {
        match key {
            "cells" => self.cells,
            "full_cell" => self.full_cell,
            "empty_cell" => self.empty_cell,
            "resistance" => self.resistance,
            "capacity_mah" => self.capacity_mah,
            _ => 0.0,
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chemistry {
    pub value: String,
    pub label: String,
    pub full: f64,
    pub empty: f64,
    pub nominal: f64,
}

/// A failsafe level: a fraction of the pack (PX4) or volts (ArduPilot).
#[derive(Debug, Clone, PartialEq)]
pub struct Threshold {
    pub id: String,
    pub label: String,
    pub percent: Option<f64>,
    pub volts: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pack {
    pub cells: f64,
    pub capacity_mah: f64,
    pub full_cell: f64,
    pub empty_cell: f64,
    pub resistance_ohm: f64,
    pub thresholds: Vec<Threshold>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub hint: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub connected: bool,
    pub error: String,
    pub received: i64,
    pub sections: Vec<Section>,
    pub pack: Pack,
    pub chemistries: Vec<Chemistry>,
    pub configured: Settings,
}

pub fn parse_doc(v: &Value) -> Doc {
    let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let f = |o: &Value, k: &str| o.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let pack = v.get("pack").cloned().unwrap_or(Value::Null);
    Doc {
        connected: v.get("connected").is_some_and(truthy),
        error: s(v, "error"),
        received: v.get("received").and_then(Value::as_i64).unwrap_or(0),
        sections: v
            .get("sections")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|sec| Section {
                        id: s(sec, "id"),
                        title: s(sec, "title"),
                        hint: s(sec, "hint"),
                        fields: parse_fields(sec.get("fields").unwrap_or(&Value::Null)),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        pack: Pack {
            cells: f(&pack, "cells"),
            capacity_mah: f(&pack, "capacity_mah"),
            full_cell: f(&pack, "full_cell"),
            empty_cell: f(&pack, "empty_cell"),
            resistance_ohm: f(&pack, "resistance_ohm"),
            thresholds: pack
                .get("thresholds")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|t| Threshold {
                            id: s(t, "id"),
                            label: s(t, "label"),
                            percent: t.get("percent").and_then(Value::as_f64),
                            volts: t.get("volts").and_then(Value::as_f64),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        chemistries: v
            .get("chemistries")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|c| Chemistry {
                        value: s(c, "value"),
                        label: s(c, "label"),
                        full: f(c, "full"),
                        empty: f(c, "empty"),
                        nominal: f(c, "nominal"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        configured: Settings::from_value(v.get("configured").unwrap_or(&Value::Null)),
    }
}

/// The chemistry the operator picked, else the first listed, else a LiPo.
pub fn chemistry(list: &[Chemistry], settings: &Settings) -> Chemistry {
    let wanted = if settings.chemistry.is_empty() {
        "lipo"
    } else {
        &settings.chemistry
    };
    list.iter()
        .find(|c| c.value == wanted)
        .or_else(|| list.first())
        .cloned()
        .unwrap_or(Chemistry {
            value: wanted.to_string(),
            label: wanted.to_string(),
            full: 4.2,
            empty: 3.6,
            nominal: 3.7,
        })
}

/// A setting's effective value: what the operator pinned, else the default.
pub fn resolved_number(settings: &Settings, key: &str, fallback: f64) -> f64 {
    let v = settings.number(key);
    if v > 0.0 {
        v
    } else {
        fallback
    }
}

/// Where a threshold sits on a 0 to 100 scale, or `None` when it cannot be
/// placed: PX4 states levels as a fraction of the pack, ArduPilot in volts,
/// which is a position only once the cell count and the chemistry endpoints
/// are known.
pub fn threshold_percent(t: &Threshold, cells: f64, settings: &Settings, list: &[Chemistry]) -> Option<f64> {
    if let Some(p) = t.percent {
        return Some(p);
    }
    let volts = t.volts?;
    if cells <= 0.0 {
        return None;
    }
    let chem = chemistry(list, settings);
    let full = resolved_number(settings, "full_cell", chem.full);
    let empty = resolved_number(settings, "empty_cell", chem.empty);
    if full <= empty {
        return None;
    }
    let per_cell = volts / cells;
    Some((per_cell - empty) / (full - empty) * 100.0)
}

/// One threshold flag to place: the x of its line and the width of its label.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flag {
    pub x: f32,
    pub width: f32,
}

/// Which side of its line a label sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    End,
}

/// A row and a side for every threshold label, so none is printed over
/// another and no line cuts through a label that is not its own
/// (`layoutThresholdLabels`). Row 0 is the one nearest the pack.
pub fn layout_threshold_labels(flags: &[Flag], left: f32, right: f32) -> Vec<(usize, Anchor)> {
    let fits_right = |f: &Flag| f.x + FLAG_GAP + f.width <= right;
    let fits_left = |f: &Flag| f.x - FLAG_GAP - f.width >= left;
    let side = if flags.iter().all(fits_right) {
        Some(Anchor::Start)
    } else if flags.iter().all(fits_left) {
        Some(Anchor::End)
    } else {
        None
    };
    struct Boxed {
        x: f32,
        anchor: Anchor,
        from: f32,
        to: f32,
        row: usize,
    }
    let mut boxes: Vec<Boxed> = flags
        .iter()
        .map(|f| {
            let anchor = side.unwrap_or(if fits_right(f) || !fits_left(f) {
                Anchor::Start
            } else {
                Anchor::End
            });
            let from = if anchor == Anchor::Start {
                f.x + FLAG_GAP
            } else {
                f.x - FLAG_GAP - f.width
            };
            Boxed {
                x: f.x,
                anchor,
                from: from - FLAG_PAD,
                to: from + f.width + FLAG_PAD,
                row: 0,
            }
        })
        .collect();
    let apart = |a: &Boxed, b: &Boxed| a.to <= b.from || b.to <= a.from;
    let misses = |line: &Boxed, label: &Boxed| line.x <= label.from || line.x >= label.to;
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| {
        let (ba, bb) = (&boxes[a], &boxes[b]);
        if ba.anchor != bb.anchor {
            return if ba.anchor == Anchor::End {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let by_x = if ba.anchor == Anchor::End {
            ba.x.partial_cmp(&bb.x)
        } else {
            bb.x.partial_cmp(&ba.x)
        }
        .unwrap_or(std::cmp::Ordering::Equal);
        by_x.then(if ba.anchor == Anchor::End { a.cmp(&b) } else { b.cmp(&a) })
    });
    let mut done: Vec<usize> = Vec::new();
    for i in order {
        let fits = |boxes: &[Boxed], row: usize, done: &[usize]| {
            done.iter().all(|&o| {
                let other = &boxes[o];
                if other.row == row {
                    apart(&boxes[i], other)
                } else if other.row > row {
                    misses(other, &boxes[i])
                } else {
                    misses(&boxes[i], other)
                }
            })
        };
        let mut row = 0;
        while row <= done.len() && !fits(&boxes, row, &done) {
            row += 1;
        }
        if row > done.len() {
            row = 0;
            while done
                .iter()
                .any(|&o| boxes[o].row == row && !apart(&boxes[i], &boxes[o]))
            {
                row += 1;
            }
        }
        boxes[i].row = row;
        done.push(i);
    }
    boxes.iter().map(|b| (b.row, b.anchor)).collect()
}

/// Healthy, warning or critical, as the top bar's own thresholds do.
pub fn level(percent: f64, connected: bool) -> &'static str {
    if !connected {
        "off"
    } else if percent > 25.0 {
        "healthy"
    } else if percent > 12.0 {
        "warning"
    } else {
        "critical"
    }
}

/// `formatDuration`.
pub fn format_duration(seconds: f64) -> String {
    let total = js_round(seconds.max(0.0)) as i64;
    let minutes = total / 60;
    let rest = total % 60;
    if minutes >= 60 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else if minutes > 0 {
        format!("{minutes} min {rest} s")
    } else {
        format!("{rest} s")
    }
}

/// `Number(x)` of a telemetry key: NaN when it is not there, 0 for null.
pub fn number_of(s: &Value, key: &str) -> f64 {
    match s.get(key) {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(t)) => {
            let t = t.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `Number(x) || 0`.
fn num0(s: &Value, key: &str) -> f64 {
    let v = number_of(s, key);
    if v.is_nan() {
        0.0
    } else {
        v
    }
}

/// The per cell voltages the pack reports.
pub fn measured_cells(s: &Value) -> Vec<f64> {
    s.get("battery_cell_voltages")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default()
}

/// The cell count the backend resolved, else the vehicle's own parameter.
pub fn cells_of(s: &Value, pack: &Pack) -> f64 {
    let live = number_of(s, "battery_cells");
    if live > 0.0 {
        live
    } else {
        pack.cells
    }
}

/// `cellText`.
pub fn cell_text(connected: bool, s: &Value, voltage: f64, cells: f64, measured: &[f64]) -> String {
    if !connected || cells <= 0.0 {
        return "\u{2014}".into();
    }
    if measured.len() > 1 {
        let low = measured.iter().copied().fold(f64::INFINITY, f64::min);
        let high = measured.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let spread = (high - low) * 1000.0;
        return format!(
            "{} to {} V ({} mV apart)",
            js_to_fixed(low, 2),
            js_to_fixed(high, 2),
            js_to_fixed(js_round(spread), 0)
        );
    }
    let live = number_of(s, "battery_cell_voltage");
    let per_cell = if live > 0.0 {
        live
    } else if voltage > 0.0 {
        voltage / cells
    } else {
        0.0
    };
    if per_cell <= 0.0 {
        return "\u{2014}".into();
    }
    format!("{} V \u{d7} {}S", js_to_fixed(per_cell, 2), js_to_fixed(cells, 0))
}

/// The label under each cell of the drawing.
pub fn cell_label(connected: bool, index: usize, measured: &[f64], voltage: f64, cells: f64) -> String {
    if !connected {
        return "\u{2014}".into();
    }
    let volts = if measured.len() > index {
        measured[index]
    } else if cells > 0.0 {
        voltage / cells
    } else {
        0.0
    };
    if volts > 0.0 {
        format!("{} V", js_to_fixed(volts, 2))
    } else {
        "\u{2014}".into()
    }
}

/// One line of the readout under the drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    /// `healthy`, `warning`, `critical` or `off`: the value's colour.
    pub level: Option<&'static str>,
}

/// Everything the live readout shows for a telemetry state.
pub struct Live {
    pub connected: bool,
    pub percent: f64,
    pub voltage: f64,
    pub cells: f64,
    pub using_estimate: bool,
    pub measured: Vec<f64>,
    pub lines: Vec<Line>,
}

pub fn live(s: &Value, pack: &Pack, units: &Units) -> Live {
    let dash = || "\u{2014}".to_string();
    let connected = s.get("connected").is_some_and(truthy);
    let cells = cells_of(s, pack);
    let percent = if connected { num0(s, "battery_percent") } else { 0.0 };
    let voltage = if connected { num0(s, "battery_voltage") } else { 0.0 };
    let current = if connected { num0(s, "battery_current") } else { 0.0 };
    let estimated = number_of(s, "battery_percent_est");
    let reported = number_of(s, "battery_percent_fc");
    let using_estimate = s.get("battery_source").and_then(Value::as_str) == Some("estimate");
    let measured = measured_cells(s);
    let consumed = number_of(s, "battery_consumed_mah");
    let remaining_time = number_of(s, "battery_time_remaining");
    let temperature = s.get("battery_temperature").and_then(Value::as_f64);
    let lv = level(percent, connected);
    let line = |key, label, value: String, level| Line {
        key,
        label,
        value,
        level,
    };
    let lines = vec![
        line(
            "remaining",
            "Remaining",
            if connected {
                format!("{}%", js_to_fixed(js_round(percent), 0))
            } else {
                dash()
            },
            Some(lv),
        ),
        line(
            "voltage",
            "Pack voltage",
            if connected && voltage > 0.0 {
                format!("{} V", js_to_fixed(voltage, 1))
            } else {
                dash()
            },
            None,
        ),
        line(
            "cell",
            "Per cell",
            cell_text(connected, s, voltage, cells, &measured),
            None,
        ),
        line(
            "current",
            "Current",
            if connected {
                format!("{} A", js_to_fixed(current, 1))
            } else {
                dash()
            },
            None,
        ),
        line(
            "power",
            "Power",
            if connected && voltage > 0.0 {
                format!("{} W", js_to_fixed(js_round(voltage * current), 0))
            } else {
                dash()
            },
            None,
        ),
        line(
            "consumed",
            "Used",
            if consumed > 0.0 {
                format!("{} mAh", js_to_fixed(js_round(consumed), 0))
            } else {
                dash()
            },
            None,
        ),
        line(
            "capacity",
            "Capacity",
            if pack.capacity_mah > 0.0 {
                format!("{} mAh", js_to_fixed(js_round(pack.capacity_mah), 0))
            } else {
                dash()
            },
            None,
        ),
        line(
            "temperature",
            "Temperature",
            temperature.map_or_else(dash, |t| units.format_temperature(t, 1)),
            None,
        ),
        line(
            "endurance",
            "Time left",
            if remaining_time > 0.0 {
                format_duration(remaining_time)
            } else {
                dash()
            },
            None,
        ),
        line(
            "fc",
            if using_estimate {
                "Autopilot says"
            } else {
                "Cell voltage says"
            },
            if !connected {
                dash()
            } else if using_estimate {
                if reported >= 0.0 {
                    format!("{}%", js_to_fixed(js_round(reported), 0))
                } else {
                    "no estimate".into()
                }
            } else if estimated >= 0.0 {
                format!("{}%", js_to_fixed(js_round(estimated), 0))
            } else {
                "no cell count".into()
            },
            None,
        ),
    ];
    Live {
        connected,
        percent,
        voltage,
        cells,
        using_estimate,
        measured,
        lines,
    }
}

/// What "Take these from the vehicle" saves: the cell count and endpoints the
/// autopilot reports, and its resistance as milliohms per cell (the vehicle
/// holds ohms for the whole pack).
pub fn copy_patch(pack: &Pack) -> Value {
    let mut patch = serde_json::Map::new();
    if pack.cells > 0.0 {
        patch.insert("cells".into(), json!(pack.cells));
    }
    if pack.full_cell > 0.0 {
        patch.insert("full_cell".into(), json!(pack.full_cell));
    }
    if pack.empty_cell > 0.0 {
        patch.insert("empty_cell".into(), json!(pack.empty_cell));
    }
    if pack.resistance_ohm > 0.0 && pack.cells > 0.0 {
        patch.insert(
            "resistance".into(),
            json!(js_round(pack.resistance_ohm / pack.cells * 1000.0 * 10.0) / 10.0),
        );
    }
    Value::Object(patch)
}

/// True when the vehicle told us something worth copying.
pub fn has_vehicle_pack(pack: &Pack) -> bool {
    pack.cells != 0.0 || pack.full_cell != 0.0 || pack.empty_cell != 0.0 || pack.resistance_ohm != 0.0
}

/// A typed estimator setting: blank means "work it out" (0), else a number
/// within `min..=max`. The error is `rangeProblem`'s text.
pub fn parse_setting(raw: &str, min: f64, max: f64) -> Result<f64, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(0.0);
    }
    let Some(v) = t.parse::<f64>().ok().filter(|v| v.is_finite()) else {
        return Err("not a number".into());
    };
    if v < min {
        return Err(format!("below {}", crate::setup::schema_form::format_number(min)));
    }
    if v > max {
        return Err(format!("above {}", crate::setup::schema_form::format_number(max)));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(v: &[(f32, f32)]) -> Vec<Flag> {
        v.iter().map(|&(x, width)| Flag { x, width }).collect()
    }

    fn rows(v: &[(f32, f32)]) -> Vec<(usize, Anchor)> {
        layout_threshold_labels(&flags(v), 0.0, FIGURE_WIDTH)
    }

    #[test]
    fn the_flags_are_stacked_as_the_web_page_stacks_them() {
        use Anchor::{End, Start};
        // expected values taken from running layoutThresholdLabels
        assert_eq!(
            rows(&[(30.0, 40.0), (50.0, 44.0), (80.0, 50.0)]),
            [(2, Start), (1, Start), (0, Start)]
        );
        assert_eq!(rows(&[(10.0, 40.0)]), [(0, Start)]);
        assert_eq!(rows(&[(240.0, 40.0)]), [(0, End)]);
        assert_eq!(
            rows(&[(240.0, 40.0), (200.0, 40.0), (20.0, 50.0)]),
            [(0, End), (1, Start), (0, Start)]
        );
        assert_eq!(
            rows(&[(100.0, 30.0), (105.0, 30.0), (110.0, 30.0), (112.0, 30.0)]),
            [(3, Start), (2, Start), (1, Start), (0, Start)]
        );
    }

    #[test]
    fn a_level_in_volts_needs_a_cell_count() {
        let volts = Threshold {
            id: "low".into(),
            label: "Low".into(),
            percent: None,
            volts: Some(21.6),
        };
        let chems = vec![Chemistry {
            value: "lipo".into(),
            label: "LiPo".into(),
            full: 4.2,
            empty: 3.6,
            nominal: 3.7,
        }];
        let s = Settings::default();
        assert_eq!(threshold_percent(&volts, 0.0, &s, &chems), None);
        // 21.6 V on 6S is 3.6 V a cell: empty
        assert_eq!(threshold_percent(&volts, 6.0, &s, &chems).map(|p| p.round()), Some(0.0));
        let pinned = Settings {
            full_cell: 4.0,
            empty_cell: 3.0,
            ..Settings::default()
        };
        let p = threshold_percent(&volts, 6.0, &pinned, &chems).unwrap();
        assert!((p - 60.0).abs() < 1e-9);
        let pct = Threshold {
            percent: Some(15.0),
            volts: None,
            ..volts
        };
        assert_eq!(threshold_percent(&pct, 0.0, &s, &chems), Some(15.0));
    }

    #[test]
    fn the_fill_takes_the_top_bars_colours() {
        assert_eq!(level(26.0, true), "healthy");
        assert_eq!(level(25.0, true), "warning");
        assert_eq!(level(12.5, true), "warning");
        assert_eq!(level(12.0, true), "critical");
        assert_eq!(level(80.0, false), "off");
    }

    #[test]
    fn a_time_left_reads_as_the_page_writes_it() {
        assert_eq!(format_duration(45.0), "45 s");
        assert_eq!(format_duration(125.0), "2 min 5 s");
        assert_eq!(format_duration(3600.0), "1 h 0 min");
        assert_eq!(format_duration(7260.4), "2 h 1 min");
        assert_eq!(format_duration(-3.0), "0 s");
    }

    #[test]
    fn the_cell_line_says_the_spread_or_the_mean() {
        let s = json!({});
        assert_eq!(
            cell_text(true, &s, 24.9, 6.0, &[4.149, 4.15, 4.149]),
            "4.15 to 4.15 V (1 mV apart)"
        );
        assert_eq!(cell_text(true, &json!({}), 24.9, 6.0, &[]), "4.15 V \u{d7} 6S");
        assert_eq!(
            cell_text(true, &json!({"battery_cell_voltage": 3.71}), 24.9, 6.0, &[]),
            "3.71 V \u{d7} 6S"
        );
        assert_eq!(cell_text(false, &s, 24.9, 6.0, &[]), "\u{2014}");
        assert_eq!(cell_text(true, &s, 24.9, 0.0, &[]), "\u{2014}");
        assert_eq!(cell_label(true, 1, &[4.1, 4.2], 24.0, 6.0), "4.20 V");
        assert_eq!(cell_label(true, 3, &[4.1, 4.2], 24.0, 6.0), "4.00 V");
        assert_eq!(cell_label(false, 0, &[], 24.0, 6.0), "\u{2014}");
    }

    fn state() -> Value {
        json!({
            "connected": true, "battery_percent": 100, "battery_voltage": 24.9,
            "battery_current": 0.9, "battery_percent_fc": 100, "battery_percent_est": 94.7,
            "battery_source": "autopilot", "battery_cells": 6, "battery_cell_voltage": 4.15,
            "battery_cell_voltages": [4.149, 4.149, 4.149, 4.149, 4.149, 4.149],
            "battery_consumed_mah": 27.6, "battery_temperature": 28.7, "battery_time_remaining": 0
        })
    }

    #[test]
    fn the_readout_matches_the_simulated_pack_in_the_web_page() {
        let pack = Pack {
            cells: 6.0,
            capacity_mah: 16000.0,
            ..Pack::default()
        };
        let l = live(&state(), &pack, &Units::default());
        let text: Vec<(&str, &str)> = l.lines.iter().map(|x| (x.label, x.value.as_str())).collect();
        assert_eq!(
            text,
            [
                ("Remaining", "100%"),
                ("Pack voltage", "24.9 V"),
                ("Per cell", "4.15 to 4.15 V (0 mV apart)"),
                ("Current", "0.9 A"),
                ("Power", "22 W"),
                ("Used", "28 mAh"),
                ("Capacity", "16000 mAh"),
                ("Temperature", "28.7 \u{b0}C"),
                ("Time left", "\u{2014}"),
                ("Cell voltage says", "95%"),
            ]
        );
        assert_eq!(l.lines[0].level, Some("healthy"));
    }

    #[test]
    fn the_other_answer_is_named_by_the_one_flown_by() {
        let mut s = state();
        s["battery_source"] = json!("estimate");
        let l = live(&s, &Pack::default(), &Units::default());
        assert_eq!(l.lines[9].label, "Autopilot says");
        assert_eq!(l.lines[9].value, "100%");
        s["battery_percent_fc"] = json!(-1);
        assert_eq!(
            live(&s, &Pack::default(), &Units::default()).lines[9].value,
            "no estimate"
        );
        let mut t = state();
        t["battery_percent_est"] = json!(-1);
        assert_eq!(
            live(&t, &Pack::default(), &Units::default()).lines[9].value,
            "no cell count"
        );
        let off = json!({"connected": false});
        let l = live(&off, &Pack::default(), &Units::default());
        assert!(l.lines.iter().all(|x| x.value == "\u{2014}"));
        assert_eq!(l.lines[0].level, Some("off"));
    }

    #[test]
    fn the_cell_count_comes_from_the_backend_before_the_vehicle() {
        let pack = Pack {
            cells: 4.0,
            ..Pack::default()
        };
        assert_eq!(cells_of(&json!({"battery_cells": 6}), &pack), 6.0);
        assert_eq!(cells_of(&json!({"battery_cells": 0}), &pack), 4.0);
        assert_eq!(cells_of(&json!({}), &pack), 4.0);
    }

    #[test]
    fn the_resistance_is_copied_in_milliohms_a_cell() {
        let pack = Pack {
            cells: 6.0,
            full_cell: 4.15,
            empty_cell: 3.5,
            resistance_ohm: 0.005,
            ..Pack::default()
        };
        assert_eq!(
            copy_patch(&pack),
            json!({"cells": 6.0, "full_cell": 4.15, "empty_cell": 3.5, "resistance": 0.8})
        );
        assert_eq!(copy_patch(&Pack::default()), json!({}));
        assert!(has_vehicle_pack(&pack));
        assert!(!has_vehicle_pack(&Pack::default()));
    }

    #[test]
    fn a_blank_setting_means_work_it_out() {
        assert_eq!(parse_setting("", 0.0, 5.0), Ok(0.0));
        assert_eq!(parse_setting(" 4.2 ", 0.0, 5.0), Ok(4.2));
        assert_eq!(parse_setting("6", 0.0, 5.0), Err("above 5".into()));
        assert_eq!(parse_setting("-1", 0.0, 5.0), Err("below 0".into()));
        assert_eq!(parse_setting("x", 0.0, 5.0), Err("not a number".into()));
    }

    #[test]
    fn the_description_is_read() {
        let d = parse_doc(&json!({
            "connected": true, "received": 9,
            "sections": [{"id": "pack", "title": "Pack", "hint": "h",
                "fields": [{"param": "BAT1_N_CELLS", "kind": "number", "value": 6}]}],
            "pack": {"cells": 6, "thresholds": [{"id": "low", "label": "Low", "percent": 15}]},
            "chemistries": [{"value": "lipo", "label": "LiPo", "full": 4.2, "empty": 3.6, "nominal": 3.7}],
            "configured": {"estimate": true, "cells": 6}
        }));
        assert!(d.connected);
        assert_eq!((d.received, d.sections.len(), d.sections[0].fields.len()), (9, 1, 1));
        assert_eq!(d.pack.thresholds[0].percent, Some(15.0));
        assert!(d.configured.estimate);
        assert_eq!(d.configured.cells, 6.0);
        assert_eq!(d.configured.chemistry, "lipo");
        assert_eq!(chemistry(&d.chemistries, &d.configured).label, "LiPo");
        assert_eq!(chemistry(&[], &Settings::default()).full, 4.2);
        assert_eq!(resolved_number(&d.configured, "cells", 3.0), 6.0);
        assert_eq!(resolved_number(&d.configured, "full_cell", 4.2), 4.2);
    }
}
