//! Vibration Monitor: live PX4 vibration metrics and accelerometer clipping
//! (`python/plugins/vibration/vibration.js` and `vibration.css`).
//!
//! PX4 repurposes the three VIBRATION fields: `vibration_x` is gyro
//! delta-angle coning, `vibration_y` gyro high frequency and `vibration_z`
//! accelerometer high frequency, which is the mechanical health indicator.
//! While the view is open the plugin asks the vehicle for 10 Hz VIBRATION
//! (`POST /api/vibration/stream`, PX4 sends 0.1 Hz by default) and gives the
//! default back when it closes.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::{Host, Info, Plugin, Reply};
use crate::ui::chart::{self, Series};
use crate::ui::style::{self, Weight};

/// About 60 s at 10 Hz.
pub const MAX_POINTS: usize = 600;
/// Illustrative accelerometer HF level for the one-shot alert.
pub const HIGH_VIB_Z: f64 = 30.0;
const RATE_HZ: u32 = 10;
const NOTE: &str = "PX4 repurposes these fields: vibration_x = gyro delta-angle coning, vibration_y = gyro high-frequency, vibration_z = accel high-frequency vibration. Higher accel-HF (red) = more mechanical vibration.";
const STREAM_FAILED: &str = "Could not enable high-rate vibration stream, showing PX4 default-rate data";

/// The three series of the chart, oldest first.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Buffer {
    pub t: Vec<f64>,
    pub vx: Vec<f64>,
    pub vy: Vec<f64>,
    pub vz: Vec<f64>,
}

impl Buffer {
    /// Appends a point only when a vibration value changed since the last
    /// one, so unrelated telemetry frames do not flood the buffer with
    /// duplicates (`updateBuffer`). Returns whether a point was added.
    pub fn update(&mut self, state: &Value, t_sec: f64) -> bool {
        let n = |k: &str| {
            state
                .get(k)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
                .unwrap_or(0.0)
        };
        let (vx, vy, vz) = (n("vibration_x"), n("vibration_y"), n("vibration_z"));
        if let (Some(x), Some(y), Some(z)) = (self.vx.last(), self.vy.last(), self.vz.last()) {
            if *x == vx && *y == vy && *z == vz {
                return false;
            }
        }
        self.t.push(t_sec);
        self.vx.push(vx);
        self.vy.push(vy);
        self.vz.push(vz);
        if self.t.len() > MAX_POINTS {
            self.t.remove(0);
            self.vx.remove(0);
            self.vy.remove(0);
            self.vz.remove(0);
        }
        true
    }
}

/// The stats line: x, y, z to four decimals and the clipping counts
/// (`updateStats`).
pub fn stats(state: &Value) -> [(String, String); 4] {
    let n = |k: &str| {
        state
            .get(k)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
    };
    let c = |k: &str| format!("{}", n(k) as i64);
    [
        ("x".into(), format!("{:.4}", n("vibration_x"))),
        ("y".into(), format!("{:.4}", n("vibration_y"))),
        ("z".into(), format!("{:.4}", n("vibration_z"))),
        (
            "Clipping".into(),
            format!("{}/{}/{}", c("clipping_0"), c("clipping_1"), c("clipping_2")),
        ),
    ]
}

/// Whether a reading starts or ends a high vibration episode. The warning is
/// said once per crossing and the latch resets when the level recovers.
pub fn latch(notified: bool, vz: f64) -> (bool, bool) {
    if vz >= HIGH_VIB_Z && !notified {
        (true, true)
    } else if vz < HIGH_VIB_Z && notified {
        (false, false)
    } else {
        (notified, false)
    }
}

#[derive(Default)]
pub struct Vibration {
    live: bool,
    start: f64,
    buf: Buffer,
    notified: bool,
    warn: Option<String>,
    enable: Option<Reply>,
    last: Value,
}

impl Plugin for Vibration {
    fn info(&self) -> Info {
        Info {
            id: "vibration",
            name: "Vibration Monitor",
            icon: "activity",
            description: "Live PX4 vibration metrics and accelerometer clipping",
            version: "1.0.0",
            order: 10,
            can_tab: false,
        }
    }

    fn open(&mut self, h: &mut Host) {
        self.live = true;
        self.start = h.now;
        self.buf = Buffer::default();
        self.notified = false;
        self.warn = None;
        self.last = h.state.clone();
        self.enable = Some(h.post("/api/vibration/stream", json!({"enabled": true, "rate_hz": RATE_HZ})));
    }

