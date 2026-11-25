//! The geometry behind the tools: places as `[lat, lon]` in degrees, lengths in
//! metres, bearings in degrees clockwise from north. Spherical, which is right
//! to a fraction of a percent over the distances an aircraft flies here.

const EARTH_RADIUS_M: f64 = 6_371_008.8;

pub type Place = [f64; 2];

fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

/// How far `a` is from `b`.
pub fn distance(a: Place, b: Place) -> f64 {
    let (p1, p2) = (a[0].to_radians(), b[0].to_radians());
    let (dp, dl) = ((b[0] - a[0]).to_radians(), (b[1] - a[1]).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// The direction from `a` to `b`, 0 to 360.
pub fn bearing(a: Place, b: Place) -> f64 {
    let (p1, p2, dl) = (a[0].to_radians(), b[0].to_radians(), (b[1] - a[1]).to_radians());
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// Where one gets from `a` going `metres` in the direction `bearing_deg`.
pub fn project(a: Place, bearing_deg: f64, metres: f64) -> Place {
    let (phi, theta, d) = (a[0].to_radians(), bearing_deg.to_radians(), metres / EARTH_RADIUS_M);
    let sin = (phi.sin() * d.cos() + phi.cos() * d.sin() * theta.cos()).clamp(-1.0, 1.0);
    let lat = sin.asin();
    let lon = a[1].to_radians() + (theta.sin() * d.sin() * phi.cos()).atan2(d.cos() - phi.sin() * sin);
    [lat.to_degrees(), wrap_lon(lon.to_degrees())]
}

/// Where one gets from `a` going `north` and `east` metres.
pub fn offset(a: Place, north: f64, east: f64) -> Place {
    if north == 0.0 && east == 0.0 {
        return a;
    }
    project(a, east.atan2(north).to_degrees(), north.hypot(east))
}

/// `p` as metres `[east, north]` of `origin`.
pub fn local(origin: Place, p: Place) -> [f64; 2] {
    let d = distance(origin, p);
    let b = bearing(origin, p).to_radians();
    [d * b.sin(), d * b.cos()]
}

/// The length of a path.
pub fn length(path: &[Place]) -> f64 {
    path.windows(2).map(|w| distance(w[0], w[1])).sum()
}

/// The area inside a ring, in square metres.
pub fn area(ring: &[Place]) -> f64 {
    let Some(first) = ring.first() else { return 0.0 };
    let p: Vec<[f64; 2]> = ring.iter().map(|q| local(*first, *q)).collect();
    let twice: f64 = (0..p.len())
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    twice.abs() / 2.0
}

/// `n` points around `center`, the first at `start_deg`, not closed.
pub fn ring(center: Place, radius: f64, n: usize, start_deg: f64, clockwise: bool) -> Vec<Place> {
    let turn = if clockwise { 360.0 } else { -360.0 };
    (0..n)
        .map(|i| project(center, start_deg + turn * i as f64 / n.max(1) as f64, radius))
        .collect()
}

/// A slice of a circle from `from_deg` clockwise to `to_deg`, the centre first.
pub fn sector(center: Place, radius: f64, from_deg: f64, to_deg: f64) -> Vec<Place> {
    let sweep = match (to_deg - from_deg).rem_euclid(360.0) {
        0.0 => 360.0,
        s => s,
    };
    let n = ((sweep / 6.0).ceil() as usize).max(2);
    let mut out = vec![center];
    out.extend((0..=n).map(|i| project(center, from_deg + sweep * i as f64 / n as f64, radius)));
    out
}

/// Parallel lines `spacing` apart across the area, flown back and forth in the
/// direction `angle_deg`. An area with a bay is crossed line by line, so the way
/// from one piece of a line to the next leads over the bay. `None` when that
/// takes more than `limit` points.
pub fn grid(area: &[Place], spacing: f64, angle_deg: f64, limit: usize) -> Option<Vec<Place>> {
    let first = *area.first()?;
    if !spacing.is_finite() || spacing <= 0.0 || !angle_deg.is_finite() {
        return None;
    }
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    // u runs along the lines, v across them.
    let pts: Vec<[f64; 2]> = area
        .iter()
        .map(|p| {
            let [e, n] = local(first, *p);
            [e * sin + n * cos, e * cos - n * sin]
        })
        .collect();
    let (lo, hi) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
        (lo.min(p[1]), hi.max(p[1]))
    });
    // A hundredth of a line over is the rounding of the projection, not ground to cover.
    let lines = ((hi - lo) / spacing - 0.01).ceil().max(1.0);
    if !lines.is_finite() || lines * 2.0 > limit as f64 {
        return None;
    }
    let lines = lines as usize;
    let start = lo + ((hi - lo) - (lines - 1) as f64 * spacing) / 2.0;
    let mut out: Vec<Place> = Vec::new();
    for k in 0..lines {
        let v = start + k as f64 * spacing;
        let mut cuts: Vec<f64> = (0..pts.len())
            .filter_map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                ((a[1] <= v) != (b[1] <= v)).then(|| a[0] + (b[0] - a[0]) * (v - a[1]) / (b[1] - a[1]))
            })
            .collect();
        cuts.sort_by(f64::total_cmp);
        if k % 2 == 1 {
            cuts.reverse();
        }
        if out.len() + cuts.len() > limit {
            return None;
        }
        out.extend(cuts.iter().map(|u| offset(first, u * cos - v * sin, u * sin + v * cos)));
    }
    Some(out)
}

