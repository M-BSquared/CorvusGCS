//! A small plotting module for the flight review: lines and markers on
//! linear axes, flight mode bands, a threshold line, a legend, a hover read
//! out and drag to zoom. It stands in for the Plotly charts of
//! `python/src/js/analysis.js` (`drawPlot`), with the margins, colours and type of
//! the web's `plotlyTheme`, and draws with egui's own painter.

use eframe::egui::{self, epaint::TextShape, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::ui::style::{self, Weight};

/// `margin: { l: 52, r: 12, t: 6, b: 62 }`.
pub const MARGIN: (f32, f32, f32, f32) = (52.0, 12.0, 6.0, 62.0);

/// One trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub markers: bool,
    pub step: bool,
}

/// One plot of a review.
#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    pub title: String,
    pub group: String,
    pub unit: String,
    pub xlabel: String,
    pub ylabel: String,
    pub equal: bool,
    pub threshold: Option<f64>,
    pub note: String,
    pub origin: Option<(f64, f64)>,
    pub series: Vec<Series>,
}

/// A flight mode's span, drawn behind a time plot.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub color: u32,
}

fn floats(v: Option<&Value>) -> Vec<f64> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|n| n.as_f64().unwrap_or(f64::NAN)).collect())
        .unwrap_or_default()
}

