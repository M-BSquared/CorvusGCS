//! Battery chemistry, cell arithmetic and the remaining charge estimate
//! (`battery.py`).
//!
//! Pure functions and tables. This is the one place that decides what "47%
//! remaining" means. `SYS_STATUS.battery_remaining` is whatever the autopilot
//! chose to publish, and on most airframes that is a coulomb count seeded by a
//! guess, wrong in the one direction that costs an aircraft: optimistic.
//! Resting cell voltage is independent of it, needs no capacity figure and
//! cannot inherit a stale count, so Corvus computes it alongside and lets the
//! operator choose which one the interface shows.
//!
//! State of charge against resting cell voltage is not a straight line, so
//! each chemistry carries a measured table that is interpolated. Voltage under
//! load is not resting voltage, so with an internal resistance the measured
//! voltage is corrected back to rest before the curve is consulted; without
//! one the raw voltage is used and the estimate is conservative.

use serde_json::{json, Map, Value};

use crate::protocol::common::{float_of_text, py_max, py_round, py_sum};

pub const MIN_CELLS: i64 = 1;
pub const MAX_CELLS: i64 = 24;
/// Per-cell resistance the sag correction accepts, in milliohms: a healthy
/// pack is single digit, anything past 100 is a typo.
pub const MAX_RESISTANCE_MOHM: f64 = 100.0;

pub struct Chemistry {
    pub key: &'static str,
    pub label: &'static str,
    pub full: f64,
    pub empty: f64,
    pub nominal: f64,
    pub curve: &'static [(f64, f64)],
}

pub const CHEMISTRIES: &[Chemistry] = &[
    Chemistry {
        key: "lipo",
        label: "LiPo",
        full: 4.2,
        empty: 3.6,
        nominal: 3.7,
        curve: &[
            (4.20, 100.0),
            (4.15, 95.0),
            (4.11, 90.0),
            (4.08, 85.0),
            (4.02, 80.0),
            (3.98, 75.0),
            (3.95, 70.0),
            (3.91, 65.0),
            (3.87, 60.0),
            (3.85, 55.0),
            (3.84, 50.0),
            (3.82, 45.0),
            (3.80, 40.0),
            (3.79, 35.0),
            (3.77, 30.0),
            (3.75, 25.0),
            (3.73, 20.0),
            (3.71, 15.0),
            (3.69, 10.0),
            (3.61, 5.0),
            (3.27, 0.0),
        ],
    },
    Chemistry {
        key: "liion",
        label: "Li-ion",
        full: 4.2,
        empty: 3.2,
        nominal: 3.6,
        curve: &[
            (4.20, 100.0),
            (4.10, 90.0),
            (4.00, 80.0),
            (3.92, 70.0),
            (3.85, 60.0),
            (3.78, 50.0),
            (3.70, 40.0),
            (3.62, 30.0),
            (3.53, 20.0),
            (3.40, 10.0),
            (3.20, 5.0),
            (3.00, 0.0),
        ],
    },
    Chemistry {
        key: "lifepo4",
        label: "LiFePO4",
        full: 3.65,
        empty: 3.0,
        nominal: 3.2,
        curve: &[
            (3.65, 100.0),
            (3.35, 99.0),
            (3.32, 90.0),
            (3.30, 80.0),
            (3.28, 70.0),
            (3.26, 60.0),
            (3.25, 50.0),
            (3.22, 40.0),
            (3.20, 30.0),
            (3.17, 20.0),
            (3.13, 10.0),
            (3.00, 5.0),
            (2.80, 0.0),
        ],
    },
];

pub const DEFAULT_CHEMISTRY: &str = "lipo";

/// Python's `str.strip()`, `str.lower()` and a table lookup. A name this build
/// has never heard of gets LiPo rather than an error: the setting travels in a
/// config file an older or newer Corvus may have written.
pub fn chemistry(name: &str) -> &'static Chemistry {
    let key = name.trim().to_lowercase();
    CHEMISTRIES.iter().find(|c| c.key == key).unwrap_or(&CHEMISTRIES[0])
}