    fn tick(&mut self, h: &mut Host) {
        if let Some(r) = self.enable.as_ref().and_then(Reply::take) {
            self.enable = None;
            if r.is_err() {
                self.warn = Some(STREAM_FAILED.into());
            }
        }
        if !self.live {
            return;
        }
        self.last = h.state.clone();
        self.buf.update(h.state, h.now - self.start);
        let vz = h.state.get("vibration_z").and_then(Value::as_f64).unwrap_or(0.0);
        let (next, say) = latch(self.notified, vz);
        self.notified = next;
        if say {
            h.notify(
                "warning",
                &format!("High accelerometer vibration (vibration_z = {vz:.2})"),
            );
        }
    }

    fn close(&mut self, h: &mut Host) {
        self.live = false;
        self.enable = None;
        let _ = h.post("/api/vibration/stream", json!({"enabled": false, "rate_hz": RATE_HZ}));
        self.buf = Buffer::default();
    }

    fn show(&mut self, ui: &mut egui::Ui, area: Rect, _h: &mut Host) -> f32 {
        let (x, w) = (area.left(), area.width());
        let mut y = area.top();
        let p = ui.painter().clone();
        if let Some(text) = &self.warn {
            y += warning(ui, x, y, w, text) + 10.0;
        }
        // .vib-header
        let t = style::galley(ui, "Vibration Monitor", 13.0, Weight::W600, style::text_1(), 0.3);
        p.galley(Pos2::new(x, y), t.clone(), style::text_1());
        y += t.size().y + 8.0;
        // .vib-legend
        let colors = trace_colors();
        let items = [
            (colors[0], "Gyro coning (x)"),
            (colors[1], "Gyro HF vibration (y)"),
            (colors[2], "Accel HF vibration (z)"),
        ];
        let (mut lx, mut ly, mut row_h) = (x, y, 0.0f32);
        for (c, label) in items {
            let g = style::galley(ui, label, 11.0, Weight::W400, style::text_2(), 0.0);
            let iw = 9.0 + 6.0 + g.size().x;
            if lx > x && lx + iw > x + w {
                lx = x;
                ly += row_h + 6.0;
                row_h = 0.0;
            }
            let h = g.size().y.max(9.0);
            p.circle_filled(Pos2::new(lx + 4.5, ly + h / 2.0), 4.5, c);
            p.galley(Pos2::new(lx + 15.0, ly + (h - g.size().y) / 2.0), g, style::text_2());
            lx += iw + 12.0;
            row_h = row_h.max(h);
        }
        y = ly + row_h + 6.0;
        // .vib-note
        let g = crate::ui::kit::wrapped_weight(ui, NOTE, 10.5, Weight::W400, style::text_3(), w - 20.0 - 2.0, 1.5);
        let note = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 16.0 + 2.0));
        p.rect_filled(note, style::RADIUS_BTN, style::surface_2());
        p.rect_stroke(
            note,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        p.galley(Pos2::new(note.left() + 11.0, note.top() + 9.0), g, style::text_3());
        y = note.bottom() + 10.0;
        // .vib-chart
        let plot = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 320.0));
        let wrap = |v: &[f64]| v.iter().map(|n| Some(*n)).collect::<Vec<_>>();
        let (vx, vy, vz) = (wrap(&self.buf.vx), wrap(&self.buf.vy), wrap(&self.buf.vz));
        let series = [
            Series {
                y: &vx,
                color: colors[0],
                width: 1.5,
                dotted: false,
            },
            Series {
                y: &vy,
                color: colors[1],
                width: 1.5,
                dotted: false,
            },
            Series {
                y: &vz,
                color: colors[2],
                width: 1.5,
                dotted: false,
            },
        ];
        chart::draw(
            ui,
            plot,
            &self.buf.t,
            &series,
            "Time (s)",
            "Vibration metric (unitless)",
        );
        legend(ui, plot, &colors);
        y = plot.bottom() + 10.0;
        // .vib-stats
        let (mut sx, mut sy, mut sh) = (x, y, 0.0f32);
        for (k, v) in stats(&self.last) {
            let kg = style::galley(ui, &k.to_uppercase(), 9.5, Weight::W600, style::text_3(), 0.7);
            let vg = style::galley_mono(ui, &v, 11.0, style::text_1());
            let iw = kg.size().x + 5.0 + vg.size().x;
            if sx > x && sx + iw > x + w {
                sx = x;
                sy += sh + 0.0;
                sh = 0.0;
            }
            let base = kg.size().y.max(vg.size().y);
            p.galley(Pos2::new(sx, sy + base - kg.size().y), kg.clone(), style::text_3());
            p.galley(
                Pos2::new(sx + kg.size().x + 5.0, sy + base - vg.size().y),
                vg,
                style::text_1(),
            );
            sx += iw + 14.0;
            sh = sh.max(base);
        }
        (sy + sh) - area.top()
    }
}