impl Plot {
    /// A plot as the backend describes it.
    pub fn from_json(p: &Value) -> Plot {
        let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let origin = p
            .get("origin")
            .and_then(|o| Some((o.get("lat")?.as_f64()?, o.get("lon")?.as_f64()?)));
        Plot {
            title: text("title"),
            group: text("group"),
            unit: text("unit"),
            xlabel: text("xlabel"),
            ylabel: text("ylabel"),
            equal: p
                .get("equal")
                .is_some_and(|e| e.as_bool() == Some(true) || e.as_i64() == Some(1)),
            threshold: p.get("threshold").and_then(Value::as_f64),
            note: text("note"),
            origin,
            series: p
                .get("series")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|s| Series {
                            name: s.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            x: floats(s.get("x")),
                            y: floats(s.get("y")),
                            markers: s.get("draw").and_then(Value::as_str) == Some("markers"),
                            step: s
                                .get("shape")
                                .and_then(Value::as_str)
                                .is_some_and(|s| s.starts_with("hv")),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The extent of all finite points: `(x0, x1, y0, y1)`.
    pub fn extent(&self) -> Option<(f64, f64, f64, f64)> {
        let mut e: Option<(f64, f64, f64, f64)> = None;
        for s in &self.series {
            for (x, y) in s.x.iter().zip(&s.y) {
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                e = Some(match e {
                    None => (*x, *x, *y, *y),
                    Some((a, b, c, d)) => (a.min(*x), b.max(*x), c.min(*y), d.max(*y)),
                });
            }
        }
        e
    }

    /// Whether the x axis is time (a track and the like name their own).
    pub fn is_time(&self) -> bool {
        self.xlabel.is_empty()
    }
}

/// Ticks of a "nice" step (1, 2 or 5 times a power of ten) across `lo..hi`,
/// about `target` of them, and the step.
pub fn nice_ticks(lo: f64, hi: f64, target: usize) -> (Vec<f64>, f64) {
    let span = hi - lo;
    if span <= 0.0 || !span.is_finite() {
        return (vec![lo], 1.0);
    }
    let raw = span / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = mag
        * if norm <= 1.0 {
            1.0
        } else if norm <= 2.0 {
            2.0
        } else if norm <= 5.0 {
            5.0
        } else {
            10.0
        };
    let first = (lo / step).ceil();
    let last = (hi / step).floor();
    let ticks = (first as i64..=last as i64).map(|i| i as f64 * step).collect();
    (ticks, step)
}

/// A tick's label: as many decimals as the step needs.
pub fn tick_label(v: f64, step: f64) -> String {
    let decimals = if step >= 1.0 {
        0
    } else {
        (-step.log10().floor()) as usize
    };
    let v = if v.abs() < step * 1e-9 { 0.0 } else { v };
    format!("{v:.decimals$}")
}

/// The range an axis shows: the data's extent, padded by 5 % where Plotly
/// pads (y), and never empty.
pub fn axis_range(lo: f64, hi: f64, pad: bool) -> (f64, f64) {
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        let d = if lo == 0.0 { 1.0 } else { lo.abs() * 0.05 };
        return (lo - d, hi + d);
    }
    if pad {
        let p = (hi - lo) * 0.05;
        (lo - p, hi + p)
    } else {
        (lo, hi)
    }
}

/// Fewer points for a narrow plot: for each pixel column the first, lowest,
/// highest and last point, in order, so a spike survives.
pub fn reduce(x: &[f64], y: &[f64], columns: usize) -> Vec<(f64, f64)> {
    let n = x.len().min(y.len());
    if n <= columns * 4 || columns == 0 {
        return (0..n).map(|i| (x[i], y[i])).collect();
    }
    let (x0, x1) = x[..n]
        .iter()
        .filter(|v| v.is_finite())
        .fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
    if x1.partial_cmp(&x0) != Some(std::cmp::Ordering::Greater) {
        return (0..n).map(|i| (x[i], y[i])).collect();
    }
    let col_of = |v: f64| {
        (((v - x0) / (x1 - x0)) * columns as f64)
            .floor()
            .clamp(0.0, columns as f64 - 1.0) as usize
    };
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(columns * 4);
    let mut i = 0;
    while i < n {
        if !x[i].is_finite() || !y[i].is_finite() {
            out.push((x[i], f64::NAN));
            i += 1;
            continue;
        }
        let c = col_of(x[i]);
        let (mut lo, mut hi) = (i, i);
        let mut j = i;
        while j < n && x[j].is_finite() && y[j].is_finite() && col_of(x[j]) == c {
            if y[j] < y[lo] {
                lo = j;
            }
            if y[j] > y[hi] {
                hi = j;
            }
            j += 1;
        }
        let last = j - 1;
        let mut idx = vec![i, lo, hi, last];
        idx.sort_unstable();
        idx.dedup();
        out.extend(idx.into_iter().map(|k| (x[k], y[k])));
        i = j;
    }
    out
}

/// A held value between points: horizontal first, then vertical (`hv`).
pub fn stepped(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(points.len() * 2);
    for (i, p) in points.iter().enumerate() {
        if i > 0 && p.1.is_finite() && points[i - 1].1.is_finite() {
            out.push((p.0, points[i - 1].1));
        }
        out.push(*p);
    }
    out
}

/// Equal scale on both axes: the shorter span in pixels is widened around its
/// middle so a metre is as long east as it is north.
pub fn equalize(xr: (f64, f64), yr: (f64, f64), w: f32, h: f32) -> ((f64, f64), (f64, f64)) {
    let (sx, sy) = ((xr.1 - xr.0) / f64::from(w), (yr.1 - yr.0) / f64::from(h));
    let s = sx.max(sy);
    let widen = |r: (f64, f64), px: f32| {
        let mid = (r.0 + r.1) / 2.0;
        let half = s * f64::from(px) / 2.0;
        (mid - half, mid + half)
    };
    (widen(xr, w), widen(yr, h))
}

/// What a plot remembers between frames: the reduced traces for one width,
/// the zoom, and the drag in progress.
#[derive(Default)]
pub struct PlotState {
    width: u32,
    traces: Vec<Vec<(f64, f64)>>,
    pub view: Option<[f64; 4]>,
    drag_from: Option<Pos2>,
}

fn series_color(i: usize) -> Color32 {
    match i % 5 {
        0 => style::nav(),
        1 => style::healthy(),
        2 => style::warning(),
        3 => style::critical(),
        _ => style::accent(),
    }
}

fn hexc(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Draws `plot` into `rect` (the whole `.review-plot` box).
pub fn show(ui: &mut egui::Ui, rect: Rect, plot: &Plot, bands: &[Band], st: &mut PlotState, id: usize) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0, style::surface_1());
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + MARGIN.0, rect.top() + MARGIN.2),
        Pos2::new(rect.right() - MARGIN.1, rect.bottom() - MARGIN.3),
    );
    if inner.width() < 20.0 || inner.height() < 20.0 {
        return;
    }
    let Some((x0, x1, y0, y1)) = plot.extent() else {
        let g = style::galley(ui, "Nothing to plot.", 11.0, Weight::W400, style::text_3(), 0.0);
        painter.galley(rect.center() - g.size() / 2.0, g, style::text_3());
        return;
    };
    let (mut xr, mut yr) = (axis_range(x0, x1, false), axis_range(y0, y1, true));
    if plot.equal {
        let (a, b) = equalize(axis_range(x0, x1, true), yr, inner.width(), inner.height());
        xr = a;
        yr = b;
    }
    if let Some(v) = st.view {
        xr = (v[0], v[1]);
        yr = (v[2], v[3]);
    }
    let to_px = |x: f64, y: f64| {
        Pos2::new(
            inner.left() + ((x - xr.0) / (xr.1 - xr.0)) as f32 * inner.width(),
            inner.bottom() - ((y - yr.0) / (yr.1 - yr.0)) as f32 * inner.height(),
        )
    };
    let from_px = |p: Pos2| {
        (
            xr.0 + f64::from((p.x - inner.left()) / inner.width()) * (xr.1 - xr.0),
            yr.0 + f64::from((inner.bottom() - p.y) / inner.height()) * (yr.1 - yr.0),
        )
    };

    // the traces, reduced once for this width
    let cols = inner.width().max(1.0) as u32;
    if st.width != cols || st.traces.len() != plot.series.len() {
        st.width = cols;
        st.traces = plot
            .series
            .iter()
            .map(|s| {
                let pts = reduce(&s.x, &s.y, cols as usize);
                if s.step && !s.markers {
                    stepped(&pts)
                } else {
                    pts
                }
            })
            .collect();
    }

    let plot_clip = ui.painter_at(inner);
    // mode bands behind a time plot
    if plot.is_time() {
        for b in bands {
            let l = to_px(b.start, 0.0).x.clamp(inner.left(), inner.right());
            let r = to_px(b.end, 0.0).x.clamp(inner.left(), inner.right());
            if r > l {
                plot_clip.rect_filled(
                    Rect::from_min_max(Pos2::new(l, inner.top()), Pos2::new(r, inner.bottom())),
                    0,
                    hexc(b.color).gamma_multiply(0.16),
                );
            }
        }
    }
    // grid, zero lines and tick labels
    let (xt, xstep) = nice_ticks(xr.0, xr.1, (inner.width() / 90.0).max(2.0) as usize);
    let (yt, ystep) = nice_ticks(yr.0, yr.1, (inner.height() / 45.0).max(2.0) as usize);
    for t in &xt {
        let x = to_px(*t, 0.0).x;
        let zero = t.abs() < xstep * 1e-9;
        plot_clip.vline(
            x,
            inner.y_range(),
            Stroke::new(1.0, if zero { style::border() } else { style::border_soft() }),
        );
        let g = style::galley_mono(ui, &tick_label(*t, xstep), 9.0, style::text_3());
        painter.galley(
            Pos2::new(x - g.size().x / 2.0, inner.bottom() + 5.0),
            g,
            style::text_3(),
        );
    }
    for t in &yt {
        let y = to_px(0.0, *t).y;
        let zero = t.abs() < ystep * 1e-9;
        plot_clip.hline(
            inner.x_range(),
            y,
            Stroke::new(1.0, if zero { style::border() } else { style::border_soft() }),
        );
        let g = style::galley_mono(ui, &tick_label(*t, ystep), 9.0, style::text_3());
        painter.galley(
            Pos2::new(inner.left() - 6.0 - g.size().x, y - g.size().y / 2.0),
            g,
            style::text_3(),
        );
    }
    // band names, turned on their side
    if plot.is_time() {
        let span = x1 - x0;
        for b in bands {
            let (from, to) = (b.start.max(x0), b.end.min(x1));
            if span > 0.0 && to - from >= span * 0.025 && st.view.is_none() {
                let g = style::galley(ui, &b.label, 9.0, Weight::W400, hexc(b.color).gamma_multiply(0.85), 0.0);
                let mid = to_px((from + to) / 2.0, 0.0).x;
                let at = Pos2::new(
                    mid - g.size().y / 2.0,
                    inner.top() + 0.015 * inner.height() + g.size().x,
                );
                painter.add(TextShape::new(at, g, hexc(b.color)).with_angle(-std::f32::consts::FRAC_PI_2));
            }
        }
    }
    // the threshold
    if let Some(t) = plot.threshold {
        let y = to_px(0.0, t).y;
        if y >= inner.top() && y <= inner.bottom() {
            dotted(&plot_clip, inner.left(), inner.right(), y, style::critical());
        }
    }
    // the traces
    for (i, (s, pts)) in plot.series.iter().zip(&st.traces).enumerate() {
        let color = series_color(i);
        if s.markers {
            for (x, y) in pts.iter().filter(|p| p.1.is_finite()) {
                let c = to_px(*x, *y);
                if inner.expand(4.0).contains(c) {
                    plot_clip.circle_stroke(c, 2.5, Stroke::new(1.0, color));
                }
            }
        } else {
            let mut run: Vec<Pos2> = Vec::new();
            for (x, y) in pts {
                if y.is_finite() && x.is_finite() {
                    run.push(to_px(*x, *y));
                } else if !run.is_empty() {
                    plot_clip.add(egui::Shape::line(std::mem::take(&mut run), Stroke::new(1.4, color)));
                }
            }
            if run.len() == 1 {
                plot_clip.circle_filled(run[0], 1.2, color);
            } else if !run.is_empty() {
                plot_clip.add(egui::Shape::line(run, Stroke::new(1.4, color)));
            }
        }
    }
    // axis titles
    let xt = if plot.xlabel.is_empty() {
        "Time (s)"
    } else {
        plot.xlabel.as_str()
    };
    let g = style::galley_mono(ui, xt, 10.0, style::text_2());
    painter.galley(
        Pos2::new(inner.center().x - g.size().x / 2.0, inner.bottom() + 20.0),
        g,
        style::text_2(),
    );
    let yt = if plot.ylabel.is_empty() {
        plot.unit.as_str()
    } else {
        plot.ylabel.as_str()
    };
    if !yt.is_empty() {
        let g = style::galley_mono(ui, yt, 10.0, style::text_2());
        let at = Pos2::new(rect.left() + 4.0, inner.center().y + g.size().x / 2.0);
        painter.add(TextShape::new(at, g, style::text_2()).with_angle(-std::f32::consts::FRAC_PI_2));
    }
    // the legend, centred under the axis title
    let items: Vec<_> = plot
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| (i, style::galley_mono(ui, &s.name, 12.0, style::text_2())))
        .collect();
    let total: f32 = items.iter().map(|(_, g)| 30.0 + g.size().x + 14.0).sum::<f32>() - 14.0;
    let mut lx = (rect.center().x - total / 2.0).max(rect.left() + 4.0);
    let ly = inner.bottom() + 46.0;
    for (i, g) in items {
        let c = series_color(i);
        let s = &plot.series[i];
        if s.markers {
            painter.circle_stroke(Pos2::new(lx + 15.0, ly + g.size().y / 2.0), 3.0, Stroke::new(1.0, c));
        } else {
            painter.hline(lx..=lx + 30.0, ly + g.size().y / 2.0, Stroke::new(2.0, c));
        }
        let w = g.size().x;
        painter.galley(Pos2::new(lx + 30.0 + 4.0, ly), g, style::text_2());
        lx += 30.0 + 4.0 + w + 10.0;
    }

    // hover, zoom
    let resp = ui.interact(inner, ui.scope_id().with(("plot", id)), egui::Sense::click_and_drag());
    if resp.double_clicked() {
        st.view = None;
        st.drag_from = None;
    }
    if resp.drag_started() {
        st.drag_from = resp.interact_pointer_pos();
    }
    if let (Some(a), Some(b)) = (st.drag_from, resp.interact_pointer_pos()) {
        if resp.dragged() {
            plot_clip.rect_filled(Rect::from_two_pos(a, b), 0, style::accent().gamma_multiply(0.18));
            plot_clip.rect_stroke(
                Rect::from_two_pos(a, b),
                0,
                Stroke::new(1.0, style::accent()),
                egui::StrokeKind::Inside,
            );
        }
    }
    if resp.drag_stopped() {
        if let (Some(a), Some(b)) = (st.drag_from.take(), resp.interact_pointer_pos()) {
            let r = Rect::from_two_pos(a, b);
            if r.width() > 6.0 && r.height() > 6.0 {
                let (xa, ya) = from_px(Pos2::new(r.left(), r.bottom()));
                let (xb, yb) = from_px(Pos2::new(r.right(), r.top()));
                st.view = Some([xa, xb, ya, yb]);
            }
        }
    }
    if let Some(p) = resp.hover_pos().filter(|_| !resp.dragged()) {
        let mut best: Option<(f32, usize, (f64, f64), Pos2)> = None;
        for (i, pts) in st.traces.iter().enumerate() {
            for (x, y) in pts.iter().filter(|q| q.0.is_finite() && q.1.is_finite()) {
                let c = to_px(*x, *y);
                let d = c.distance(p);
                if best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((d, i, (*x, *y), c));
                }
            }
        }
        if let Some((d, i, (x, y), c)) = best.filter(|b| b.0 < 40.0) {
            let _ = d;
            plot_clip.circle_filled(c, 3.5, series_color(i));
            let text = format!(
                "{}\n{}: {}\n{}: {}",
                plot.series[i].name,
                xt,
                nice_value(x, xstep),
                if yt.is_empty() { "y" } else { yt },
                nice_value(y, ystep)
            );
            let g = style::galley_mono(ui, &text, 10.0, style::text_1());
            let box_ = Rect::from_min_size(
                Pos2::new(
                    (c.x + 10.0).min(inner.right() - g.size().x - 12.0).max(inner.left()),
                    (c.y - g.size().y - 12.0).max(inner.top()),
                ),
                g.size() + Vec2::new(10.0, 8.0),
            );
            let p = ui.painter().clone();
            p.rect_filled(box_, 4, style::surface_2());
            p.rect_stroke(box_, 4, Stroke::new(1.0, series_color(i)), egui::StrokeKind::Inside);
            p.galley(box_.min + Vec2::new(5.0, 4.0), g, style::text_1());
        }
    }
}