/// The chemistries as the page's picker wants them (the curve does not travel).
pub fn chemistry_options() -> Vec<Value> {
    CHEMISTRIES
        .iter()
        .map(|c| json!({"value": c.key, "label": c.label, "full": c.full, "empty": c.empty, "nominal": c.nominal}))
        .collect()
}

fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

/// A float, or `default`. Booleans are not numbers here.
fn number(raw: Option<&Value>, default: f64) -> f64 {
    let value = match raw {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => match float_of_text(s) {
            Some(v) => v,
            None => return default,
        },
        _ => return default,
    };
    if value.is_finite() {
        value
    } else {
        default
    }
}

/// The estimator keys of a parsed config file, bounded here because these
/// numbers reach an arithmetic path that runs on every telemetry frame; the
/// rest is dropped. None when nothing is left.
pub fn coerce_settings(raw: &Value) -> Option<Map<String, Value>> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    if let Some(Value::Bool(b)) = raw.get("estimate") {
        out.insert("estimate".into(), json!(b));
    }
    if let Some(Value::String(name)) = raw.get("chemistry") {
        let key = name.trim().to_lowercase();
        if CHEMISTRIES.iter().any(|c| c.key == key) {
            out.insert("chemistry".into(), json!(key));
        }
    }
    if raw.contains_key("cells") {
        out.insert(
            "cells".into(),
            json!(clamp(number(raw.get("cells"), 0.0), 0.0, MAX_CELLS as f64) as i64),
        );
    }
    for key in ["full_cell", "empty_cell"] {
        if raw.contains_key(key) {
            out.insert(
                key.into(),
                json!(py_round(clamp(number(raw.get(key), 0.0), 0.0, 5.0), 3)),
            );
        }
    }
    if raw.contains_key("resistance") {
        out.insert(
            "resistance".into(),
            json!(py_round(
                clamp(number(raw.get("resistance"), 0.0), 0.0, MAX_RESISTANCE_MOHM),
                3
            )),
        );
    }
    if raw.contains_key("capacity_mah") {
        out.insert(
            "capacity_mah".into(),
            json!(py_round(clamp(number(raw.get("capacity_mah"), 0.0), 0.0, 1e6), 1)),
        );
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The resolved estimator settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub estimate: bool,
    pub chemistry: String,
    /// 0 works the count out from the pack voltage.
    pub cells: i64,
    pub full_cell: f64,
    pub empty_cell: f64,
    /// Milliohms per cell; 0 does not correct for sag.
    pub resistance: f64,
    /// 0 is unknown.
    pub capacity_mah: f64,
}

impl Settings {
    pub fn to_json(&self) -> Value {
        json!({
            "estimate": self.estimate,
            "chemistry": self.chemistry,
            "cells": self.cells,
            "full_cell": self.full_cell,
            "empty_cell": self.empty_cell,
            "resistance": self.resistance,
            "capacity_mah": self.capacity_mah,
        })
    }
}

/// The full settings: the defaults with `raw` laid over them. `estimate` is
/// off by default on purpose: turning it on changes the number on the top
/// bar, and a ground station must not do that to an operator who never asked.
pub fn settings(raw: Option<&Value>) -> Settings {
    let mut s = Settings {
        estimate: false,
        chemistry: DEFAULT_CHEMISTRY.to_string(),
        cells: 0,
        full_cell: 0.0,
        empty_cell: 0.0,
        resistance: 0.0,
        capacity_mah: 0.0,
    };
    if let Some(over) = raw.and_then(coerce_settings) {
        if let Some(v) = over.get("estimate").and_then(Value::as_bool) {
            s.estimate = v;
        }
        if let Some(v) = over.get("chemistry").and_then(Value::as_str) {
            s.chemistry = v.to_string();
        }
        if let Some(v) = over.get("cells").and_then(Value::as_i64) {
            s.cells = v;
        }
        if let Some(v) = over.get("full_cell").and_then(Value::as_f64) {
            s.full_cell = v;
        }
        if let Some(v) = over.get("empty_cell").and_then(Value::as_f64) {
            s.empty_cell = v;
        }
        if let Some(v) = over.get("resistance").and_then(Value::as_f64) {
            s.resistance = v;
        }
        if let Some(v) = over.get("capacity_mah").and_then(Value::as_f64) {
            s.capacity_mah = v;
        }
    }
    let chem = chemistry(&s.chemistry);
    // Endpoints resolve here, once. 0 has always meant "use the chemistry's
    // own figure"; an operator who pinned 4.35 V full keeps it.
    if s.full_cell == 0.0 {
        s.full_cell = chem.full;
    }
    if s.empty_cell == 0.0 {
        s.empty_cell = chem.empty;
    }
    if s.empty_cell >= s.full_cell {
        s.full_cell = chem.full;
        s.empty_cell = chem.empty;
    }
    s
}

