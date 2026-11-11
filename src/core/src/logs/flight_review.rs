//! Flight Review (`flight_review.py`): a ULog reduced to the plots that
//! answer "was that flight healthy?", the findings above them and the flight
//! modes behind them.
//!
//! The arithmetic follows the Python line by line (the same order of
//! operations, `round` as Python rounds, `sum` of floats as Python 3.12 sums
//! them) so the review of the same log carries the same numbers.

#![allow(clippy::needless_range_loop, clippy::needless_late_init, clippy::type_complexity)]

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use super::ulog::{read, Col, ULog, UlogError};
use crate::json::num;
use crate::protocol::common::{py_max, py_min, py_round, py_sum};

pub const MAX_POINTS: usize = 1200;
pub const SENSITIVITIES: [&str; 3] = ["relaxed", "normal", "strict"];
pub const DEFAULT_SENSITIVITY: &str = "normal";

/// Where each finding starts, at one sensitivity.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub vibration_warn: f64,
    pub vibration_crit: f64,
    pub clipping_warn: f64,
    pub clipping_crit: f64,
    pub innovation_hold_s: f64,
    pub saturation_warn_s: f64,
    pub saturation_crit_s: f64,
    pub reset_events: i64,
    pub gps_lost_s: f64,
    pub rc_lost_s: f64,
    pub attitude_warn_deg: f64,
    pub attitude_crit_deg: f64,
    pub ringing_dps: f64,
    pub motor_spread_us: f64,
    pub mag_correlation: f64,
    pub mag_swing: f64,
    pub cell_warn_v: f64,
    pub cell_crit_v: f64,
    pub cell_hold_s: f64,
    pub sag_per_cell_v: f64,
    pub cpu_pct: f64,
    pub cpu_hold_s: f64,
    pub dropout_ms: f64,
    pub link_drop_pct: f64,
    pub remote_rssi: f64,
    pub unhealthy_s: f64,
}

const RELAXED: Limits = Limits {
    vibration_warn: 45.0,
    vibration_crit: 80.0,
    clipping_warn: 200.0,
    clipping_crit: 2000.0,
    innovation_hold_s: 5.0,
    saturation_warn_s: 4.0,
    saturation_crit_s: 10.0,
    reset_events: 5,
    gps_lost_s: 5.0,
    rc_lost_s: 1.0,
    attitude_warn_deg: 15.0,
    attitude_crit_deg: 25.0,
    ringing_dps: 20.0,
    motor_spread_us: 120.0,
    mag_correlation: 0.7,
    mag_swing: 0.15,
    cell_warn_v: 3.3,
    cell_crit_v: 3.1,
    cell_hold_s: 5.0,
    sag_per_cell_v: 0.7,
    cpu_pct: 95.0,
    cpu_hold_s: 3.0,
    dropout_ms: 1000.0,
    link_drop_pct: 10.0,
    remote_rssi: 40.0,
    unhealthy_s: 5.0,
};

const NORMAL: Limits = Limits {
    vibration_warn: 30.0,
    vibration_crit: 60.0,
    clipping_warn: 25.0,
    clipping_crit: 500.0,
    innovation_hold_s: 2.0,
    saturation_warn_s: 1.5,
    saturation_crit_s: 5.0,
    reset_events: 3,
    gps_lost_s: 2.0,
    rc_lost_s: 0.5,
    attitude_warn_deg: 10.0,
    attitude_crit_deg: 20.0,
    ringing_dps: 12.0,
    motor_spread_us: 80.0,
    mag_correlation: 0.5,
    mag_swing: 0.10,
    cell_warn_v: 3.4,
    cell_crit_v: 3.2,
    cell_hold_s: 3.0,
    sag_per_cell_v: 0.5,
    cpu_pct: 90.0,
    cpu_hold_s: 2.0,
    dropout_ms: 500.0,
    link_drop_pct: 5.0,
    remote_rssi: 55.0,
    unhealthy_s: 2.0,
};

const STRICT: Limits = Limits {
    vibration_warn: 20.0,
    vibration_crit: 45.0,
    clipping_warn: 1.0,
    clipping_crit: 50.0,
    innovation_hold_s: 1.0,
    saturation_warn_s: 0.5,
    saturation_crit_s: 2.0,
    reset_events: 1,
    gps_lost_s: 1.0,
    rc_lost_s: 0.0,
    attitude_warn_deg: 8.0,
    attitude_crit_deg: 15.0,
    ringing_dps: 8.0,
    motor_spread_us: 60.0,
    mag_correlation: 0.4,
    mag_swing: 0.05,
    cell_warn_v: 3.5,
    cell_crit_v: 3.3,
    cell_hold_s: 1.0,
    sag_per_cell_v: 0.35,
    cpu_pct: 85.0,
    cpu_hold_s: 1.0,
    dropout_ms: 200.0,
    link_drop_pct: 2.0,
    remote_rssi: 60.0,
    unhealthy_s: 0.0,
};

/// A known sensitivity name, or the default for anything else.
pub fn normalize_sensitivity(value: &Value) -> &'static str {
    if let Some(s) = value.as_str() {
        let s = s.trim().to_lowercase();
        for name in SENSITIVITIES {
            if s == name {
                return name;
            }
        }
    }
    DEFAULT_SENSITIVITY
}

pub fn normalize_sensitivity_str(value: &str) -> &'static str {
    normalize_sensitivity(&Value::from(value))
}

pub fn limits_for(sensitivity: &str) -> Limits {
    match normalize_sensitivity_str(sensitivity) {
        "relaxed" => RELAXED,
        "strict" => STRICT,
        _ => NORMAL,
    }
}

// ---------------------------------------------------------------------------
// Shared arithmetic
// ---------------------------------------------------------------------------

pub fn round_to(x: f64, digits: usize) -> f64 {
    py_round(x, digits)
}

/// `round(x)`: ties to even.
fn round_int(x: f64) -> f64 {
    x.round_ties_even()
}

/// `values[i]` of a Python list is read as a float, `nan` where that raises.
fn floats(col: &Col) -> Vec<f64> {
    (0..col.len()).map(|i| col.f64_at(i).unwrap_or(f64::NAN)).collect()
}

/// Reduce to MAX_POINTS, keeping the extreme sample of each bucket.
pub fn decimate(times: &[f64], values: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let count = values.len();
    if count <= MAX_POINTS {
        return (times.to_vec(), values.to_vec());
    }
    let step = count as f64 / MAX_POINTS as f64;
    let mut out_t = Vec::with_capacity(MAX_POINTS);
    let mut out_v = Vec::with_capacity(MAX_POINTS);
    for bucket in 0..MAX_POINTS {
        let start = (bucket as f64 * step) as usize;
        let end = (start + 1).max(((bucket + 1) as f64 * step) as usize);
        let mut best = start;
        let mut best_abs = -1.0;
        for index in start..end.min(count) {
            let value = values[index];
            let magnitude = if !(value).is_nan() { value.abs() } else { -1.0 };
            if magnitude > best_abs {
                best_abs = magnitude;
                best = index;
            }
        }
        out_t.push(times[best]);
        out_v.push(values[best]);
    }
    (out_t, out_v)
}

type Spans = [Span];

#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

fn series_value(name: &str, x: &[f64], y: &[f64], y_digits: usize) -> Value {
    json!({
        "name": name,
        "x": x.iter().map(|t| num(round_to(*t, 3))).collect::<Vec<_>>(),
        "y": y.iter().map(|v| if (*v).is_nan() { Value::Null } else { num(round_to(*v, y_digits)) }).collect::<Vec<_>>(),
    })
}

fn plot(
    id: &str,
    title: &str,
    unit: &str,
    series: Vec<Option<Value>>,
    group: &str,
    extra: Vec<(&str, Value)>,
) -> Option<Value> {
    let kept: Vec<Value> = series.into_iter().flatten().collect();
    if kept.is_empty() {
        return None;
    }
    let mut p = Map::new();
    p.insert("id".into(), id.into());
    p.insert("title".into(), title.into());
    p.insert("unit".into(), unit.into());
    p.insert("group".into(), group.into());
    p.insert("series".into(), Value::Array(kept));
    for (k, v) in extra {
        p.insert(k.into(), v);
    }
    Some(Value::Object(p))
}

fn note(text: &str) -> Vec<(&'static str, Value)> {
    vec![("note", text.into())]
}