fn nice_value(v: f64, step: f64) -> String {
    let extra = if step >= 1.0 {
        2
    } else {
        (-step.log10().floor()) as usize + 1
    };
    format!("{v:.extra$}")
}

fn dotted(p: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let mut x = x0;
    while x < x1 {
        p.hline(x..=(x + 2.0).min(x1), y, Stroke::new(1.0, color));
        x += 4.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ticks_land_on_one_two_or_five() {
        let (t, step) = nice_ticks(0.0, 100.0, 5);
        assert_eq!(step, 20.0);
        assert_eq!(t, [0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        let (t, step) = nice_ticks(-0.3, 0.9, 6);
        assert!((step - 0.2).abs() < 1e-12);
        assert!((t[0] + 0.2).abs() < 1e-9 && (t.last().unwrap() - 0.8).abs() < 1e-9);
        let (t, step) = nice_ticks(3.0, 3.0, 5);
        assert_eq!((t, step), (vec![3.0], 1.0));
        assert_eq!(nice_ticks(0.0, 7.0, 7).1, 1.0);
        assert_eq!(nice_ticks(0.0, 4500.0, 5).1, 1000.0);
    }

    #[test]
    fn labels_carry_the_decimals_the_step_needs() {
        assert_eq!(tick_label(40.0, 20.0), "40");
        assert_eq!(tick_label(0.4, 0.2), "0.4");
        assert_eq!(tick_label(-0.05, 0.05), "-0.05");
        assert_eq!(tick_label(1e-18, 0.5), "0.0");
        assert_eq!(tick_label(0.0, 0.5), "0.0");
    }

    #[test]
    fn a_flat_axis_is_given_room_and_y_is_padded() {
        assert_eq!(axis_range(2.0, 2.0, true), (1.9, 2.1));
        assert_eq!(axis_range(0.0, 0.0, false), (-1.0, 1.0));
        assert_eq!(axis_range(0.0, 10.0, true), (-0.5, 10.5));
        assert_eq!(axis_range(0.0, 10.0, false), (0.0, 10.0));
    }

    #[test]
    fn a_long_trace_keeps_its_spikes_and_its_order() {
        let n = 100_000;
        let x: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let mut y: Vec<f64> = (0..n).map(|i| (i as f64 / 500.0).sin()).collect();
        y[54_321] = 50.0;
        let r = reduce(&x, &y, 200);
        assert!(r.len() <= 200 * 4);
        assert!(r.iter().any(|p| p.1 == 50.0));
        assert!(r.windows(2).all(|w| w[0].0 <= w[1].0));
        assert_eq!((r[0].0, r.last().unwrap().0), (0.0, 99_999.0));
        // a short trace is left alone
        assert_eq!(reduce(&[0.0, 1.0], &[5.0, 6.0], 200), [(0.0, 5.0), (1.0, 6.0)]);
    }

    #[test]
    fn a_gap_in_the_data_stays_a_gap() {
        let r = reduce(&[0.0, 1.0, 2.0], &[1.0, f64::NAN, 3.0], 200);
        assert!(r[1].1.is_nan());
    }

    #[test]
    fn a_step_holds_the_value_then_jumps() {
        assert_eq!(
            stepped(&[(0.0, 1.0), (2.0, 3.0), (5.0, 2.0)]),
            [(0.0, 1.0), (2.0, 1.0), (2.0, 3.0), (5.0, 3.0), (5.0, 2.0)]
        );
    }

    #[test]
    fn equal_axes_make_a_metre_as_long_east_as_north() {
        let ((x0, x1), (y0, y1)) = equalize((0.0, 100.0), (0.0, 10.0), 400.0, 200.0);
        let (sx, sy) = ((x1 - x0) / 400.0, (y1 - y0) / 200.0);
        assert!((sx - sy).abs() < 1e-12);
        assert_eq!((x0, x1), (0.0, 100.0));
        assert_eq!((y0 + y1) / 2.0, 5.0);
    }

    #[test]
    fn a_plot_is_read_from_the_backends_json() {
        let p = Plot::from_json(&json!({
            "title": "Track", "group": "Position", "unit": "m", "xlabel": "East (m)", "ylabel": "North (m)",
            "equal": true, "threshold": 1.0, "origin": {"lat": 47.0, "lon": 8.0},
            "series": [
                {"name": "estimate", "x": [0, 1, 2], "y": [0, null, 2]},
                {"name": "setpoint", "x": [0, 1], "y": [1, 1], "draw": "markers"},
                {"name": "mode", "x": [0, 1], "y": [1, 2], "shape": "hv"}]
        }));
        assert_eq!((p.title.as_str(), p.equal, p.is_time()), ("Track", true, false));
        assert_eq!(p.origin, Some((47.0, 8.0)));
        assert!(p.series[0].y[1].is_nan());
        assert!(p.series[1].markers && p.series[2].step);
        assert_eq!(p.extent(), Some((0.0, 2.0, 0.0, 2.0)));
        let empty = Plot::from_json(&json!({"series": [{"name": "x", "x": [], "y": []}]}));
        assert!(empty.extent().is_none() && empty.is_time());
    }
}