/// An expanding square around `center` with legs `spacing` apart, until it has
/// covered `radius`. `None` when that takes more than `limit` points.
pub fn spiral(center: Place, radius: f64, spacing: f64, angle_deg: f64, limit: usize) -> Option<Vec<Place>> {
    if !(spacing.is_finite() && spacing > 0.0 && radius.is_finite() && radius > 0.0) {
        return None;
    }
    let mut out = vec![center];
    let (mut e, mut n) = (0.0f64, 0.0f64);
    let mut leg = 0usize;
    while e.abs().max(n.abs()) < radius {
        let long = spacing * (leg / 2 + 1) as f64;
        match leg % 4 {
            0 => n += long,
            1 => e += long,
            2 => n -= long,
            _ => e -= long,
        }
        leg += 1;
        if out.len() >= limit {
            return None;
        }
        out.push(project(center, e.atan2(n).to_degrees() + angle_deg, e.hypot(n)));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: Place = [48.0795, 11.6385];

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn turned(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    #[test]
    fn going_there_and_measuring_back_agree() {
        for (bear, d) in [(0.0, 100.0), (90.0, 2500.0), (200.0, 40.0), (359.0, 10_000.0)] {
            let p = project(HERE, bear, d);
            assert!(near(distance(HERE, p), d, 0.01), "{bear} {d}");
            assert!(turned(bearing(HERE, p), bear) < 0.01, "{bear} {d}");
        }
        let p = offset(HERE, 30.0, -40.0);
        assert!(near(distance(HERE, p), 50.0, 0.01));
        let [e, n] = local(HERE, p);
        assert!(near(e, -40.0, 0.01) && near(n, 30.0, 0.01));
        assert_eq!(offset(HERE, 0.0, 0.0), HERE);
        let over = project([0.0, 179.999], 90.0, 1000.0);
        assert!(over[1] < -179.0, "over the date line: {over:?}");
    }

    #[test]
    fn a_square_has_its_area_and_its_round() {
        let square = [
            HERE,
            offset(HERE, 100.0, 0.0),
            offset(HERE, 100.0, 100.0),
            offset(HERE, 0.0, 100.0),
        ];
        assert!(near(area(&square), 10_000.0, 10.0));
        assert!(near(length(&square), 300.0, 0.1));
        assert!(ring(HERE, 20.0, 12, 0.0, true)
            .iter()
            .all(|p| near(distance(HERE, *p), 20.0, 0.001)));
        assert!(near(bearing(HERE, ring(HERE, 20.0, 4, 0.0, false)[1]), 270.0, 0.01));
        let s = sector(HERE, 50.0, 350.0, 10.0);
        assert_eq!(s[0], HERE);
        assert!(near(bearing(HERE, s[1]), 350.0, 0.1) && near(bearing(HERE, s[s.len() - 1]), 10.0, 0.1));
    }

    #[test]
    fn a_grid_covers_the_area_in_lines_and_stays_in_it() {
        let square = [
            HERE,
            offset(HERE, 200.0, 0.0),
            offset(HERE, 200.0, 100.0),
            offset(HERE, 0.0, 100.0),
        ];
        let g = grid(&square, 25.0, 0.0, 200).unwrap();
        assert_eq!(g.len(), 8, "four lines north and south");
        for pair in g.chunks(2) {
            assert!(near(distance(pair[0], pair[1]), 200.0, 1.0));
        }
        assert!(near(distance(g[1], g[2]), 25.0, 0.5), "lines are flown back and forth");
        for p in &g {
            let [e, n] = local(HERE, *p);
            assert!((-1.0..=101.0).contains(&e) && (-1.0..=201.0).contains(&n), "{e} {n}");
        }
        let turned = grid(&square, 25.0, 90.0, 200).unwrap();
        assert_eq!(turned.len(), 16);
        assert!(grid(&square, 0.1, 0.0, 200).is_none(), "too many lines");
        assert!(grid(&square, 0.0, 0.0, 200).is_none());
        assert!(grid(&square, 1e9, 0.0, 200).is_some_and(|g| g.len() == 2));
    }

    #[test]
    fn a_spiral_widens_until_it_has_covered_the_radius() {
        let s = spiral(HERE, 100.0, 20.0, 0.0, 200).unwrap();
        assert!(near(distance(s[0], s[1]), 20.0, 0.1) && near(distance(s[2], s[3]), 40.0, 0.1));
        assert!(s.iter().any(|p| distance(HERE, *p) >= 100.0));
        assert!(spiral(HERE, 10_000.0, 1.0, 0.0, 200).is_none());
    }
}