fn ys(series: &Value) -> Vec<Option<f64>> {
    series["y"]
        .as_array()
        .map(|a| a.iter().map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn xs(series: &Value) -> Vec<f64> {
    series["x"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

/// `bisect.bisect_left`, step for step.
fn bisect_left(a: &[f64], x: f64) -> usize {
    let (mut lo, mut hi) = (0usize, a.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if a[mid] < x {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn sample_dt(times: &[f64]) -> f64 {
    if times.len() < 2 {
        return 0.0;
    }
    let span = times[times.len() - 1] - times[0];
    if span > 0.0 {
        span / (times.len() - 1) as f64
    } else {
        0.0
    }
}

/// `(seconds past the threshold, when it first was, the worst value)`.
fn time_above(times: &[f64], values: &[f64], threshold: f64, below: bool) -> (f64, Option<f64>, f64) {
    let mut total = 0.0;
    let mut first = None;
    let mut worst = if below { f64::INFINITY } else { f64::NEG_INFINITY };
    let dt = sample_dt(times);
    for (index, &value) in values.iter().enumerate() {
        let past = if below { value < threshold } else { value > threshold };
        if !past {
            continue;
        }
        if first.is_none() {
            first = Some(times[index]);
        }
        worst = if below {
            py_min(worst, value)
        } else {
            py_max(worst, value)
        };
        let step = if index + 1 < times.len() {
            times[index + 1] - times[index]
        } else {
            dt
        };
        total += py_max(0.0, py_min(step, 1.0));
    }
    match first {
        None => (0.0, None, 0.0),
        Some(_) => (total, first, worst),
    }
}

fn during(when: Option<f64>, spans: &Spans, grace: f64) -> bool {
    if spans.is_empty() {
        return true;
    }
    let Some(when) = when else {
        return false;
    };
    spans.iter().any(|s| s.start <= when && when <= s.end + grace)
}

fn rise(times: &[f64], values: &[f64], spans: &Spans) -> (f64, Option<f64>) {
    let all = [Span {
        start: f64::NEG_INFINITY,
        end: f64::INFINITY,
    }];
    let windows: &[Span] = if spans.is_empty() { &all } else { spans };
    let mut total = 0.0;
    let mut first = None;
    for span in windows {
        let mut index = bisect_left(times, span.start);
        let mut previous: Option<f64> = None;
        while index < times.len() && times[index] <= span.end {
            let value = values[index];
            if let Some(p) = previous {
                if value > p {
                    total += value - p;
                    if first.is_none() {
                        first = Some(times[index]);
                    }
                }
            }
            previous = Some(value);
            index += 1;
        }
    }
    (total, first)
}

fn held_floor(times: &[f64], values: &[f64], hold_s: f64) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let dt = sample_dt(times);
    let durations: Vec<f64> = (0..values.len())
        .map(|index| {
            let step = if index + 1 < times.len() {
                times[index + 1] - times[index]
            } else {
                dt
            };
            py_max(0.0, py_min(step, 1.0))
        })
        .collect();
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*a].partial_cmp(&values[*b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut spent = 0.0;
    for index in order {
        spent += durations[index];
        if spent > 0.0 && spent >= hold_s {
            let level = values[index];
            let when = values
                .iter()
                .enumerate()
                .find(|(_, v)| **v <= level)
                .map(|(i, _)| times[i])?;
            return Some((level, when));
        }
    }
    None
}

const SAG_WINDOW_S: f64 = 10.0;

fn sag(times: &[f64], values: &[f64], window: f64) -> (f64, Option<f64>) {
    let (mut best, mut best_at) = (0.0, None);
    let mut highs: VecDeque<usize> = VecDeque::new();
    for (index, (&moment, &value)) in times.iter().zip(values).enumerate() {
        while highs.front().is_some_and(|h| times[*h] < moment - window) {
            highs.pop_front();
        }
        while highs.back().is_some_and(|h| values[*h] <= value) {
            highs.pop_back();
        }
        highs.push_back(index);
        let drop = values[highs[0]] - value;
        if drop > best {
            best = drop;
            best_at = Some(moment);
        }
    }
    (best, best_at)
}

fn within(times: &[f64], values: &[f64], spans: &Spans) -> (Vec<f64>, Vec<f64>) {
    if spans.is_empty() {
        return (times.to_vec(), values.to_vec());
    }
    let mut out_t = Vec::new();
    let mut out_v = Vec::new();
    let mut index = 0;
    for span in spans {
        while index < times.len() && times[index] < span.start {
            index += 1;
        }
        while index < times.len() && times[index] <= span.end {
            out_t.push(times[index]);
            out_v.push(values[index]);
            index += 1;
        }
    }
    (out_t, out_v)
}

fn align(times: &[f64], other_t: &[f64], other_v: &[f64], max_age: Option<f64>) -> Vec<f64> {
    let mut out = Vec::with_capacity(times.len());
    let mut cursor = 0;
    let mut last = f64::NAN;
    let mut last_t = f64::NEG_INFINITY;
    for &moment in times {
        while cursor < other_t.len() && other_t[cursor] <= moment {
            last = other_v[cursor];
            last_t = other_t[cursor];
            cursor += 1;
        }
        match max_age {
            Some(age) if moment - last_t > age => out.push(f64::NAN),
            _ => out.push(last),
        }
    }
    out
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut ordered = values.to_vec();
    ordered.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let last = (ordered.len() - 1) as f64;
    let position = py_max(0.0, py_min(last, round_int(fraction * last))) as usize;
    ordered[position]
}

fn correlate(a: &[f64], b: &[f64]) -> Option<f64> {
    let pairs: Vec<(f64, f64)> = a
        .iter()
        .zip(b)
        .filter(|(x, y)| !(**x).is_nan() && !(**y).is_nan())
        .map(|(x, y)| (*x, *y))
        .collect();
    if pairs.len() < 50 {
        return None;
    }
    let n = pairs.len() as f64;
    let mean_a = py_sum(&pairs.iter().map(|p| p.0).collect::<Vec<_>>()) / n;
    let mean_b = py_sum(&pairs.iter().map(|p| p.1).collect::<Vec<_>>()) / n;
    let var_a = py_sum(
        &pairs
            .iter()
            .map(|p| (p.0 - mean_a) * (p.0 - mean_a))
            .collect::<Vec<_>>(),
    );
    let var_b = py_sum(
        &pairs
            .iter()
            .map(|p| (p.1 - mean_b) * (p.1 - mean_b))
            .collect::<Vec<_>>(),
    );
    if var_a <= 0.0 || var_b <= 0.0 {
        return None;
    }
    let covariance = py_sum(
        &pairs
            .iter()
            .map(|p| (p.0 - mean_a) * (p.1 - mean_b))
            .collect::<Vec<_>>(),
    );
    Some(covariance / (var_a * var_b).sqrt())
}

fn oscillation(times: &[f64], values: &[f64]) -> Option<(f64, f64)> {
    if values.len() < 200 {
        return None;
    }
    let dt = sample_dt(times);
    if dt <= 0.0 {
        return None;
    }
    let rate = 1.0 / dt;
    if rate < 50.0 || times[times.len() - 1] - times[0] < 2.0 {
        return None;
    }
    let window = (round_int(0.15 * rate) as i64 | 1) as usize;
    if window < 3 || window >= values.len() {
        return None;
    }
    let half = window / 2;
    let mut running = py_sum(&values[..window]);
    let mut ac: Vec<f64> = Vec::new();
    for centre in half..values.len() - half {
        ac.push(values[centre] - running / window as f64);
        let leaving = centre - half;
        let entering = centre + half + 1;
        if entering < values.len() {
            running += values[entering] - values[leaving];
        }
    }
    if ac.len() < 100 {
        return None;
    }
    let rms = (py_sum(&ac.iter().map(|v| v * v).collect::<Vec<_>>()) / ac.len() as f64).sqrt();
    let crossings = (1..ac.len()).filter(|&i| (ac[i - 1] < 0.0) != (ac[i] < 0.0)).count();
    let seconds = (ac.len() - 1) as f64 * dt;
    if seconds <= 0.0 {
        return None;
    }
    Some((rms, crossings as f64 / (2.0 * seconds)))
}

pub struct Mode {
    pub mode: String,
    pub state: i128,
    pub start: f64,
    pub end: f64,
}

fn mode_at(modes: &[Mode], when: Option<f64>) -> String {
    let Some(when) = when else {
        return String::new();
    };
    for span in modes {
        if span.start <= when && when < span.end {
            return span.mode.clone();
        }
    }
    match modes.last() {
        Some(last) if when >= last.end => last.mode.clone(),
        _ => String::new(),
    }
}

fn finding(level: &str, text: &str, when: Option<f64>, modes: &[Mode], detail: &str) -> Value {
    let mut out = Map::new();
    out.insert("level".into(), level.into());
    out.insert("text".into(), text.into());
    if !detail.is_empty() {
        out.insert("detail".into(), detail.into());
    }
    if let Some(when) = when {
        out.insert("t".into(), num(round_to(when, 1)));
        let mode = mode_at(modes, Some(when));
        if !mode.is_empty() {
            out.insert("mode".into(), mode.into());
        }
    }
    Value::Object(out)
}

/// `{value:.Nf}` as Python writes it.
fn f(value: f64, digits: usize) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    format!("{value:.digits$}")
}

// ---------------------------------------------------------------------------
// The log as the review reads it
// ---------------------------------------------------------------------------

type Tv = (Vec<f64>, Vec<f64>);

pub struct Ctx<'a> {
    pub log: &'a ULog,
    base: i128,
}

fn advancing(stamps: &[i128]) -> bool {
    stamps.len() > 1 && stamps[stamps.len() - 1] > stamps[0]
}

fn stamps_of(col: &Col) -> Vec<i128> {
    (0..col.len()).map(|i| col.i128_at(i).unwrap_or(0)).collect()
}

impl<'a> Ctx<'a> {
    pub fn new(log: &'a ULog) -> Ctx<'a> {
        let mut best: Option<i128> = None;
        let mut fallback: Option<i128> = None;
        for instances in log.data.values() {
            for fields in instances.values() {
                let Some(col) = fields.get("timestamp") else {
                    continue;
                };
                if col.is_empty() {
                    continue;
                }
                let stamps = stamps_of(col);
                if fallback.is_none_or(|f| stamps[0] < f) {
                    fallback = Some(stamps[0]);
                }
                if advancing(&stamps) && best.is_none_or(|b| stamps[0] < b) {
                    best = Some(stamps[0]);
                }
            }
        }
        let base = best.or(fallback).unwrap_or(0);
        Ctx { log, base }
    }

    pub fn log_start(&self) -> i128 {
        self.base
    }

    fn col(&self, topic: &str, field: &str, multi: u8) -> Option<&'a Col> {
        self.log.series(topic, field, multi).filter(|c| !c.is_empty())
    }

    fn first_topic(&self, names: &[&'static str]) -> Option<&'static str> {
        names.iter().copied().find(|n| self.log.has(n))
    }

    fn first_field(&self, topic: &str, names: &[&'static str], multi: u8) -> Option<&'static str> {
        let fields = self.log.fields(topic, multi)?;
        names
            .iter()
            .copied()
            .find(|n| fields.get(*n).is_some_and(|c| !c.is_empty()))
    }

    fn time_axis(&self, topic: &str, multi: u8) -> Vec<f64> {
        let Some(col) = self.col(topic, "timestamp", multi) else {
            return Vec::new();
        };
        match col {
            Col::Int(v) => v.iter().map(|t| (*t - self.base) as f64 / 1e6).collect(),
            _ => (0..col.len())
                .map(|i| (col.f64_at(i).unwrap_or(f64::NAN) - self.base as f64) / 1e6)
                .collect(),
        }
    }

    fn stamps(&self, topic: &str, multi: u8) -> Vec<i128> {
        self.col(topic, "timestamp", multi).map(stamps_of).unwrap_or_default()
    }

    fn series(
        &self,
        topic: &str,
        field: &str,
        name: &str,
        multi: u8,
        scale: f64,
        transform: Option<fn(f64) -> f64>,
    ) -> Option<Value> {
        let col = self.col(topic, field, multi)?;
        let mut times = self.time_axis(topic, multi);
        let mut values: Vec<f64> = floats(col);
        if times.len() != values.len() {
            let span = times.len().min(values.len());
            times.truncate(span);
            values.truncate(span);
        }
        let numeric: Vec<f64> = values
            .into_iter()
            .map(|mut number| {
                if let Some(t) = transform {
                    if !(number).is_nan() {
                        number = t(number);
                    }
                }
                number * scale
            })
            .collect();
        let (t, v) = decimate(&times, &numeric);
        Some(series_value(name, &t, &v, 5))
    }

    fn s(&self, topic: &str, field: &str, name: &str) -> Option<Value> {
        self.series(topic, field, name, 0, 1.0, None)
    }

    fn series_from_column(&self, topic: &str, column: &[f64], name: &str, multi: u8) -> Value {
        let times = self.time_axis(topic, multi);
        let span = times.len().min(column.len());
        let (t, v) = decimate(&times[..span], &column[..span]);
        series_value(name, &t, &v, 5)
    }

    fn vector(&self, topic: &str, field: &str, labels: &[&str], multi: u8, scale: f64) -> Vec<Option<Value>> {
        let Some(Col::Arr(values)) = self.col(topic, field, multi) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            if index >= values[0].len() {
                break;
            }
            let column: Vec<f64> = values
                .iter()
                .map(|row| {
                    if index < row.len() {
                        row[index] * scale
                    } else {
                        f64::NAN
                    }
                })
                .collect();
            out.push(Some(self.series_from_column(topic, &column, label, multi)));
        }
        out
    }

    fn rows(&self, topic: &str, field: &str, multi: u8) -> Option<&'a Vec<Vec<f64>>> {
        match self.col(topic, field, multi) {
            Some(Col::Arr(v)) => Some(v),
            _ => None,
        }
    }

    // -------------------------------------------------------------- raw

    fn raw(&self, topic: &str, field: &str, multi: u8, scale: f64) -> Tv {
        let Some(col) = self.col(topic, field, multi) else {
            return (Vec::new(), Vec::new());
        };
        let times = self.time_axis(topic, multi);
        let span = times.len().min(col.len());
        let (mut out_t, mut out_v) = (Vec::new(), Vec::new());
        for index in 0..span {
            let Some(number) = col.f64_at(index) else {
                continue;
            };
            if (number).is_nan() {
                continue;
            }
            out_t.push(times[index]);
            out_v.push(number * scale);
        }
        (out_t, out_v)
    }

    fn raw0(&self, topic: &str, field: &str) -> Tv {
        self.raw(topic, field, 0, 1.0)
    }

    fn raw_component(&self, topic: &str, field: &str, index: usize, multi: u8, scale: f64) -> Tv {
        let Some(rows) = self.rows(topic, field, multi) else {
            return (Vec::new(), Vec::new());
        };
        let times = self.time_axis(topic, multi);
        let span = times.len().min(rows.len());
        let (mut out_t, mut out_v) = (Vec::new(), Vec::new());
        for (row_index, row) in rows.iter().enumerate().take(span) {
            if index >= row.len() {
                continue;
            }
            let number = row[index];
            if (number).is_nan() {
                continue;
            }
            out_t.push(times[row_index]);
            out_v.push(number * scale);
        }
        (out_t, out_v)
    }

    // ------------------------------------------------------------ plots

    fn plot_actuators(&self) -> Option<Value> {
        let topic = self.first_topic(&["actuator_motors", "actuator_outputs"])?;
        let mut series: Vec<Option<Value>> = Vec::new();
        let unit;
        if topic == "actuator_motors" {
            let values = self.rows(topic, "control", 0)?;
            let width = 8.min(values[0].len());
            for index in 0..width {
                let column: Vec<f64> = values
                    .iter()
                    .map(|row| if index < row.len() { row[index] } else { f64::NAN })
                    .collect();
                if column.iter().all(|v| (*v).is_nan() || *v <= 0.0) {
                    continue;
                }
                series.push(Some(self.series_from_column(
                    topic,
                    &column,
                    &format!("Motor {}", index + 1),
                    0,
                )));
            }
            unit = "normalised";
        } else {
            let values = self.rows(topic, "output", 0)?;
            let width = match self.col(topic, "noutputs", 0) {
                Some(c) => c.i128_at(0).unwrap_or(0),
                None => values[0].len() as i128,
            };
            let width = width.min(8).min(values[0].len() as i128).max(0) as usize;
            for index in 0..width {
                let column: Vec<f64> = values
                    .iter()
                    .map(|row| if index < row.len() { row[index] } else { f64::NAN })
                    .collect();
                let finite: Vec<f64> = column.iter().copied().filter(|v| !(*v).is_nan()).collect();
                if finite.is_empty() || finite.iter().all(|v| *v == 0.0) {
                    continue;
                }
                series.push(Some(self.series_from_column(
                    topic,
                    &column,
                    &format!("Output {}", index + 1),
                    0,
                )));
            }
            unit = "µs";
        }
        let mut text =
            "A motor sitting at its limit while the others do not is an imbalance. Check the airframe, not the tuning.";
        let flat = series.iter().flatten().all(|s| {
            let mut distinct: Vec<f64> = Vec::new();
            for y in ys(s).into_iter().flatten() {
                if !distinct.contains(&y) {
                    distinct.push(y);
                }
            }
            distinct.len() <= 1
        });
        if !series.is_empty() && flat {
            text = "The outputs never changed. The motors did not run in this log.";
        }
        plot("actuators", "Motor outputs", unit, series, "Airframe", note(text))
    }

    fn clip_column(&self, topic: &str, multi: u8) -> Option<Vec<f64>> {
        if let Some(rows) = self.rows(topic, "accel_clipping", multi) {
            return Some(rows.iter().map(|r| py_sum(r)).collect());
        }
        let counter = self.first_field(topic, &["accel_clipping_count"], multi)?;
        Some(floats(self.col(topic, counter, multi)?))
    }

    fn plot_clipping(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_imu_status", "sensor_accel_status"])?;
        let mut series = Vec::new();
        for multi in self.log.instances(topic).into_iter().take(3) {
            let Some(total) = self.clip_column(topic, multi) else {
                continue;
            };
            if total.is_empty() || max_of(&total) <= 0.0 {
                continue;
            }
            let times = self.time_axis(topic, multi);
            let span = times.len().min(total.len());
            let (t, v) = decimate(&times[..span], &total[..span]);
            series.push(Some(series_value(&format!("IMU {multi}"), &t, &v, 1)));
        }
        plot(
            "clipping",
            "Accelerometer clipping",
            "clipped samples (cumulative)",
            series,
            "Sensors",
            note("Any rise at all means the accelerometer saturated. soften the flight-controller mounting before trusting the rest of this log."),
        )
    }

    fn plot_vibration(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_imu_status", "estimator_status"])?;
        let mut series = Vec::new();
        if topic == "vehicle_imu_status" {
            for multi in self.log.instances(topic).into_iter().take(3) {
                for (field, label) in [("accel_vibration_metric", "Accel"), ("gyro_vibration_metric", "Gyro")] {
                    if let Some(found) = self.first_field(topic, &[field], multi) {
                        series.push(self.series(topic, found, &format!("{label} IMU {multi}"), multi, 1.0, None));
                    }
                }
            }
        } else {
            series = self.vector(
                topic,
                "vibe",
                &["Delta angle coning", "Delta angle", "Delta velocity"],
                0,
                1.0,
            );
        }
        plot(
            "vibration",
            "Vibration metrics",
            "m/s² · rad/s",
            series,
            "Sensors",
            note("Read against the thrust plot: a level that climbs with throttle is a propeller or motor problem, not a tuning one. PX4 treats roughly 30 as the point to act on and 60 as the point the estimator suffers."),
        )
    }

    fn plot_ekf(&self) -> Option<Value> {
        if !self.log.has("estimator_status") {
            return None;
        }
        let mut series = Vec::new();
        for (field, label) in INNOVATIONS {
            if let Some(found) = self.first_field("estimator_status", &[field], 0) {
                series.push(self.s("estimator_status", found, label));
            }
        }
        plot(
            "ekf",
            "EKF innovation test ratios",
            "ratio",
            series,
            "Estimator",
            vec![
                ("threshold", json!(1.0)),
                ("note", "1.0 is the rejection threshold: above it the estimator is discarding that sensor. Brief spikes are normal, a sustained excursion is not.".into()),
            ],
        )
    }

    fn euler(&self) -> Option<(&'static str, Vec<f64>, Vec<f64>, Vec<f64>)> {
        let topic = self.first_topic(&["vehicle_attitude", "control_state"])?;
        let quats = self.rows(topic, "q", 0)?;
        let (mut roll, mut pitch, mut yaw) = (Vec::new(), Vec::new(), Vec::new());
        for q in quats {
            if q.len() < 4 {
                roll.push(f64::NAN);
                pitch.push(f64::NAN);
                yaw.push(f64::NAN);
                continue;
            }
            let (w, x, y, z) = (q[0], q[1], q[2], q[3]);
            roll.push((2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y)).to_degrees());
            pitch.push(py_max(-1.0, py_min(1.0, 2.0 * (w * y - z * x))).asin().to_degrees());
            yaw.push((2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees());
        }
        Some((topic, roll, pitch, yaw))
    }

    fn attitude_plot(&self, axis: &str) -> Option<Value> {
        let (topic, roll, pitch, yaw) = self.euler()?;
        let values = match axis {
            "roll" => &roll,
            "pitch" => &pitch,
            _ => &yaw,
        };
        let label = capitalize(axis);
        let mut series = vec![Some(self.series_from_column(
            topic,
            values,
            &format!("{label} estimate"),
            0,
        ))];
        if let Some(setpoint) = self.first_topic(&["vehicle_attitude_setpoint"]) {
            let body = format!("{axis}_body");
            let names: [&str; 2] = [leak(&body), leak(axis)];
            if let Some(found) = self.first_field(setpoint, &names, 0) {
                series.push(self.series(
                    setpoint,
                    found,
                    &format!("{label} setpoint"),
                    0,
                    1.0,
                    Some(f64::to_degrees),
                ));
            }
        }
        let mut text =
            "The estimate should sit on the setpoint. A steady offset is trim or tuning; a sudden divergence is not.";
        if axis == "yaw" {
            text = "Yaw wraps at ±180°, so a jump between the two is the wrap, not the aircraft. Watch the gap between the pair instead.";
        }
        plot(
            &format!("att_{axis}"),
            &format!("{label} angle"),
            "degrees",
            series,
            "Control",
            note(text),
        )
    }

    fn rate_plot(&self, axis: &str) -> Option<Value> {
        let index = axis_index(axis);
        let label = capitalize(axis);
        let topic = self.first_topic(&["vehicle_angular_velocity", "sensor_combined"])?;
        let field = if topic == "vehicle_angular_velocity" {
            "xyz"
        } else {
            "gyro_rad"
        };
        let values = self.rows(topic, field, 0)?;
        if values[0].len() <= index {
            return None;
        }
        let column: Vec<f64> = values
            .iter()
            .map(|row| {
                if index < row.len() {
                    row[index] * 180.0 / std::f64::consts::PI
                } else {
                    f64::NAN
                }
            })
            .collect();
        let mut series = vec![Some(self.series_from_column(
            topic,
            &column,
            &format!("{label} rate"),
            0,
        ))];
        if let Some(setpoint) = self.first_topic(&["vehicle_rates_setpoint"]) {
            if let Some(found) = self.first_field(setpoint, &[leak(axis)], 0) {
                series.push(self.series(
                    setpoint,
                    found,
                    &format!("{label} rate setpoint"),
                    0,
                    180.0 / std::f64::consts::PI,
                    None,
                ));
            }
        }
        plot(
            &format!("rate_{axis}"),
            &format!("{label} angular rate"),
            "deg/s",
            series,
            "Control",
            note("Lag behind the setpoint means the gains are low; overshoot and ringing mean they are high."),
        )
    }

    fn local_pair(
        &self,
        id: &str,
        title: &str,
        field: &'static str,
        unit: &str,
        scale: f64,
        text: &str,
    ) -> Option<Value> {
        if !self.log.has("vehicle_local_position") {
            return None;
        }
        let found = self.first_field("vehicle_local_position", &[field], 0)?;
        let mut series = vec![self.series("vehicle_local_position", found, "Estimate", 0, scale, None)];
        if let Some(setpoint) = self.first_topic(&["vehicle_local_position_setpoint"]) {
            if let Some(target) = self.first_field(setpoint, &[field], 0) {
                series.push(self.series(setpoint, target, "Setpoint", 0, scale, None));
            }
        }
        plot(id, title, unit, series, "Flight", note(text))
    }

    fn plot_manual_control(&self) -> Option<Value> {
        let topic = self.first_topic(&["manual_control_setpoint", "manual_control_input"])?;
        let mut series = Vec::new();
        for (names, label) in [
            (["roll", "y"], "Roll stick"),
            (["pitch", "x"], "Pitch stick"),
            (["yaw", "r"], "Yaw stick"),
            (["throttle", "z"], "Throttle"),
        ] {
            if let Some(found) = self.first_field(topic, &names, 0) {
                series.push(self.s(topic, found, label));
            }
        }
        plot(
            "manual",
            "Manual control input",
            "normalised",
            series,
            "Control",
            note("The pilot's side of every control plot above. A rate that only misbehaves while a stick is moving is a different problem from one that misbehaves on its own."),
        )
    }

    fn plot_altitude(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_local_position", "vehicle_global_position"])?;
        let mut series = Vec::new();
        if topic == "vehicle_local_position" {
            if let Some(field) = self.first_field(topic, &["z"], 0) {
                series.push(self.series(topic, field, "Altitude (local)", 0, -1.0, None));
            }
        } else if let Some(field) = self.first_field(topic, &["alt"], 0) {
            series.push(self.s(topic, field, "Altitude (AMSL)"));
        }
        plot("altitude", "Altitude", "m", series, "Flight", vec![])
    }

    fn plot_battery(&self) -> Option<Value> {
        if !self.log.has("battery_status") {
            return None;
        }
        let voltage = self.first_field("battery_status", &["voltage_filtered_v", "voltage_v"], 0);
        let current = self.first_field("battery_status", &["current_filtered_a", "current_a"], 0);
        let mut series = Vec::new();
        if let Some(v) = voltage {
            series.push(self.s("battery_status", v, "Voltage"));
        }
        if let Some(c) = current {
            series.push(self.s("battery_status", c, "Current"));
        }
        plot(
            "battery",
            "Battery",
            "V / A",
            series,
            "System",
            note("A voltage that dives under throttle and recovers is a tired pack, and it is the reading that decides whether the next flight is a good idea."),
        )
    }

    fn plot_cpu(&self) -> Option<Value> {
        if !self.log.has("cpuload") {
            return None;
        }
        let series = vec![
            self.series("cpuload", "load", "CPU load", 0, 100.0, None),
            self.series("cpuload", "ram_usage", "RAM used", 0, 100.0, None),
        ];
        plot("cpu", "Processor", "%", series, "System", vec![])
    }

    fn plot_gps(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_gps_position", "sensor_gps"])?;
        let mut series = Vec::new();
        if let Some(sats) = self.first_field(topic, &["satellites_used"], 0) {
            series.push(self.s(topic, sats, "Satellites"));
        }
        if let Some(eph) = self.first_field(topic, &["eph"], 0) {
            series.push(self.s(topic, eph, "Horizontal accuracy (m)"));
        }
        plot("gps", "GPS", "count / m", series, "Sensors", vec![])
    }
}

/// `max(values)` of a non-empty list: the first, replaced by anything larger.
fn max_of(values: &[f64]) -> f64 {
    let mut best = values[0];
    for &v in &values[1..] {
        if v > best {
            best = v;
        }
    }
    best
}

/// `min(values)` of a non-empty list.
fn min_of(values: &[f64]) -> f64 {
    let mut best = values[0];
    for &v in &values[1..] {
        if v < best {
            best = v;
        }
    }
    best
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn axis_index(axis: &str) -> usize {
    match axis {
        "roll" => 0,
        "pitch" => 1,
        _ => 2,
    }
}

/// A `&'static str` for a name built at run time; the handful of axis names
/// is all this ever leaks.
fn leak(s: &str) -> &'static str {
    use std::collections::HashSet;
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut guard = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if let Some(found) = set.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

const INNOVATIONS: [(&str, &str); 5] = [
    ("mag_test_ratio", "Magnetometer"),
    ("vel_test_ratio", "Velocity"),
    ("pos_test_ratio", "Position"),
    ("hgt_test_ratio", "Height"),
    ("tas_test_ratio", "Airspeed"),
];

// ---------------------------------------------------------------------------
// Ground track, and the rest of the plots
// ---------------------------------------------------------------------------

const EARTH_RADIUS_M: f64 = 6371000.0;

struct Projection {
    sin_lat: f64,
    cos_lat: f64,
    lon: f64,
    lat_deg: f64,
    lon_deg: f64,
}

impl Projection {
    fn new(ref_lat_deg: f64, ref_lon_deg: f64) -> Projection {
        let lat = ref_lat_deg.to_radians();
        Projection {
            sin_lat: lat.sin(),
            cos_lat: lat.cos(),
            lon: ref_lon_deg.to_radians(),
            lat_deg: ref_lat_deg,
            lon_deg: ref_lon_deg,
        }
    }

    fn project(&self, lat_deg: f64, lon_deg: f64) -> (f64, f64) {
        let lat = lat_deg.to_radians();
        let d_lon = lon_deg.to_radians() - self.lon;
        let sin_lat = lat.sin();
        let cos_lat = lat.cos();
        let cos_d_lon = d_lon.cos();
        let arg = py_max(
            -1.0,
            py_min(1.0, self.sin_lat * sin_lat + self.cos_lat * cos_lat * cos_d_lon),
        );
        let angle = arg.acos();
        let scale = if angle.abs() > 1e-12 { angle / angle.sin() } else { 1.0 };
        let north = scale * (self.cos_lat * sin_lat - self.sin_lat * cos_lat * cos_d_lon) * EARTH_RADIUS_M;
        let east = scale * cos_lat * d_lon.sin() * EARTH_RADIUS_M;
        (north, east)
    }
}

fn degrees(value: f64) -> Option<f64> {
    let mut number = value;
    if (number).is_nan() {
        return None;
    }
    if number.abs() > 180.0 {
        number /= 1e7;
    }
    if number.abs() <= 180.0 {
        Some(number)
    } else {
        None
    }
}

struct Path {
    xs: Vec<f64>,
    ys: Vec<f64>,
}

impl<'a> Ctx<'a> {
    fn local_origin(&self) -> Option<Projection> {
        if !self.log.has("vehicle_local_position") {
            return None;
        }
        let fields = self.log.fields("vehicle_local_position", 0)?;
        let lats = fields.get("ref_lat");
        let lons = fields.get("ref_lon");
        let valid = fields.get("xy_global");
        let n = lats.map_or(0, Col::len).min(lons.map_or(0, Col::len));
        for index in 0..n {
            if let Some(v) = valid {
                if index < v.len() && v.truthy_at(index) == Some(false) {
                    continue;
                }
            }
            let (Some(lat), Some(lon)) = (lats?.f64_at(index), lons?.f64_at(index)) else {
                continue;
            };
            if lat == 0.0 && lon == 0.0 {
                continue;
            }
            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
                return Some(Projection::new(lat, lon));
            }
        }
        None
    }

    fn xy_path(&self, topic: &str, north_field: &str, east_field: &str) -> Option<Path> {
        let north = self.col(topic, north_field, 0);
        let east = self.col(topic, east_field, 0);
        let span = north.map_or(0, Col::len).min(east.map_or(0, Col::len));
        if span < 2 {
            return None;
        }
        let (north, east) = (north?, east?);
        let step = 1.max(span / MAX_POINTS);
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        let mut index = 0;
        while index < span {
            if let (Some(x), Some(y)) = (east.f64_at(index), north.f64_at(index)) {
                if !(x).is_nan() && !(y).is_nan() {
                    xs.push(round_to(x, 2));
                    ys.push(round_to(y, 2));
                }
            }
            index += step;
        }
        if xs.len() < 2 {
            return None;
        }
        Some(Path { xs, ys })
    }

    fn gps_path(&self, projection: &Projection) -> Option<Path> {
        let topic = self.first_topic(&["vehicle_gps_position", "sensor_gps"])?;
        let lats = self.col(topic, "lat", 0);
        let lons = self.col(topic, "lon", 0);
        let fixes = self.col(topic, "fix_type", 0);
        let span = lats.map_or(0, Col::len).min(lons.map_or(0, Col::len));
        if span < 2 {
            return None;
        }
        let (lats, lons) = (lats?, lons?);
        let step = 1.max(span / MAX_POINTS);
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        let mut index = 0;
        while index < span {
            let skip = fixes.is_some_and(|f| {
                index < f.len()
                    && f.f64_at(index)
                        .map(|v| if v == 0.0 { 0.0 } else { v })
                        .is_some_and(|v| v < 3.0)
            });
            if !skip {
                let lat = lats.f64_at(index).and_then(degrees);
                let lon = lons.f64_at(index).and_then(degrees);
                if let (Some(lat), Some(lon)) = (lat, lon) {
                    if !(lat == 0.0 && lon == 0.0) {
                        let (north, east) = projection.project(lat, lon);
                        xs.push(round_to(east, 2));
                        ys.push(round_to(north, 2));
                    }
                }
            }
            index += step;
        }
        if xs.len() < 2 {
            return None;
        }
        Some(Path { xs, ys })
    }

    fn waypoints(&self, projection: Option<&Projection>) -> Option<Value> {
        if !self.log.has("position_setpoint_triplet") {
            return None;
        }
        let fields = self.log.fields("position_setpoint_triplet", 0)?;
        let valid = fields.get("current.valid").filter(|c| !c.is_empty());
        let (mut xs, mut ys): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
        let mut last: Option<(f64, f64)> = None;
        let mut add = |east: f64, north: f64| {
            let point = (round_to(east, 2), round_to(north, 2));
            if Some(point) == last {
                return;
            }
            last = Some(point);
            xs.push(point.0);
            ys.push(point.1);
        };
        let skip = |index: usize| valid.is_some_and(|v| index < v.len() && v.truthy_at(index) == Some(false));
        let lats = fields.get("current.lat").filter(|c| !c.is_empty());
        let lons = fields.get("current.lon").filter(|c| !c.is_empty());
        if let (Some(projection), Some(lats), Some(lons)) = (projection, lats, lons) {
            for index in 0..lats.len().min(lons.len()) {
                if skip(index) {
                    continue;
                }
                let lat = lats.f64_at(index).and_then(degrees);
                let lon = lons.f64_at(index).and_then(degrees);
                let (Some(lat), Some(lon)) = (lat, lon) else {
                    continue;
                };
                if lat == 0.0 && lon == 0.0 {
                    continue;
                }
                let (north, east) = projection.project(lat, lon);
                add(east, north);
            }
        } else {
            let norths = fields.get("current.x").filter(|c| !c.is_empty());
            let easts = fields.get("current.y").filter(|c| !c.is_empty());
            let n = norths.map_or(0, Col::len).min(easts.map_or(0, Col::len));
            for index in 0..n {
                if skip(index) {
                    continue;
                }
                if let (Some(e), Some(nn)) = (easts?.f64_at(index), norths?.f64_at(index)) {
                    add(e, nn);
                }
            }
        }
        if xs.is_empty() {
            return None;
        }
        Some(
            json!({"name": "Commanded position", "x": xs.iter().map(|v| num(*v)).collect::<Vec<_>>(),
                    "y": ys.iter().map(|v| num(*v)).collect::<Vec<_>>(), "draw": "markers"}),
        )
    }

    fn plot_track(&self) -> Option<Value> {
        if !self.log.has("vehicle_local_position") {
            return None;
        }
        let estimate = self.xy_path("vehicle_local_position", "x", "y")?;
        if max_of(&estimate.xs) - min_of(&estimate.xs) < 0.5 && max_of(&estimate.ys) - min_of(&estimate.ys) < 0.5 {
            return None;
        }
        let projection = self.local_origin();
        let path_value = |name: &str, p: Path| {
            json!({"name": name, "x": p.xs.iter().map(|v| num(*v)).collect::<Vec<_>>(),
                   "y": p.ys.iter().map(|v| num(*v)).collect::<Vec<_>>()})
        };
        let series = vec![
            projection
                .as_ref()
                .and_then(|p| self.gps_path(p))
                .map(|p| path_value("GPS (projected)", p)),
            self.xy_path("vehicle_local_position_setpoint", "x", "y")
                .map(|p| path_value("Setpoint", p)),
            Some(path_value("Estimated", estimate)),
            self.waypoints(projection.as_ref()),
        ];
        let mut text = String::from(
            "Local position, north up, equal axes. Origin is where the estimator started, not a survey point.",
        );
        if projection.is_some() {
            text.push_str(" GPS is projected through the estimator's own origin, so a gap between the two tracks is estimator error and not a difference of frames.");
        } else {
            text.push_str(" This flight had no global reference, so there is no GPS track to compare against.");
        }
        let mut extra = vec![
            ("xlabel", Value::from("East (m)")),
            ("ylabel", Value::from("North (m)")),
            ("equal", Value::from(true)),
            ("note", Value::from(text)),
        ];
        if let Some(p) = &projection {
            extra.push((
                "origin",
                json!({"lat": num(round_to(p.lat_deg, 7)), "lon": num(round_to(p.lon_deg, 7))}),
            ));
        }
        plot("track", "Ground track", "m", series, "Flight", extra)
    }

    fn plot_velocity(&self) -> Option<Value> {
        if !self.log.has("vehicle_local_position") {
            return None;
        }
        let mut series = Vec::new();
        let vx = self.col("vehicle_local_position", "vx", 0);
        let vy = self.col("vehicle_local_position", "vy", 0);
        if let (Some(vx), Some(vy)) = (vx, vy) {
            let span = vx.len().min(vy.len());
            let ground: Vec<f64> = (0..span)
                .map(|i| vx.f64_at(i).unwrap_or(f64::NAN).hypot(vy.f64_at(i).unwrap_or(f64::NAN)))
                .collect();
            series.push(Some(self.series_from_column(
                "vehicle_local_position",
                &ground,
                "Ground speed",
                0,
            )));
        }
        if let Some(vz) = self.first_field("vehicle_local_position", &["vz"], 0) {
            series.push(self.series("vehicle_local_position", vz, "Climb rate", 0, -1.0, None));
        }
        plot("velocity", "Speed", "m/s", series, "Flight", vec![])
    }

    fn plot_thrust(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_thrust_setpoint", "actuator_controls_0"])?;
        let mut series = Vec::new();
        if topic == "vehicle_thrust_setpoint" {
            if let Some(values) = self.rows(topic, "xyz", 0) {
                if values[0].len() >= 3 {
                    let column: Vec<f64> = values.iter().map(|r| r[2].abs()).collect();
                    series.push(Some(self.series_from_column(topic, &column, "Thrust", 0)));
                }
            }
        } else if let Some(values) = self.rows(topic, "control", 0) {
            if values[0].len() >= 4 {
                let column: Vec<f64> = values.iter().map(|r| r[3]).collect();
                series.push(Some(self.series_from_column(topic, &column, "Throttle", 0)));
            }
        }
        plot(
            "thrust",
            "Thrust demand",
            "normalised",
            series,
            "Control",
            note("Read the vibration and current plots against this: both are expected to rise with thrust."),
        )
    }

    fn plot_mag(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_magnetometer", "sensor_mag"])?;
        let mut series = Vec::new();
        for multi in self.log.instances(topic).into_iter().take(3) {
            let norm: Vec<f64>;
            if let Some(values) = self.rows(topic, "magnetometer_ga", multi).filter(|v| v[0].len() >= 3) {
                norm = values
                    .iter()
                    .map(|row| py_sum(&row[..3].iter().map(|c| c * c).collect::<Vec<_>>()).sqrt())
                    .collect();
            } else {
                let (Some(x), Some(y), Some(z)) = (
                    self.col(topic, "x", multi),
                    self.col(topic, "y", multi),
                    self.col(topic, "z", multi),
                ) else {
                    continue;
                };
                let span = x.len().min(y.len()).min(z.len());
                norm = (0..span)
                    .map(|i| {
                        let (a, b, c) = (
                            x.f64_at(i).unwrap_or(f64::NAN),
                            y.f64_at(i).unwrap_or(f64::NAN),
                            z.f64_at(i).unwrap_or(f64::NAN),
                        );
                        (a * a + b * b + c * c).sqrt()
                    })
                    .collect();
            }
            series.push(Some(self.series_from_column(
                topic,
                &norm,
                &format!("Mag {multi}"),
                multi,
            )));
        }
        plot(
            "mag",
            "Magnetic field strength",
            "gauss",
            series,
            "Sensors",
            note("A flat line is what you want. Movement that tracks throttle is interference from the power wiring, and it is what makes a compass calibration not stick."),
        )
    }

    fn plot_imu_temp(&self) -> Option<Value> {
        if !self.log.has("vehicle_imu_status") {
            return None;
        }
        let mut series = Vec::new();
        for multi in self.log.instances("vehicle_imu_status").into_iter().take(3) {
            if let Some(field) =
                self.first_field("vehicle_imu_status", &["temperature_accel", "temperature_gyro"], multi)
            {
                series.push(self.series("vehicle_imu_status", field, &format!("IMU {multi}"), multi, 1.0, None));
            }
        }
        plot(
            "imu_temp",
            "IMU temperature",
            "°C",
            series,
            "Sensors",
            note("A sensor still warming up drifts. A cold first flight of the day is where that shows."),
        )
    }

    fn plot_bias(&self) -> Option<Value> {
        let topic = self.first_topic(&["estimator_sensor_bias", "estimator_states"])?;
        if topic != "estimator_sensor_bias" {
            return None;
        }
        let series = self.vector(
            topic,
            "gyro_bias",
            &["Gyro X", "Gyro Y", "Gyro Z"],
            0,
            180.0 / std::f64::consts::PI,
        );
        plot(
            "bias",
            "Estimated gyro bias",
            "deg/s",
            series,
            "Estimator",
            note("Bias should settle and stay put. One that keeps walking is a gyro worth replacing."),
        )
    }

    fn plot_rc(&self) -> Option<Value> {
        let topic = self.first_topic(&["input_rc", "rc_channels"])?;
        let mut series = Vec::new();
        if let Some(rssi) = self.first_field(topic, &["rssi", "rssi_dbm"], 0) {
            series.push(self.s(topic, rssi, "RC signal"));
        }
        if let Some(lost) = self.first_field(topic, &["rc_lost", "signal_lost"], 0) {
            series.push(self.s(topic, lost, "Signal lost (1 = yes)"));
        }
        plot(
            "rc",
            "RC link",
            "rssi / flag",
            series,
            "System",
            note("A signal-lost flag that goes high in flight is a failsafe waiting to happen."),
        )
    }

    fn plot_airspeed(&self) -> Option<Value> {
        if !self.log.has("airspeed") {
            return None;
        }
        let mut series = Vec::new();
        for (field, label) in [("indicated_airspeed_m_s", "Indicated"), ("true_airspeed_m_s", "True")] {
            if let Some(found) = self.first_field("airspeed", &[field], 0) {
                series.push(self.s("airspeed", found, label));
            }
        }
        plot("airspeed", "Airspeed", "m/s", series, "Flight", vec![])
    }

    fn plot_distance(&self) -> Option<Value> {
        if !self.log.has("distance_sensor") {
            return None;
        }
        let mut series = Vec::new();
        for multi in self.log.instances("distance_sensor").into_iter().take(2) {
            if let Some(field) = self.first_field("distance_sensor", &["current_distance"], multi) {
                series.push(self.series(
                    "distance_sensor",
                    field,
                    &format!("Rangefinder {multi}"),
                    multi,
                    1.0,
                    None,
                ));
            }
        }
        plot("distance", "Rangefinder", "m", series, "Sensors", vec![])
    }

    fn plot_wind(&self) -> Option<Value> {
        let topic = self.first_topic(&["wind", "wind_estimate"])?;
        let north = self.col(topic, "windspeed_north", 0)?;
        let east = self.col(topic, "windspeed_east", 0)?;
        let span = north.len().min(east.len());
        let speed: Vec<f64> = (0..span)
            .map(|i| {
                north
                    .f64_at(i)
                    .unwrap_or(f64::NAN)
                    .hypot(east.f64_at(i).unwrap_or(f64::NAN))
            })
            .collect();
        plot(
            "wind",
            "Estimated wind",
            "m/s",
            vec![Some(self.series_from_column(topic, &speed, "Wind speed", 0))],
            "Flight",
            note(
                "Estimated, not measured, but a climbing estimate explains a lot of otherwise puzzling tracking error.",
            ),
        )
    }

    fn plot_power(&self) -> Option<Value> {
        if !self.log.has("battery_status") {
            return None;
        }
        let mut series = Vec::new();
        if let Some(remaining) = self.first_field("battery_status", &["remaining"], 0) {
            series.push(self.series("battery_status", remaining, "Remaining", 0, 100.0, None));
        }
        if let Some(used) = self.first_field("battery_status", &["discharged_mah"], 0) {
            series.push(self.s("battery_status", used, "Discharged (mAh)"));
        }
        plot("power", "Battery state", "% / mAh", series, "System", vec![])
    }

    fn plot_gps_quality(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_gps_position", "sensor_gps"])?;
        let mut series = Vec::new();
        if let Some(found) = self.first_field(topic, &["fix_type"], 0) {
            series.push(self.s(topic, found, "Fix type"));
        }
        plot(
            "gps_quality",
            "GPS fix type",
            "fix",
            series,
            "Sensors",
            note("Below 3 there is no 3-D fix, and every position plot above is running on dead reckoning."),
        )
    }

    fn reference_altitude(&self) -> Option<f64> {
        let col = self.col("vehicle_local_position", "ref_alt", 0)?;
        for i in 0..col.len() {
            let Some(number) = col.f64_at(i) else {
                continue;
            };
            if -2000.0 < number && number < 20000.0 {
                return Some(number);
            }
        }
        None
    }

    fn plot_altitude_sources(&self) -> Option<Value> {
        let mut series: Vec<Option<Value>> = Vec::new();
        let reference = self.reference_altitude();

        if let Some(gps_topic) = self.first_topic(&["vehicle_gps_position", "sensor_gps"]) {
            if let Some(field) = self.first_field(gps_topic, &["alt"], 0) {
                let values = self.col(gps_topic, field, 0);
                let scale = if values.is_some_and(Col::is_int) { 1e-3 } else { 1.0 };
                series.push(self.series(gps_topic, field, "GPS altitude (MSL)", 0, scale, None));
            }
        }
        if let Some(baro) = self.first_topic(&["vehicle_air_data", "sensor_baro"]) {
            if let Some(field) = self.first_field(baro, &["baro_alt_meter", "altitude"], 0) {
                series.push(self.s(baro, field, "Barometer altitude"));
            }
        }
        if self.log.has("vehicle_global_position") {
            if let Some(field) = self.first_field("vehicle_global_position", &["alt"], 0) {
                series.push(self.s("vehicle_global_position", field, "Fused estimate"));
            }
        } else if self.log.has("vehicle_local_position") {
            let z = self.col("vehicle_local_position", "z", 0);
            let r = self.col("vehicle_local_position", "ref_alt", 0);
            if let (Some(z), Some(r)) = (z, r) {
                let span = z.len().min(r.len());
                let amsl: Vec<f64> = (0..span)
                    .map(|i| r.f64_at(i).unwrap_or(f64::NAN) - z.f64_at(i).unwrap_or(f64::NAN))
                    .collect();
                series.push(Some(self.series_from_column(
                    "vehicle_local_position",
                    &amsl,
                    "Fused estimate",
                    0,
                )));
            }
        }
        if let (Some(setpoint), Some(reference)) = (self.first_topic(&["vehicle_local_position_setpoint"]), reference) {
            if let Some(z) = self.col(setpoint, "z", 0) {
                let amsl: Vec<f64> = (0..z.len())
                    .map(|i| {
                        let v = z.f64_at(i).unwrap_or(f64::NAN);
                        if -1e5 < v && v < 1e5 {
                            reference - v
                        } else {
                            f64::NAN
                        }
                    })
                    .collect();
                let mut point = self.series_from_column(setpoint, &amsl, "Altitude setpoint", 0);
                point["draw"] = "markers".into();
                series.push(Some(point));
            }
        }
        let lines = series
            .iter()
            .flatten()
            .filter(|s| s.get("draw").and_then(Value::as_str) != Some("markers"))
            .count();
        if lines < 2 {
            return None;
        }
        plot(
            "alt_sources",
            "Altitude estimate",
            "m AMSL",
            series,
            "Estimator",
            note("The three sources should track each other. GPS stepping away is a fix problem; the barometer drifting away is weather or prop wash reaching the sensor. The fused estimate sitting away from its setpoint is neither. that is the controller."),
        )
    }

    fn plot_gps_accuracy(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_gps_position", "sensor_gps"])?;
        let mut series = Vec::new();
        for (field, label) in [
            ("eph", "Horizontal (eph)"),
            ("epv", "Vertical (epv)"),
            ("s_variance_m_s", "Speed variance"),
        ] {
            if let Some(found) = self.first_field(topic, &[field], 0) {
                series.push(self.s(topic, found, label));
            }
        }
        plot(
            "gps_accuracy",
            "GPS uncertainty",
            "m",
            series,
            "Sensors",
            note("This is what the receiver claims, not what it achieved. A value that climbs while the satellite count holds is usually multipath. Read it with the noise plot below."),
        )
    }

    fn plot_gps_noise(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_gps_position", "sensor_gps"])?;
        let mut series = Vec::new();
        for (field, label) in [
            ("noise_per_ms", "Noise per ms"),
            ("jamming_indicator", "Jamming indicator"),
        ] {
            if let Some(found) = self.first_field(topic, &[field], 0) {
                series.push(self.s(topic, found, label));
            }
        }
        plot(
            "gps_noise",
            "GPS noise and jamming",
            "indicator",
            series,
            "Sensors",
            note("A jamming indicator that rises with throttle is almost always the aircraft's own power wiring or video transmitter, not somebody else's jammer."),
        )
    }

    fn plot_baro(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_air_data", "sensor_baro"])?;
        let mut series = Vec::new();
        if let Some(altitude) = self.first_field(topic, &["baro_alt_meter", "altitude"], 0) {
            series.push(self.s(topic, altitude, "Baro altitude"));
        }
        if let Some(temperature) = self.first_field(topic, &["baro_temp_celcius", "temperature"], 0) {
            series.push(self.s(topic, temperature, "Sensor temperature"));
        }
        plot(
            "baro",
            "Barometer",
            "m / °C",
            series,
            "Sensors",
            note("Barometric altitude drifts with temperature and with air moving over the sensor. A reading that walks while the aircraft sits still is the sensor, not the aircraft."),
        )
    }

    fn plot_gps_velocity(&self) -> Option<Value> {
        let gps_topic = self.first_topic(&["vehicle_gps_position", "sensor_gps"])?;
        if !self.log.has("vehicle_local_position") {
            return None;
        }
        let mut series = Vec::new();
        if let (Some(n), Some(e)) = (self.col(gps_topic, "vel_n_m_s", 0), self.col(gps_topic, "vel_e_m_s", 0)) {
            let span = n.len().min(e.len());
            let speed: Vec<f64> = (0..span)
                .map(|i| n.f64_at(i).unwrap_or(f64::NAN).hypot(e.f64_at(i).unwrap_or(f64::NAN)))
                .collect();
            series.push(self.series_from_column(gps_topic, &speed, "GPS", 0));
        }
        if let (Some(vx), Some(vy)) = (
            self.col("vehicle_local_position", "vx", 0),
            self.col("vehicle_local_position", "vy", 0),
        ) {
            let span = vx.len().min(vy.len());
            let speed: Vec<f64> = (0..span)
                .map(|i| vx.f64_at(i).unwrap_or(f64::NAN).hypot(vy.f64_at(i).unwrap_or(f64::NAN)))
                .collect();
            series.push(self.series_from_column("vehicle_local_position", &speed, "Estimator", 0));
        }
        if series.len() < 2 {
            return None;
        }
        plot(
            "gps_velocity",
            "Horizontal velocity: GPS vs estimate",
            "m/s",
            series.into_iter().map(Some).collect(),
            "Estimator",
            note("A persistent gap between the two is the estimator distrusting GPS. Cross-read it against the innovation test ratios."),
        )
    }

    fn plot_rate_error(&self) -> Option<Value> {
        let topic = self.first_topic(&["vehicle_angular_velocity", "sensor_combined"])?;
        let setpoint = self.first_topic(&["vehicle_rates_setpoint"])?;
        let field = if topic == "vehicle_angular_velocity" {
            "xyz"
        } else {
            "gyro_rad"
        };
        let mut series = Vec::new();
        for (index, axis) in ["roll", "pitch", "yaw"].iter().enumerate() {
            let (times, measured) = self.raw_component(topic, field, index, 0, 180.0 / std::f64::consts::PI);
            let found = self.first_field(setpoint, &[axis], 0);
            let Some(found) = found.filter(|_| !times.is_empty()) else {
                continue;
            };
            let (sp_t, sp_v) = self.raw(setpoint, found, 0, 180.0 / std::f64::consts::PI);
            if sp_v.len() < 2 {
                continue;
            }
            let held = align(&times, &sp_t, &sp_v, None);
            let column: Vec<f64> = (0..measured.len())
                .map(|i| {
                    if !held[i].is_nan() {
                        held[i] - measured[i]
                    } else {
                        f64::NAN
                    }
                })
                .collect();
            let (out_t, out_v) = decimate(&times, &column);
            series.push(Some(series_value(
                &format!("{} error", capitalize(axis)),
                &out_t,
                &out_v,
                5,
            )));
        }
        plot(
            "rate_error",
            "Rate tracking error",
            "deg/s",
            series,
            "Control",
            note("Setpoint minus measured, so zero is perfect. A trace that sits off zero means the aircraft could not deliver what was asked; one that buzzes about zero means the gains are chasing noise."),
        )
    }

    fn plot_esc(&self) -> Option<Value> {
        if !self.log.has("esc_status") {
            return None;
        }
        let width = match self.col("esc_status", "esc_count", 0) {
            Some(c) => c.i128_at(0).unwrap_or(0),
            None => 8,
        };
        let width = width.clamp(0, 8) as usize;
        let mut series = Vec::new();
        for index in 0..width {
            let (times, column) = self.raw0("esc_status", &format!("esc[{index}].esc_rpm"));
            if column.is_empty() || max_of(&column) <= 0.0 {
                continue;
            }
            let (out_t, out_v) = decimate(&times, &column);
            let mut s = series_value(&format!("ESC {}", index + 1), &out_t, &out_v, 1);
            s["y"] = Value::Array(out_v.iter().map(|v| num(round_to(*v, 1))).collect());
            series.push(Some(s));
        }
        plot(
            "esc",
            "ESC RPM",
            "rpm",
            series,
            "Airframe",
            note("Reported by the ESCs, not commanded. One motor consistently faster than its neighbours is carrying more of the airframe; one that drops out and returns is a motor or an ESC on its way out."),
        )
    }
}

// ---------------------------------------------------------------------------
// Flight modes and armed time
// ---------------------------------------------------------------------------

fn nav_state(state: i128) -> Option<&'static str> {
    Some(match state {
        0 => "Manual",
        1 => "Altitude",
        2 => "Position",
        3 => "Mission",
        4 => "Loiter",
        5 => "Return",
        6 => "Position slow",
        10 => "Acro",
        12 => "Descend",
        13 => "Termination",
        14 => "Offboard",
        15 => "Stabilized",
        17 => "Takeoff",
        18 => "Land",
        19 => "Follow target",
        20 => "Precision land",
        21 => "Orbit",
        22 => "VTOL takeoff",
        _ => return None,
    })
}

fn main_state(state: i128) -> Option<&'static str> {
    Some(match state {
        0 => "Manual",
        1 => "Altitude",
        2 => "Position",
        3 => "Mission",
        4 => "Loiter",
        5 => "Return",
        6 => "Acro",
        7 => "Offboard",
        8 => "Stabilized",
        9 => "Rattitude",
        10 => "Takeoff",
        11 => "Land",
        12 => "Follow target",
        13 => "Precision land",
        14 => "Orbit",
        _ => return None,
    })
}

pub const MIN_MODE_S: f64 = 0.4;

impl<'a> Ctx<'a> {
    pub fn flight_modes(&self) -> Vec<Mode> {
        let Some(topic) = self.first_topic(&["vehicle_status", "commander_state"]) else {
            return Vec::new();
        };
        let (field, table): (_, fn(i128) -> Option<&'static str>) = if topic == "vehicle_status" {
            (self.first_field(topic, &["nav_state"], 0), nav_state)
        } else {
            (self.first_field(topic, &["main_state"], 0), main_state)
        };
        let Some(field) = field else {
            return Vec::new();
        };
        let Some(states) = self.col(topic, field, 0) else {
            return Vec::new();
        };
        if !advancing(&self.stamps(topic, 0)) {
            return Vec::new();
        }
        let times = self.time_axis(topic, 0);
        let span = states.len().min(times.len());
        if span == 0 {
            return Vec::new();
        }
        let name = |state: i128| table(state).map_or_else(|| format!("Mode {state}"), str::to_string);
        let mut spans: Vec<Mode> = Vec::new();
        let mut current: Option<i128> = None;
        let mut start = times[0];
        for index in 0..span {
            let state = states.i128_at(index).unwrap_or(0);
            if Some(state) != current {
                if let Some(c) = current {
                    spans.push(Mode {
                        mode: name(c),
                        state: c,
                        start,
                        end: times[index],
                    });
                }
                current = Some(state);
                start = times[index];
            }
        }
        let c = current.unwrap_or(0);
        spans.push(Mode {
            mode: name(c),
            state: c,
            start,
            end: times[span - 1],
        });
        let mut out = Vec::new();
        for mut entry in spans {
            entry.start = round_to(entry.start, 2);
            entry.end = round_to(entry.end, 2);
            if entry.end - entry.start >= MIN_MODE_S {
                out.push(entry);
            }
        }
        out
    }

    pub fn armed_spans(&self) -> Vec<Span> {
        let Some(topic) = self.first_topic(&["vehicle_status"]) else {
            return Vec::new();
        };
        let Some(field) = self.first_field(topic, &["arming_state"], 0) else {
            return Vec::new();
        };
        let Some(states) = self.col(topic, field, 0) else {
            return Vec::new();
        };
        if !advancing(&self.stamps(topic, 0)) {
            return Vec::new();
        }
        let times = self.time_axis(topic, 0);
        let span = states.len().min(times.len());
        let mut out: Vec<Span> = Vec::new();
        let mut start: Option<f64> = None;
        for index in 0..span {
            let armed = states.i128_at(index) == Some(2);
            if armed && start.is_none() {
                start = Some(times[index]);
            } else if !armed {
                if let Some(s) = start.take() {
                    out.push(Span {
                        start: round_to(s, 2),
                        end: round_to(times[index], 2),
                    });
                }
            }
        }
        if let Some(s) = start {
            if span > 0 {
                out.push(Span {
                    start: round_to(s, 2),
                    end: round_to(times[span - 1], 2),
                });
            }
        }
        out.into_iter().filter(|s| s.end - s.start >= MIN_MODE_S).collect()
    }
}

// ---------------------------------------------------------------------------
// Detectors
// ---------------------------------------------------------------------------

const SATURATED: f64 = 0.95;
const HEADROOM: f64 = 0.8;
const RESET_EVENT_S: f64 = 1.0;
const SETPOINT_MAX_AGE_S: f64 = 0.5;

const RESET_COUNTERS: [(&str, &str); 4] = [
    ("xy_reset_counter", "horizontal position"),
    ("z_reset_counter", "height"),
    ("vxy_reset_counter", "horizontal velocity"),
    ("heading_reset_counter", "heading"),
];

const FAILURE_BITS: [(i128, &str); 8] = [
    (1, "roll exceeded its limit"),
    (2, "pitch exceeded its limit"),
    (4, "altitude ran away"),
    (8, "an external failure trigger fired"),
    (16, "an ESC failed to arm"),
    (32, "the battery failed"),
    (64, "a propeller is imbalanced"),
    (128, "a motor stopped responding"),
];

fn longest(spans: &Spans) -> Vec<Span> {
    let mut best: Option<Span> = None;
    for s in spans {
        if best.is_none_or(|b| (s.end - s.start) > (b.end - b.start)) {
            best = Some(*s);
        }
    }
    best.into_iter().collect()
}

impl<'a> Ctx<'a> {
    fn saturated_motor(&self, armed: &Spans, floor_s: f64) -> Option<(String, f64, f64)> {
        if !self.log.has("actuator_motors") {
            return None;
        }
        let values = self.rows("actuator_motors", "control", 0)?;
        let width = 8.min(values[0].len());
        if width < 3 {
            return None;
        }
        let times = self.time_axis("actuator_motors", 0);
        let span = times.len().min(values.len());
        let mut flags = vec![vec![0.0f64; span]; width];
        for index in 0..span {
            let row = &values[index];
            if row.len() < width {
                continue;
            }
            let outputs = &row[..width];
            if min_of(outputs) > HEADROOM {
                continue;
            }
            for (motor, value) in outputs.iter().enumerate() {
                if *value >= SATURATED {
                    flags[motor][index] = 1.0;
                }
            }
        }
        let mut worst: Option<(String, f64, f64)> = None;
        for (motor, flag) in flags.iter().enumerate() {
            let (when, column) = within(&times[..span], flag, armed);
            let (seconds, first, _) = time_above(&when, &column, 0.5, false);
            let Some(first) = first else {
                continue;
            };
            if seconds < floor_s {
                continue;
            }
            if worst.as_ref().is_none_or(|w| seconds > w.1) {
                worst = Some((format!("Motor {}", motor + 1), seconds, first));
            }
        }
        worst
    }

    fn estimator_resets(&self, armed: &Spans) -> (Vec<(&'static str, i128, f64)>, i64) {
        let mut out = Vec::new();
        let mut moments: Vec<f64> = Vec::new();
        for (field, label) in RESET_COUNTERS {
            let (t, v) = self.raw0("vehicle_local_position", field);
            let (times, values) = within(&t, &v, armed);
            if values.len() < 2 {
                continue;
            }
            let mut count: i128 = 0;
            let mut first: Option<f64> = None;
            for index in 1..values.len() {
                let step = ((values[index].trunc() as i128) - (values[index - 1].trunc() as i128)).rem_euclid(256);
                if 0 < step && step < 32 {
                    count += step;
                    moments.push(times[index]);
                    if first.is_none() {
                        first = Some(times[index]);
                    }
                }
            }
            if count != 0 {
                out.push((label, count, first.filter(|f| *f != 0.0).unwrap_or(0.0)));
            }
        }
        let mut events = 0;
        let mut last = f64::NEG_INFINITY;
        moments.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for moment in moments {
            if moment - last >= RESET_EVENT_S {
                events += 1;
            }
            last = moment;
        }
        (out, events)
    }

    fn failure_detector(&self) -> (Vec<&'static str>, Option<f64>) {
        let (times, values) = self.raw0("vehicle_status", "failure_detector_status");
        let mut flagged: Vec<&'static str> = Vec::new();
        let mut first = None;
        for (index, value) in values.iter().enumerate() {
            let bits = value.trunc() as i128;
            if bits == 0 {
                continue;
            }
            if first.is_none() {
                first = Some(times[index]);
            }
            for (mask, label) in FAILURE_BITS {
                if bits & mask != 0 && !flagged.contains(&label) {
                    flagged.push(label);
                }
            }
        }
        (flagged, first)
    }

    fn failsafe(&self, armed: &Spans) -> (f64, Option<f64>) {
        let (t, v) = self.raw0("vehicle_status", "failsafe");
        let (times, values) = within(&t, &v, armed);
        let r = time_above(&times, &values, 0.5, false);
        (r.0, r.1)
    }

    fn armed_at_end(&self) -> Option<bool> {
        let field = self.first_field("vehicle_status", &["arming_state"], 0)?;
        let states = self.col("vehicle_status", field, 0)?;
        states.i128_at(states.len() - 1).map(|v| v == 2)
    }

    fn clipping(&self, armed: &Spans) -> (f64, Option<f64>) {
        let Some(topic) = self.first_topic(&["vehicle_imu_status", "sensor_accel_status"]) else {
            return (0.0, None);
        };
        let mut worst: (f64, Option<f64>) = (0.0, None);
        for multi in self.log.instances(topic).into_iter().take(3) {
            let Some(column) = self.clip_column(topic, multi) else {
                continue;
            };
            let times = self.time_axis(topic, multi);
            let span = times.len().min(column.len());
            let (grown, when) = rise(&times[..span], &column[..span], armed);
            if grown > worst.0 {
                worst = (grown, when);
            }
        }
        worst
    }

    fn vibration_peak(&self, armed: &Spans) -> (f64, Option<f64>) {
        let mut columns: Vec<Tv> = Vec::new();
        if self.log.has("vehicle_imu_status") {
            for multi in self.log.instances("vehicle_imu_status").into_iter().take(3) {
                columns.push(self.raw("vehicle_imu_status", "accel_vibration_metric", multi, 1.0));
            }
        } else if self.log.has("estimator_status") {
            for index in 0..3 {
                columns.push(self.raw_component("estimator_status", "vibe", index, 0, 1.0));
            }
        }
        let mut worst: (f64, Option<f64>) = (0.0, None);
        for (times, values) in columns {
            let (t, v) = within(&times, &values, armed);
            for (moment, value) in t.iter().zip(&v) {
                if *value > worst.0 {
                    worst = (*value, Some(*moment));
                }
            }
        }
        worst
    }

    fn attitude_error(&self, axis: &str, armed: &Spans) -> Option<(f64, f64, Option<f64>)> {
        let (topic, roll, pitch, yaw) = self.euler()?;
        let setpoint_topic = self.first_topic(&["vehicle_attitude_setpoint"])?;
        let column = match axis {
            "roll" => roll,
            "pitch" => pitch,
            _ => yaw,
        };
        let times = self.time_axis(topic, 0);
        let span = times.len().min(column.len());
        let body = format!("{axis}_body");
        let found = self.first_field(setpoint_topic, &[leak(&body), leak(axis)], 0);
        let field = found?;
        if span < 50 {
            return None;
        }
        let (sp_t, sp_v) = self.raw(setpoint_topic, field, 0, 180.0 / std::f64::consts::PI);
        if sp_v.len() < 10 {
            return None;
        }
        let (times, column) = within(&times[..span], &column[..span], armed);
        if times.len() < 50 {
            return None;
        }
        let held = align(&times, &sp_t, &sp_v, Some(SETPOINT_MAX_AGE_S));
        let mut errors: Vec<f64> = Vec::new();
        let (mut peak, mut peak_at) = (0.0, None);
        for (index, &value) in column.iter().enumerate() {
            let target = held[index];
            if (target).is_nan() || (value).is_nan() {
                continue;
            }
            let error = (value - target).abs();
            errors.push(error);
            if error > peak {
                peak = error;
                peak_at = Some(times[index]);
            }
        }
        if errors.len() < 50 {
            return None;
        }
        Some((percentile(&errors, 0.95), peak, peak_at))
    }

    fn rate_oscillation(&self, axis: &str, armed: &Spans) -> Option<(f64, f64)> {
        let index = axis_index(axis);
        let topic = self.first_topic(&["vehicle_angular_velocity", "sensor_combined"])?;
        let field = if topic == "vehicle_angular_velocity" {
            "xyz"
        } else {
            "gyro_rad"
        };
        let (t, c) = self.raw_component(topic, field, index, 0, 180.0 / std::f64::consts::PI);
        let (times, column) = within(&t, &c, &longest(armed));
        oscillation(&times, &column)
    }

    fn thrust_column(&self) -> Tv {
        let Some(topic) = self.first_topic(&["vehicle_thrust_setpoint", "actuator_controls_0"]) else {
            return (Vec::new(), Vec::new());
        };
        if topic == "vehicle_thrust_setpoint" {
            let (times, values) = self.raw_component(topic, "xyz", 2, 0, 1.0);
            return (times, values.into_iter().map(f64::abs).collect());
        }
        self.raw_component(topic, "control", 3, 0, 1.0)
    }

    fn mag_norm(&self) -> Tv {
        let Some(topic) = self.first_topic(&["vehicle_magnetometer", "sensor_mag"]) else {
            return (Vec::new(), Vec::new());
        };
        let times = self.time_axis(topic, 0);
        if let Some(values) = self.rows(topic, "magnetometer_ga", 0).filter(|v| v[0].len() >= 3) {
            let span = times.len().min(values.len());
            let norm = (0..span)
                .map(|i| {
                    py_sum(
                        &values[i][..3.min(values[i].len())]
                            .iter()
                            .map(|c| c * c)
                            .collect::<Vec<_>>(),
                    )
                    .sqrt()
                })
                .collect();
            return (times[..span].to_vec(), norm);
        }
        let (Some(x), Some(y), Some(z)) = (
            self.col(topic, "x", 0),
            self.col(topic, "y", 0),
            self.col(topic, "z", 0),
        ) else {
            return (Vec::new(), Vec::new());
        };
        let span = times.len().min(x.len()).min(y.len()).min(z.len());
        let norm = (0..span)
            .map(|i| {
                let (a, b, c) = (
                    x.f64_at(i).unwrap_or(f64::NAN),
                    y.f64_at(i).unwrap_or(f64::NAN),
                    z.f64_at(i).unwrap_or(f64::NAN),
                );
                (a * a + b * b + c * c).sqrt()
            })
            .collect();
        (times[..span].to_vec(), norm)
    }

    fn tracks_thrust(&self, times: &[f64], values: &[f64], armed: &Spans) -> Option<f64> {
        let (thrust_t, thrust_v) = self.thrust_column();
        if thrust_v.len() < 50 {
            return None;
        }
        let (times, values) = within(times, values, armed);
        if times.len() < 50 {
            return None;
        }
        let held = align(&times, &thrust_t, &thrust_v, None);
        let finite: Vec<f64> = held.iter().copied().filter(|v| !(*v).is_nan()).collect();
        if finite.len() < 50 || max_of(&finite) - min_of(&finite) < 0.2 {
            return None;
        }
        correlate(&values, &held)
    }
}

const RECOVERY_MODES: [&str; 4] = ["Return", "Descend", "Termination", "Precision land"];
const ERROR_LEVELS: [&str; 4] = ["emergency", "alert", "critical", "error"];
const MESSAGE_GRACE_S: f64 = 3.0;

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// Where findings go: the level, headline, detail and the second they happened at.
pub struct Findings<'m> {
    pub out: Vec<Value>,
    modes: &'m [Mode],
}

impl<'m> Findings<'m> {
    pub fn new(modes: &'m [Mode]) -> Findings<'m> {
        Findings { out: Vec::new(), modes }
    }

    pub fn add(&mut self, level: &str, text: &str, detail: &str, when: Option<f64>) {
        self.out.push(finding(level, text, when, self.modes, detail));
    }
}

impl<'a> Ctx<'a> {
    pub fn findings(&self, plots: &[Value], modes: &[Mode], armed: &Spans, sensitivity: &str) -> Vec<Value> {
        let level_name = normalize_sensitivity_str(sensitivity);
        let limits = limits_for(level_name);
        let mut add = Findings::new(modes);
        let by_id = |id: &str| plots.iter().find(|p| p["id"] == id);

        let (flagged, flagged_at) = self.failure_detector();
        if !flagged.is_empty() {
            add.add(
                "critical",
                &format!("PX4's failure detector fired: {}", flagged.join(", ")),
                "That is the autopilot's own verdict on the airframe, reached in flight from more than this log holds, not a threshold crossed in this file. Whatever else is on this page, start here.",
                flagged_at,
            );
        }

        if let Some((name, seconds, when)) = self.saturated_motor(armed, limits.saturation_warn_s) {
            add.add(
                if seconds >= limits.saturation_crit_s { "critical" } else { "warning" },
                &format!("{name} was pinned at full output for {} s while the others still had headroom", f(seconds, 1)),
                "One motor at its limit alone is the mixer wanting to correct and having nothing left to correct with. The aircraft was flying without control authority on that corner. Look for a heavy or bent airframe, a failing motor, or too little thrust margin for the weight. (Every motor at its limit together would just be a hard climb, and is not reported.)",
                Some(when),
            );
        }

        let (clipped, clipped_at) = self.clipping(armed);
        if clipped >= limits.clipping_crit {
            add.add(
                "critical",
                &format!("Accelerometer clipping: {} samples", clipped.trunc() as i64),
                "The IMU saturated while armed, so for those samples it reported a limit rather than a measurement. Treat the attitude and position estimates in this log with suspicion, and soften the flight-controller mounting before trusting the next one.",
                clipped_at,
            );
        } else if clipped >= limits.clipping_warn {
            add.add(
                "warning",
                &format!("Accelerometer clipping: {} samples", clipped.trunc() as i64),
                "The IMU saturated for a moment while armed. A few at touchdown are a firm landing; more than that in the air is vibration reaching the flight controller. Only armed time is counted, because the counter itself runs from boot.",
                clipped_at,
            );
        }

        let (worst, when) = self.vibration_peak(armed);
        if worst >= limits.vibration_warn {
            let mut cause = String::new();
            let (vibe_t, vibe_v) = self.raw0("vehicle_imu_status", "accel_vibration_metric");
            let follows = self.tracks_thrust(&vibe_t, &vibe_v, armed);
            match follows {
                Some(c) if c >= 0.5 => {
                    cause = format!(" It rises and falls with the throttle (correlation {}), which points at the propellers and motors rather than at how the controller is mounted.", f(c, 2));
                }
                Some(c) if c < 0.2 => {
                    cause = " It does not follow the throttle, so the mounting or a loose airframe is a likelier cause than the rotating parts.".into();
                }
                _ => {}
            }
            if worst >= limits.vibration_crit {
                add.add(
                    "critical",
                    &format!("Vibration peaked at {} m/s²", f(worst, 0)),
                    &format!("High enough for the estimator itself to suffer. Check propellers, motor bearings and the flight-controller mounting.{cause}"),
                    when,
                );
            } else {
                add.add(
                    "warning",
                    &format!("Vibration peaked at {} m/s²", f(worst, 0)),
                    &format!("Usable, but high enough to act on. Worth watching rather than grounding the aircraft for.{cause}"),
                    when,
                );
            }
        }

        let (failsafe_s, failsafe_at) = self.failsafe(armed);
        if failsafe_s >= 0.5 {
            add.add(
                "warning",
                &format!("In failsafe for {} s", f(failsafe_s, 0)),
                "What the aircraft did about it is the mode strip above; why it happened is in the messages below.",
                failsafe_at,
            );
        }

        self.innovation_finding(armed, &mut add, &limits);

        let (resets, events) = self.estimator_resets(armed);
        if !resets.is_empty() {
            let total: i128 = resets.iter().map(|r| r.1).sum();
            let first = resets
                .iter()
                .map(|r| r.2)
                .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
            let parts: Vec<String> = resets
                .iter()
                .map(|(label, count, _)| format!("{label} {count}×"))
                .collect();
            let detail = format!(
                "Which: {}. A reset is a step in the estimate and therefore in what the controller was flying towards. The track and position plots jump there because the aircraft's idea of where it was jumped, not because it moved.",
                parts.join(", ")
            );
            if events >= limits.reset_events {
                add.add(
                    "warning",
                    &format!("The estimator reset its state {total} time(s) in flight"),
                    &format!("{detail} GPS being re-accepted after a glitch is the usual cause."),
                    Some(first),
                );
            } else {
                add.add(
                    "note",
                    &format!("The estimator reset its state {total} time(s) in flight"),
                    &format!("{detail} One or two are part of an ordinary flight: PX4 aligns its heading again once the aircraft is clear of the ground."),
                    Some(first),
                );
            }
        }

        self.gps_finding(armed, &mut add, &limits);

        for axis in ["roll", "pitch"] {
            let Some((typical, peak, when)) = self.attitude_error(axis, armed) else {
                continue;
            };
            let cap = capitalize(axis);
            if typical >= limits.attitude_crit_deg {
                add.add(
                    "critical",
                    &format!("{cap} missed its setpoint by {}° or more for most of the flight", f(typical, 0)),
                    &format!("Peak {}°. An aircraft that cannot hold the angle it is given is short of control authority, not short of tuning. Read the motor outputs before touching a gain.", f(peak, 0)),
                    when,
                );
            } else if typical >= limits.attitude_warn_deg {
                add.add(
                    "warning",
                    &format!("{cap} tracked its setpoint to about {}°", f(typical, 0)),
                    &format!(
                        "Peak {}°. Not alarming on its own; worth a look at the attitude plot before the next flight.",
                        f(peak, 0)
                    ),
                    when,
                );
            }
        }

        for axis in ["roll", "pitch", "yaw"] {
            let Some((amplitude, frequency)) = self.rate_oscillation(axis, armed) else {
                continue;
            };
            if amplitude >= limits.ringing_dps && frequency >= 6.0 {
                add.add(
                    "warning",
                    &format!("The {axis} rate rings at about {} Hz ({} deg/s)", f(frequency, 0), f(amplitude, 0)),
                    "Oscillation that fast is the rate loop arguing with itself rather than the aircraft moving. The D gain for that axis is the usual cause; vibration reaching the gyro is the other.",
                    None,
                );
            }
        }

        if let Some(actuators) = by_id("actuators") {
            if actuators["unit"] != "normalised" {
                let mut peaks: Vec<(String, f64, f64)> = Vec::new();
                for s in actuators["series"].as_array().into_iter().flatten() {
                    let finite: Vec<f64> = ys(s).into_iter().flatten().collect();
                    if !finite.is_empty() {
                        peaks.push((
                            s["name"].as_str().unwrap_or("").to_string(),
                            max_of(&finite),
                            py_sum(&finite) / finite.len() as f64,
                        ));
                    }
                }
                if peaks.len() >= 3 {
                    let averages: Vec<f64> = peaks.iter().map(|p| p.2).collect();
                    let spread = max_of(&averages) - min_of(&averages);
                    if spread > limits.motor_spread_us {
                        let mut worst_motor = &peaks[0];
                        for p in &peaks[1..] {
                            if p.2 > worst_motor.2 {
                                worst_motor = p;
                            }
                        }
                        add.add(
                            "warning",
                            &format!("Motor outputs are uneven: {} ran {} µs above the lowest on average", worst_motor.0, f(spread, 0)),
                            "One corner working harder than the others across a whole flight is an airframe imbalance, not a tuning one: a bent arm, a heavy corner, a mistrimmed propeller.",
                            None,
                        );
                    }
                }
            }
        }

        self.magnetic_finding(armed, &mut add, &limits);
        self.rc_finding(armed, &mut add, &limits);
        self.battery_findings(armed, &mut add, &limits);

        let (cpu_t, cpu_v) = {
            let (t, v) = self.raw("cpuload", "load", 0, 100.0);
            within(&t, &v, armed)
        };
        if !cpu_v.is_empty() {
            let (heavy, heavy_at, worst_cpu) = time_above(&cpu_t, &cpu_v, limits.cpu_pct, false);
            if let Some(heavy_at) = heavy_at {
                if heavy >= limits.cpu_hold_s {
                    add.add(
                        "warning",
                        &format!("CPU held above {}% for {} s (peak {}%)", f(limits.cpu_pct, 0), f(heavy, 0), f(worst_cpu, 0)),
                        "At that load PX4 starts missing control-loop deadlines. Turn down a logging or telemetry stream rate before the next flight.",
                        Some(heavy_at),
                    );
                }
            }
        }

        if !self.log.dropout_ms.is_empty() {
            let total_ms: i64 = self.log.dropout_ms.iter().map(|d| i64::from(*d)).sum();
            add.add(
                if total_ms as f64 >= limits.dropout_ms { "warning" } else { "note" },
                &format!("{} logging dropout(s), {total_ms} ms total", self.log.dropout_ms.len()),
                "The logger could not keep up with the card. Those gaps are missing data rather than quiet flight, so a flat stretch there says nothing about the aircraft.",
                None,
            );
        }

        if self.log.truncated {
            if self.armed_at_end() == Some(false) {
                add.add(
                    "note",
                    "The log ends mid-record after the aircraft disarmed",
                    "The file was never closed, which is what pulling the battery does while PX4 is still logging. Everything up to the cut is shown, and the flight itself had already ended.",
                    None,
                );
            } else {
                add.add(
                    "warning",
                    "The log ends mid-record",
                    "The aircraft lost power before the file was closed: a crash, a battery pulled, or a brownout. Everything up to the cut is shown.",
                    None,
                );
            }
        }

        self.message_findings(armed, &mut add);

        if let Some(recovery) = modes.iter().find(|m| RECOVERY_MODES.contains(&m.mode.as_str())) {
            if failsafe_s < 0.5 {
                add.add(
                    "note",
                    &format!("The aircraft flew {}", recovery.mode),
                    "PX4 also selects that for itself on a failsafe. Nothing else in this log says a failsafe fired, so if the pilot did not ask for it, the reason is in the messages below.",
                    Some(recovery.start),
                );
            }
        }

        let mut out = add.out;
        let any_problem = out
            .iter()
            .any(|v| matches!(v["level"].as_str(), Some("critical" | "warning")));
        if !any_problem {
            out.insert(
                0,
                finding(
                    "ok",
                    "Nothing flagged in this log",
                    None,
                    modes,
                    &format!("Checked at the {level_name} sensitivity set in Settings. No clipping, vibration, estimator rejection, motor imbalance, RC loss or low battery reached that level. The plots still show everything, and anything listed under this line is context rather than a problem."),
                ),
            );
        }
        out
    }

    fn innovation_finding(&self, armed: &Spans, add: &mut Findings<'_>, limits: &Limits) {
        // (seconds, peak) rank, label, peak, seconds, first
        let mut worst: Option<((f64, f64), &str, f64, f64, Option<f64>)> = None;
        for (field, label) in INNOVATIONS {
            let (t, v) = self.raw0("estimator_status", field);
            let (times, values) = within(&t, &v, armed);
            if values.is_empty() {
                continue;
            }
            let peak = max_of(&values);
            if peak < 1.0 {
                continue;
            }
            let (seconds, first, _) = time_above(&times, &values, 1.0, false);
            let rank = (seconds, peak);
            let better = match &worst {
                None => true,
                Some(w) => rank.0 > w.0 .0 || (rank.0 == w.0 .0 && rank.1 > w.0 .1),
            };
            if better {
                worst = Some((rank, label, peak, seconds, first));
            }
        }
        let Some((_, label, peak, seconds, first)) = worst else {
            return;
        };
        if seconds >= limits.innovation_hold_s {
            add.add(
                "warning",
                &format!("{label} innovations reached {} and stayed above 1.0 for {} s", f(peak, 2), f(seconds, 1)),
                "Above 1.0 the estimator is rejecting that sensor outright. Held that long, it was flying without it. Read the altitude and velocity comparisons in the Estimator section to see what it used instead.",
                first,
            );
        } else {
            add.add(
                "note",
                &format!("{label} innovations touched {} briefly", f(peak, 2)),
                "Above 1.0 the estimator momentarily rejected that sensor. A brief excursion is normal. It is noted rather than flagged because it is only a problem when it lasts.",
                first,
            );
        }
    }

    fn gps_finding(&self, armed: &Spans, add: &mut Findings<'_>, limits: &Limits) {
        let Some(topic) = self.first_topic(&["vehicle_gps_position", "sensor_gps"]) else {
            return;
        };
        if armed.is_empty() {
            return;
        }
        let (times, values) = self.raw0(topic, "fix_type");
        let Some(fixed_at) = times.iter().zip(&values).find(|(_, v)| **v >= 3.0).map(|(t, _)| *t) else {
            return;
        };
        let (wt, wv) = within(&times, &values, armed);
        let air: Vec<(f64, f64)> = wt.into_iter().zip(wv).filter(|(t, _)| *t >= fixed_at).collect();
        let (lost, lost_at, _) = time_above(
            &air.iter().map(|p| p.0).collect::<Vec<_>>(),
            &air.iter().map(|p| p.1).collect::<Vec<_>>(),
            3.0,
            true,
        );
        if let Some(lost_at) = lost_at {
            if lost >= limits.gps_lost_s {
                add.add(
                    "warning",
                    &format!("GPS below a 3-D fix for {} s while armed", f(lost, 0)),
                    "Position and ground speed across that stretch are dead reckoning, however smooth the line on the plot looks.",
                    Some(lost_at),
                );
            }
        }
    }

    fn rc_finding(&self, armed: &Spans, add: &mut Findings<'_>, limits: &Limits) {
        let Some(topic) = self.first_topic(&["input_rc", "rc_channels"]) else {
            return;
        };
        let Some(field) = self.first_field(topic, &["rc_lost", "signal_lost"], 0) else {
            return;
        };
        let (times, values) = self.raw0(topic, field);
        let Some(linked_at) = times.iter().zip(&values).find(|(_, v)| **v < 0.5).map(|(t, _)| *t) else {
            return;
        };
        let (wt, wv) = within(&times, &values, armed);
        let air: Vec<(f64, f64)> = wt.into_iter().zip(wv).filter(|(t, _)| *t >= linked_at).collect();
        let (seconds, when, _) = time_above(
            &air.iter().map(|p| p.0).collect::<Vec<_>>(),
            &air.iter().map(|p| p.1).collect::<Vec<_>>(),
            0.5,
            false,
        );
        if let Some(when) = when {
            if seconds >= limits.rc_lost_s {
                add.add(
                    "warning",
                    "The RC link reported a signal loss in flight",
                    &format!("Lost for {} s in total, after the link had been up. The flight carried on, but the next one may reach the failsafe instead.", f(seconds, 1)),
                    Some(when),
                );
            }
        }
    }

    fn message_findings(&self, armed: &Spans, add: &mut Findings<'_>) {
        let base = self.base;
        let mut flying: Vec<&str> = Vec::new();
        let mut grounded: Vec<&str> = Vec::new();
        for message in &self.log.messages {
            if !ERROR_LEVELS.contains(&message.level) {
                continue;
            }
            let when = Some((message.t as f64 - base as f64) / 1e6);
            if during(when, armed, MESSAGE_GRACE_S) {
                flying.push(&message.text);
            } else {
                grounded.push(&message.text);
            }
        }
        if !flying.is_empty() {
            add.add(
                "warning",
                &format!("{} error-level message(s) in the flight log", flying.len()),
                &format!("The first was “{}”. The aircraft's own commentary is in full at the bottom of this page, and it is usually the shortest route to why a flight went the way it did.", flying[0]),
                None,
            );
        }
        if !grounded.is_empty() {
            add.add(
                "note",
                &format!("{} error-level message(s) while disarmed", grounded.len()),
                &format!("The first was “{}”. Printed on the ground, these are usually the pre-arm checks refusing to arm, which is the system doing its job. They are listed at the bottom of this page.", grounded[0]),
                None,
            );
        }
    }

    fn magnetic_finding(&self, armed: &Spans, add: &mut Findings<'_>, limits: &Limits) {
        let (times, values) = self.mag_norm();
        let mut finite: Vec<f64> = values.iter().copied().filter(|v| *v > 0.0).collect();
        finite.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if finite.len() < 50 {
            return;
        }
        let middle = percentile(&finite, 0.5);
        if middle <= 0.0 {
            return;
        }
        let swing = (percentile(&finite, 0.95) - percentile(&finite, 0.05)) / middle;
        if !(limits.mag_swing <= swing && swing <= 0.6) {
            return;
        }
        let follows = self.tracks_thrust(&times, &values, armed);
        let Some(follows) = follows.filter(|c| *c >= limits.mag_correlation) else {
            return;
        };
        add.add(
            "warning",
            &format!("Compass field strength moves with the throttle (correlation {})", f(follows, 2)),
            &format!("It swings {}% of its own value across the flight. That is current from the power wiring reaching the magnetometer: it biases heading exactly when the aircraft is working hardest, and it is why a compass calibration does not hold. The fix is routing and distance, not another calibration.", f(swing * 100.0, 0)),
            None,
        );
    }

    fn battery_findings(&self, armed: &Spans, add: &mut Findings<'_>, limits: &Limits) {
        let Some(field) = self.first_field("battery_status", &["voltage_filtered_v", "voltage_v"], 0) else {
            return;
        };
        let (times, volts) = self.raw0("battery_status", field);
        let kept: Vec<(f64, f64)> = times.into_iter().zip(volts).filter(|(_, v)| *v > 1.0).collect();
        if kept.is_empty() {
            return;
        }
        let top = max_of(&kept.iter().map(|p| p.1).collect::<Vec<_>>());
        let mut logged: Option<i128> = None;
        if let Some(col) = self.col("battery_status", "cell_count", 0) {
            for i in 0..col.len() {
                if col.truthy_at(i) == Some(true) {
                    logged = col.i128_at(i);
                    break;
                }
            }
        }
        let cells: f64 = match logged {
            Some(c) => c as f64,
            None => py_max(1.0, (top / 4.25).ceil()),
        };
        let (air_t, air_v) = within(
            &kept.iter().map(|p| p.0).collect::<Vec<_>>(),
            &kept.iter().map(|p| p.1).collect::<Vec<_>>(),
            armed,
        );
        if air_v.is_empty() {
            return;
        }
        let (sag_v, sag_at) = sag(&air_t, &air_v, SAG_WINDOW_S);
        if sag_v / cells > limits.sag_per_cell_v {
            add.add(
                "warning",
                &format!("Pack voltage sagged {} V under load", f(sag_v, 1)),
                &format!("{} V per cell within a few seconds, which is internal resistance rather than the pack emptying. A pack that dives this far under load is near the end of its useful life.", f(sag_v / cells, 2)),
                sag_at,
            );
        }
        let per_cell: Vec<f64> = air_v.iter().map(|v| v / cells).collect();
        let Some((per_cell, floor_at)) = held_floor(&air_t, &per_cell, limits.cell_hold_s) else {
            return;
        };
        let floor = per_cell * cells;
        if per_cell < limits.cell_crit_v {
            add.add(
                "critical",
                &format!("The pack reached {} V ({} V per cell across {})", f(floor, 1), f(per_cell, 2), cells as i64),
                &format!("It stayed at or under that for {} s or more, which is into the range that permanently costs a lithium pack capacity. If it recovered after landing the damage is smaller, but the next flight should be shorter either way.", f(limits.cell_hold_s, 0)),
                Some(floor_at),
            );
        } else if per_cell < limits.cell_warn_v {
            add.add(
                "warning",
                &format!("The pack reached {} V ({} V per cell)", f(floor, 1), f(per_cell, 2)),
                "Held there under load rather than touched in one punch. The number that decides the next flight is where the pack settles after landing.",
                Some(floor_at),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The review
// ---------------------------------------------------------------------------

pub const GROUP_ORDER: [&str; 6] = ["Flight", "Control", "Airframe", "Estimator", "Sensors", "System"];

/// Only these topics are decoded.
pub const REVIEW_TOPICS: [&str; 37] = [
    "actuator_motors",
    "actuator_outputs",
    "actuator_controls_0",
    "esc_status",
    "vehicle_imu_status",
    "sensor_accel_status",
    "sensor_combined",
    "estimator_status",
    "estimator_sensor_bias",
    "vehicle_attitude",
    "control_state",
    "vehicle_attitude_setpoint",
    "vehicle_angular_velocity",
    "vehicle_rates_setpoint",
    "vehicle_thrust_setpoint",
    "manual_control_setpoint",
    "manual_control_input",
    "vehicle_local_position",
    "vehicle_local_position_setpoint",
    "vehicle_global_position",
    "position_setpoint_triplet",
    "vehicle_magnetometer",
    "sensor_mag",
    "battery_status",
    "cpuload",
    "vehicle_gps_position",
    "sensor_gps",
    "vehicle_status",
    "commander_state",
    "input_rc",
    "rc_channels",
    "airspeed",
    "distance_sensor",
    "wind",
    "wind_estimate",
    "vehicle_air_data",
    "sensor_baro",
];

impl<'a> Ctx<'a> {
    fn all_plots(&self) -> Vec<Value> {
        let makers: Vec<Option<Value>> = vec![
            self.plot_track(),
            self.plot_altitude(),
            self.local_pair("pos_x", "Local position X (north)", "x", "m", 1.0, ""),
            self.local_pair("pos_y", "Local position Y (east)", "y", "m", 1.0, ""),
            self.local_pair(
                "pos_z",
                "Local position Z (height)",
                "z",
                "m",
                -1.0,
                "Flipped out of NED so up is up. Zero is where the estimator started, not the ground.",
            ),
            self.plot_velocity(),
            self.local_pair("vel_x", "Velocity X (north)", "vx", "m/s", 1.0, ""),
            self.local_pair("vel_y", "Velocity Y (east)", "vy", "m/s", 1.0, ""),
            self.local_pair(
                "vel_z",
                "Velocity Z (climb)",
                "vz",
                "m/s",
                -1.0,
                "Flipped out of NED, so a climb reads as a climb.",
            ),
            self.plot_airspeed(),
            self.plot_wind(),
            self.plot_manual_control(),
            self.attitude_plot("roll"),
            self.rate_plot("roll"),
            self.attitude_plot("pitch"),
            self.rate_plot("pitch"),
            self.attitude_plot("yaw"),
            self.rate_plot("yaw"),
            self.plot_rate_error(),
            self.plot_thrust(),
            self.plot_actuators(),
            self.plot_esc(),
            self.plot_ekf(),
            self.plot_altitude_sources(),
            self.plot_gps_velocity(),
            self.plot_bias(),
            self.plot_clipping(),
            self.plot_vibration(),
            self.plot_mag(),
            self.plot_imu_temp(),
            self.plot_baro(),
            self.plot_gps(),
            self.plot_gps_accuracy(),
            self.plot_gps_noise(),
            self.plot_gps_quality(),
            self.plot_distance(),
            self.plot_battery(),
            self.plot_power(),
            self.plot_cpu(),
            self.plot_rc(),
        ];
        makers.into_iter().flatten().collect()
    }

    fn timed_messages(&self) -> Vec<Value> {
        let base = self.base;
        let total = self.log.messages.len();
        self.log.messages[total.saturating_sub(300)..]
            .iter()
            .map(|m| {
                let when = py_max(0.0, (m.t as f64 - base as f64) / 1e6);
                json!({"t": num(round_to(when, 2)), "level": m.level, "text": m.text})
            })
            .collect()
    }

    /// Reduces a parsed ULog to the Flight Review payload the page renders.
    pub fn review(&self, name: &str, sensitivity: &str) -> Value {
        let sensitivity = normalize_sensitivity_str(sensitivity);
        let log = self.log;
        let modes = self.flight_modes();
        let armed = self.armed_spans();
        let plots = self.all_plots();

        let mut duration_v = match modes.split_first() {
            None => 0.0,
            Some((first, rest)) => rest
                .iter()
                .fold(first.end, |best, m| if m.end > best { m.end } else { best }),
        };
        for plot in &plots {
            for s in plot["series"].as_array().into_iter().flatten() {
                let x = xs(s);
                if let Some(last) = x.last() {
                    duration_v = py_max(duration_v, *last);
                }
            }
        }

        let info = |key: &str| {
            log.info
                .get(key)
                .filter(|v| v.truthy())
                .map(|v| v.py_str())
                .unwrap_or_default()
        };
        let armed_total = py_sum(&armed.iter().map(|s| s.end - s.start).collect::<Vec<_>>());
        let mut flown: Vec<String> = modes.iter().map(|m| m.mode.clone()).collect();
        flown.sort();
        flown.dedup();
        let summary = json!({
            "name": name,
            "duration_s": num(round_to(duration_v, 1)),
            "start_utc": log.start_timestamp,
            "sw": info("ver_sw"),
            "hw": info("ver_hw"),
            "sys_name": info("sys_name"),
            "airframe": log.params.get("SYS_AUTOSTART").map_or(Value::from(""), |v| v.to_json()),
            "dropouts": log.dropout_ms.len(),
            "dropout_ms": log.dropout_ms.iter().map(|d| i64::from(*d)).sum::<i64>(),
            "truncated": log.truncated,
            "topics": log.data.keys().filter(|t| log.has(t)).cloned().collect::<Vec<_>>(),
            "armed_s": num(round_to(armed_total, 1)),
            "modes_flown": flown,
            "sensitivity": sensitivity,
        });
        let groups: Vec<&str> = GROUP_ORDER
            .iter()
            .copied()
            .filter(|g| plots.iter().any(|p| p["group"] == *g))
            .collect();
        let findings = self.findings(&plots, &modes, &armed, sensitivity);
        json!({
            "summary": summary,
            "groups": groups,
            "modes": modes.iter().map(|m| json!({
                "mode": m.mode, "state": i64::try_from(m.state).unwrap_or(0),
                "start": num(m.start), "end": num(m.end)})).collect::<Vec<_>>(),
            "armed": armed.iter().map(|s| json!({"start": num(s.start), "end": num(s.end)})).collect::<Vec<_>>(),
            "findings": findings,
            "plots": plots,
            "messages": self.timed_messages(),
        })
    }
}

/// Reviews already produced, newest last: two cover opening a log, going
/// back and opening it (or its neighbour) again.
const CACHE_ENTRIES: usize = 2;

type CacheKey = (String, u64, i128, String);

static CACHE: Mutex<Vec<(CacheKey, std::sync::Arc<Value>)>> = Mutex::new(Vec::new());

fn cache_get(key: &CacheKey) -> Option<std::sync::Arc<Value>> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let at = cache.iter().position(|(k, _)| k == key)?;
    let entry = cache.remove(at);
    let hit = entry.1.clone();
    cache.push(entry);
    Some(hit)
}

fn cache_put(key: CacheKey, data: std::sync::Arc<Value>) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _)| *k != key);
    cache.push((key, data));
    while cache.len() > CACHE_ENTRIES {
        cache.remove(0);
    }
}

/// Drops every cached review (tests, and a shutdown that wants the memory back).
pub fn clear_cache() {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Why a review could not be made, with the text the route answers.
#[derive(Debug)]
pub enum ReviewError {
    Ulog(String),
    Io(String),
}

impl From<UlogError> for ReviewError {
    fn from(e: UlogError) -> ReviewError {
        ReviewError::Ulog(e.0)
    }
}

pub fn review_log(log: &ULog, name: &str, sensitivity: &str) -> Value {
    Ctx::new(log).review(name, sensitivity)
}

/// Reviews a ULog handed over as its own bytes (an upload), cached by digest.
pub fn review_bytes(blob: &[u8], name: &str, sensitivity: &str) -> Result<std::sync::Arc<Value>, ReviewError> {
    let sensitivity = normalize_sensitivity_str(sensitivity);
    let digest = content_digest(blob);
    let key: CacheKey = (format!("sha1:{digest}"), blob.len() as u64, 0, sensitivity.to_string());
    if let Some(hit) = cache_get(&key) {
        return Ok(hit);
    }
    let log = read(blob, Some(&REVIEW_TOPICS))?;
    let data = std::sync::Arc::new(review_log(&log, name, sensitivity));
    cache_put(key, data.clone());
    Ok(data)
}

/// Reviews the ULog at `path`, reusing a recent result for the same file.
pub fn review_file(
    path: &std::path::Path,
    name: &str,
    sensitivity: &str,
) -> Result<std::sync::Arc<Value>, ReviewError> {
    use std::io::Read;
    let label = if name.is_empty() {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        name.to_string()
    };
    let sensitivity = normalize_sensitivity_str(sensitivity);
    let key: Option<CacheKey> = std::fs::metadata(path).ok().map(|m| {
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i128);
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        (
            real.to_string_lossy().into_owned(),
            m.len(),
            mtime,
            sensitivity.to_string(),
        )
    });
    if let Some(key) = &key {
        if let Some(hit) = cache_get(key) {
            return Ok(hit);
        }
    }
    let mut blob = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(super::ulog::MAX_FILE_BYTES as u64 + 1).read_to_end(&mut blob))
        .map_err(|e| ReviewError::Io(super::pytext::os_error_text(&e, Some(path))))?;
    let log = read(&blob, Some(&REVIEW_TOPICS))?;
    let data = std::sync::Arc::new(review_log(&log, &label, sensitivity));
    if let Some(key) = key {
        cache_put(key, data.clone());
    }
    Ok(data)
}

fn content_digest(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