/// How many cells the measured pack voltage implies. A configured count
/// always wins. Otherwise the fewest cells that could hold this voltage
/// without any of them being over-charged: exact on a pack just plugged in,
/// ambiguous on a flown one (21.0 V is a fresh 5S and a tired 6S), which is
/// why the caller latches the count from the first reading of a connection.
/// 0 when nothing fits.
pub fn resolve_cells(voltage: f64, resolved: &Settings) -> i64 {
    if resolved.cells != 0 {
        return resolved.cells;
    }
    let volts = number(Some(&json!(voltage)), 0.0);
    if volts <= 0.0 {
        return 0;
    }
    let full = resolved.full_cell;
    if full <= 0.0 {
        return 0;
    }
    // 25.2 / 4.2 is 6.000000000000001 in binary floating point, and a ceil of
    // that turns every fully charged 6S pack into a 7S one.
    let cells = (volts / full - 1e-6).ceil() as i64;
    if !(MIN_CELLS..=MAX_CELLS).contains(&cells) {
        return 0;
    }
    // A pack under its own empty voltage is still that pack, so the floor is
    // generous. What it rejects is noise.
    if volts / (cells as f64) < resolved.empty_cell * 0.75 {
        return 0;
    }
    cells
}

/// Pack voltage corrected back to rest: `V_rest = V_measured + I * R_pack`;
/// the measured voltage unchanged with no configured resistance.
pub fn rest_voltage(voltage: f64, current: f64, cells: i64, resolved: &Settings) -> f64 {
    let volts = number(Some(&json!(voltage)), 0.0);
    let amps = number(Some(&json!(current)), 0.0);
    let ohms_per_cell = number(Some(&json!(resolved.resistance)), 0.0) / 1000.0;
    if volts <= 0.0 || amps <= 0.0 || cells <= 0 || ohms_per_cell <= 0.0 {
        return volts;
    }
    volts + amps * ohms_per_cell * cells as f64
}

/// Linear interpolation on a high to low (voltage, percent) table.
fn interpolate(curve: &[(f64, f64)], volts: f64) -> f64 {
    if volts >= curve[0].0 {
        return curve[0].1;
    }
    let last = curve[curve.len() - 1];
    if volts <= last.0 {
        return last.1;
    }
    for pair in curve.windows(2) {
        let (high_v, high_p) = pair[0];
        let (low_v, low_p) = pair[1];
        if low_v <= volts && volts <= high_v {
            let span = high_v - low_v;
            if span <= 0.0 {
                return low_p;
            }
            return low_p + (volts - low_v) / span * (high_p - low_p);
        }
    }
    last.1
}

/// Percent remaining for one cell at `cell_voltage`, 0 to 100. The
/// chemistry's curve answers the question; the operator's endpoints rescale
/// it so their own empty reads 0 and their own full reads 100.
pub fn state_of_charge(cell_voltage: f64, resolved: &Settings) -> f64 {
    let volts = number(Some(&json!(cell_voltage)), 0.0);
    if volts <= 0.0 {
        return 0.0;
    }
    let curve = chemistry(&resolved.chemistry).curve;
    let raw = interpolate(curve, volts);
    let at_empty = interpolate(curve, resolved.empty_cell);
    let at_full = interpolate(curve, resolved.full_cell);
    let span = at_full - at_empty;
    if span <= 0.0 {
        return clamp(raw, 0.0, 100.0);
    }
    clamp((raw - at_empty) / span * 100.0, 0.0, 100.0)
}

