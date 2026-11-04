//! A line chart in the look of the web interface's Plotly graphs
//! (`Corvus.ui.plotlyTheme`, `setupShared.plotlyLayout`): the surface as the
//! plot, hairline grid, a zero line and axes in the border colour, 9 px mono
//! tick labels, 10 px axis titles, auto ranged axes. Several lines, one of
//! which may be dotted (a setpoint beside the response).

use eframe::egui::{self, epaint::TextShape, Color32, Pos2, Rect, Shape, Stroke, Vec2};

use crate::ui::style;

/// `margin: { l: 44, r: 8, t: 6, b: 26 }` of `plotlyLayout`.
pub const MARGIN: (f32, f32, f32, f32) = (44.0, 8.0, 6.0, 26.0);

/// One line: its samples (`None` is a gap), colour, width and dash.
pub struct Series<'a> {
    pub y: &'a [Option<f64>],
    pub color: Color32,
    pub width: f32,
    pub dotted: bool,
}

/// A "nice" tick step (1, 2, 5 times a power of ten) giving about `count`
/// ticks over `span`.
pub fn tick_step(span: f64, count: f64) -> f64 {
    if span.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) || !span.is_finite() {
        return 1.0;
    }
    let raw = span / count.max(1.0);
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let nice = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// The ticks inside `lo ..= hi`.
pub fn ticks(lo: f64, hi: f64, count: f64) -> Vec<f64> {
    let step = tick_step(hi - lo, count);
    let first = (lo / step).ceil();
    let mut out = Vec::new();
    let mut i = 0.0;
    while (first + i) * step <= hi + step * 1e-9 && out.len() < 200 {
        out.push(((first + i) * step * 1e9).round() / 1e9);
        i += 1.0;
    }
    out
}

/// A tick label: whole numbers as they are, others with as few decimals as
/// the step needs.
pub fn tick_label(v: f64, step: f64) -> String {
    let decimals = if step >= 1.0 {
        0
    } else {
        (-step.log10().floor()).max(0.0) as usize
    };
    // no minus sign on a zero
    let v = if v.abs() < step * 1e-9 { 0.0 } else { v };
    format!("{v:.decimals$}")
}