/// `traceColors`: nav blue, warning yellow, critical red.
fn trace_colors() -> [Color32; 3] {
    [style::nav(), style::warning(), style::critical()]
}

/// `.vib-warning`.
fn warning(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = crate::ui::kit::wrapped_weight(ui, text, 11.0, Weight::W400, style::warning(), w - 22.0, 1.45);
    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 18.0));
    ui.painter().rect_filled(r, style::RADIUS_BTN, style::warning_dark());
    ui.painter().rect_stroke(
        r,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::warning()),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(Pos2::new(r.left() + 11.0, r.top() + 9.0), g, style::warning());
    r.height()
}

/// Plotly's legend at the plot's top right: a line sample and the name.
fn legend(ui: &egui::Ui, plot: Rect, colors: &[Color32; 3]) {
    let names = ["Gyro coning", "Gyro HF", "Accel HF"];
    let p = ui.painter().with_clip_rect(plot);
    let mut y = plot.top() + chart::MARGIN.2 + 4.0;
    let right = plot.right() - chart::MARGIN.1 - 4.0;
    for (c, n) in colors.iter().zip(names) {
        let g = style::galley(ui, n, 12.0, Weight::W400, style::text_2(), 0.0);
        let x = right - g.size().x;
        p.galley(Pos2::new(x, y), g.clone(), style::text_2());
        let cy = y + g.size().y / 2.0;
        p.line_segment(
            [Pos2::new(x - 8.0 - 30.0, cy), Pos2::new(x - 8.0, cy)],
            Stroke::new(1.5, *c),
        );
        y += g.size().y + 4.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: f64, y: f64, z: f64) -> Value {
        json!({"vibration_x": x, "vibration_y": y, "vibration_z": z})
    }

    #[test]
    fn a_point_is_added_only_when_a_value_changed() {
        let mut b = Buffer::default();
        assert!(b.update(&s(0.1, 0.2, 0.3), 0.0));
        assert!(
            !b.update(&s(0.1, 0.2, 0.3), 1.0),
            "identical vibration does not grow the buffer"
        );
        assert_eq!(b.vx.len(), 1);
        assert!(b.update(&s(0.1, 0.2, 0.9), 2.0));
        assert_eq!(b.vx.len(), 2);
        assert_eq!(b.vz[1], 0.9);
        assert_eq!(b.t[1], 2.0);
    }

    #[test]
    fn the_buffer_keeps_the_last_600_points() {
        let mut b = Buffer::default();
        for i in 0..MAX_POINTS + 50 {
            b.update(&s(i as f64, 0.0, 0.0), i as f64);
        }
        assert_eq!(b.vx.len(), MAX_POINTS);
        assert_eq!(b.vx[0], 50.0, "the oldest points go first");
    }

    #[test]
    fn missing_or_bad_values_read_as_zero() {
        let mut b = Buffer::default();
        assert!(b.update(&json!({"vibration_x": "x"}), 0.0));
        assert_eq!((b.vx[0], b.vy[0], b.vz[0]), (0.0, 0.0, 0.0));
        assert!(!b.update(&json!({}), 1.0));
    }

    #[test]
    fn the_stats_line_reads_four_decimals_and_the_clipping_counts() {
        let st = json!({"vibration_x": 0.12345, "vibration_y": 1.0, "vibration_z": 31.5, "clipping_0": 2, "clipping_1": 0, "clipping_2": 7});
        let v = stats(&st);
        assert_eq!(v[0], ("x".to_string(), "0.1235".to_string()));
        assert_eq!(v[1].1, "1.0000");
        assert_eq!(v[2].1, "31.5000");
        assert_eq!(v[3], ("Clipping".to_string(), "2/0/7".to_string()));
        assert_eq!(stats(&json!({}))[3].1, "0/0/0");
    }

    #[test]
    fn a_high_episode_warns_once_and_rearms_when_it_recovers() {
        assert_eq!(latch(false, 12.0), (false, false));
        assert_eq!(latch(false, 30.0), (true, true));
        assert_eq!(latch(true, 55.0), (true, false));
        assert_eq!(latch(true, 29.9), (false, false));
        assert_eq!(latch(false, 40.0), (true, true));
    }
}