/// The voltage side reading of a pack. `percent` is -1 when the pack cannot be
/// read at all: a ground station that shows an empty battery when it has not
/// been told anything is the same lie as one that shows a full one.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub percent: f64,
    pub cells: i64,
    pub cell_voltage: f64,
    pub rest_cell_voltage: f64,
    pub chemistry: String,
}

impl Estimate {
    pub fn to_json(&self) -> Value {
        json!({
            "percent": self.percent,
            "cells": self.cells,
            "cell_voltage": self.cell_voltage,
            "rest_cell_voltage": self.rest_cell_voltage,
            "chemistry": self.chemistry,
        })
    }
}

pub fn estimate(voltage: f64, current: f64, raw_settings: Option<&Value>) -> Estimate {
    let resolved = settings(raw_settings);
    let volts = number(Some(&json!(voltage)), 0.0);
    let cells = resolve_cells(volts, &resolved);
    if volts <= 0.0 || cells <= 0 {
        return Estimate {
            percent: -1.0,
            cells,
            cell_voltage: 0.0,
            rest_cell_voltage: 0.0,
            chemistry: resolved.chemistry,
        };
    }
    let rest = rest_voltage(volts, current, cells, &resolved);
    let n = cells as f64;
    Estimate {
        percent: py_round(state_of_charge(rest / n, &resolved), 1),
        cells,
        cell_voltage: py_round(volts / n, 3),
        rest_cell_voltage: py_round(rest / n, 3),
        chemistry: resolved.chemistry,
    }
}

/// Seconds of samples the endurance fit looks at.
pub const ENDURANCE_WINDOW_S: f64 = 180.0;
pub const ENDURANCE_MIN_SPAN_S: f64 = 60.0;
pub const ENDURANCE_MIN_DROP: f64 = 1.0;

/// Seconds until the remaining percentage reaches zero at the recent rate,
/// from `(seconds, percent)` samples taken while armed, oldest first. Only the
/// last three minutes count, so a climb does not set the rate for the cruise.
/// -1 when there is no honest answer: too short a window, too little drop, or
/// a percentage that is not falling.
pub fn drain_endurance(samples: &[(f64, f64)]) -> f64 {
    let mut points: Vec<(f64, f64)> = samples
        .iter()
        .copied()
        .filter(|(t, p)| t.is_finite() && p.is_finite() && (0.0..=100.0).contains(p))
        .collect();
    if points.len() < 3 {
        return -1.0;
    }
    let latest = points[points.len() - 1].0;
    points.retain(|pt| latest - pt.0 <= ENDURANCE_WINDOW_S);
    let span = points[points.len() - 1].0 - points[0].0;
    if points.len() < 3 || span < ENDURANCE_MIN_SPAN_S {
        return -1.0;
    }
    let n = points.len() as f64;
    let mean_t = py_sum(&points.iter().map(|p| p.0).collect::<Vec<_>>()) / n;
    let mean_p = py_sum(&points.iter().map(|p| p.1).collect::<Vec<_>>()) / n;
    let sxx = py_sum(&points.iter().map(|p| (p.0 - mean_t).powi(2)).collect::<Vec<_>>());
    if sxx <= 0.0 {
        return -1.0;
    }
    let slope = py_sum(
        &points
            .iter()
            .map(|p| (p.0 - mean_t) * (p.1 - mean_p))
            .collect::<Vec<_>>(),
    ) / sxx;
    if slope >= 0.0 || -slope * span < ENDURANCE_MIN_DROP {
        return -1.0;
    }
    // The fitted value rather than the last raw sample: the line is what the
    // rate came from, and a single sagging reading should not shorten it.
    let now = py_max(0.0, mean_p + slope * (latest - mean_t));
    (now / -slope).round_ties_even()
}