/// The y range of the data with Plotly's 5 percent either side, or a unit
/// either side of a flat line.
pub fn y_range(series: &[Series<'_>]) -> (f64, f64) {
    let vals: Vec<f64> = series.iter().flat_map(|s| s.y.iter().flatten().copied()).collect();
    if vals.is_empty() {
        return (-1.0, 1.0);
    }
    let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if (hi - lo).abs() < 1e-12 {
        return if lo == 0.0 {
            (-1.0, 1.0)
        } else {
            (lo - lo.abs() * 0.5, lo + lo.abs() * 0.5)
        };
    }
    let pad = (hi - lo) * 0.05;
    (lo - pad, hi + pad)
}

/// Draws the chart into `rect`.
pub fn draw(ui: &egui::Ui, rect: Rect, x: &[f64], series: &[Series<'_>], x_title: &str, y_title: &str) {
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, 0, style::surface_1());
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + MARGIN.0, rect.top() + MARGIN.2),
        Pos2::new(rect.right() - MARGIN.1, rect.bottom() - MARGIN.3),
    );
    if plot.width() < 20.0 || plot.height() < 20.0 {
        return;
    }
    let (x0, x1) = match (x.first(), x.last()) {
        (Some(a), Some(b)) if b > a => (*a, *b),
        (Some(a), _) => (*a - 1.0, *a + 1.0),
        _ => (0.0, 1.0),
    };
    let (y0, y1) = y_range(series);
    let sx = |v: f64| plot.left() + ((v - x0) / (x1 - x0)) as f32 * plot.width();
    let sy = |v: f64| plot.bottom() - ((v - y0) / (y1 - y0)) as f32 * plot.height();
    let grid = Stroke::new(1.0, style::border_soft());
    let axis = Stroke::new(1.0, style::border());
    let tick_font = style::mono(9.0);
    let xt = ticks(x0, x1, f64::from(plot.width() / 70.0));
    let yt = ticks(y0, y1, f64::from(plot.height() / 40.0));
    let xs = tick_step(x1 - x0, f64::from(plot.width() / 70.0));
    let ys = tick_step(y1 - y0, f64::from(plot.height() / 40.0));
    for t in &xt {
        p.vline(sx(*t), plot.y_range(), grid);
        let g = p.layout_no_wrap(tick_label(*t, xs), tick_font.clone(), style::text_3());
        p.galley(
            Pos2::new(sx(*t) - g.size().x / 2.0, plot.bottom() + 2.0),
            g,
            style::text_3(),
        );
    }
    for t in &yt {
        p.hline(plot.x_range(), sy(*t), grid);
        let g = p.layout_no_wrap(tick_label(*t, ys), tick_font.clone(), style::text_3());
        p.galley(
            Pos2::new(plot.left() - 5.0 - g.size().x, sy(*t) - g.size().y / 2.0),
            g,
            style::text_3(),
        );
    }
    if y0 < 0.0 && y1 > 0.0 {
        p.hline(plot.x_range(), sy(0.0), axis);
    }
    p.vline(plot.left(), plot.y_range(), axis);
    p.hline(plot.x_range(), plot.bottom(), axis);
    let n = x.len();
    for s in series {
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            if run.len() > 1 {
                let stroke = Stroke::new(s.width, s.color);
                if s.dotted {
                    p.extend(Shape::dashed_line(run, stroke, 1.5, 3.0));
                } else {
                    p.add(Shape::line(run.clone(), stroke));
                }
            }
            run.clear();
        };
        for (xv, yv) in x.iter().zip(s.y.iter()).take(n) {
            match yv {
                Some(v) if v.is_finite() => run.push(Pos2::new(sx(*xv), sy(*v).clamp(plot.top(), plot.bottom()))),
                _ => flush(&mut run),
            }
        }
        flush(&mut run);
    }
    let title_font = style::mono(10.0);
    let g = p.layout_no_wrap(x_title.to_string(), title_font.clone(), style::text_2());
    p.galley(
        Pos2::new(plot.center().x - g.size().x / 2.0, rect.bottom() - 1.0 - g.size().y),
        g,
        style::text_2(),
    );
    let g = p.layout_no_wrap(y_title.to_string(), title_font, style::text_2());
    let (gw, gh) = (g.size().x, g.size().y);
    // turned a quarter turn: the text runs up the left edge
    let origin = Pos2::new(rect.left() + 3.0, plot.center().y + gw / 2.0);
    p.add(TextShape::new(origin, g, style::text_2()).with_angle(-std::f32::consts::FRAC_PI_2));
    let _ = (gh, Vec2::ZERO);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_land_on_one_two_and_five_times_a_power_of_ten() {
        assert_eq!(tick_step(10.0, 5.0), 2.0);
        assert_eq!(tick_step(100.0, 4.0), 50.0);
        assert_eq!(tick_step(0.9, 4.0), 0.5);
        assert_eq!(tick_step(0.0, 4.0), 1.0);
        assert_eq!(ticks(0.0, 10.0, 5.0), [0.0, 2.0, 4.0, 6.0, 8.0, 10.0]);
        assert_eq!(ticks(-1.0, 1.0, 4.0), [-1.0, -0.5, 0.0, 0.5, 1.0]);
        assert_eq!(ticks(3.2, 9.1, 3.0), [4.0, 6.0, 8.0]);
    }

    #[test]
    fn labels_carry_only_the_decimals_the_step_needs() {
        assert_eq!(tick_label(20.0, 20.0), "20");
        assert_eq!(tick_label(0.5, 0.5), "0.5");
        assert_eq!(tick_label(0.25, 0.05), "0.25");
        assert_eq!(tick_label(-0.0, 0.5), "0.0");
        assert_eq!(tick_label(-1.0, 0.5), "-1.0");
    }

    #[test]
    fn the_range_pads_five_percent_and_a_flat_line_gets_room() {
        let y = [Some(0.0), Some(10.0), None, Some(5.0)];
        let s = [Series {
            y: &y,
            color: Color32::WHITE,
            width: 1.0,
            dotted: false,
        }];
        let (lo, hi) = y_range(&s);
        assert!((lo + 0.5).abs() < 1e-12 && (hi - 10.5).abs() < 1e-12);
        let flat = [Some(0.0), Some(0.0)];
        let s = [Series {
            y: &flat,
            color: Color32::WHITE,
            width: 1.0,
            dotted: false,
        }];
        assert_eq!(y_range(&s), (-1.0, 1.0));
        assert_eq!(y_range(&[]), (-1.0, 1.0));
    }
}
