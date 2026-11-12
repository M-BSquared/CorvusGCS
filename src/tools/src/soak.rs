//! A soak run of the interface: started with `CORVUS_GUI_SOAK` (it then
//! drives itself, see `src/app/soak.rs`) in a sandbox home, never the
//! operator's own `~/.corvus`, and sampled for memory, threads and open files
//! into a CSV. The summary gives the median memory of the first and the last
//! quarter, the peak and the slope after a two minute warm up.
//!
//! ```text
//! corvus-build soak --binary target/release/corvus-gcs --minutes 15 --csv soak.csv [--mode 3d] [--at 11.6385,48.0795] [--home DIR] [--max-mb 1500]
//! ```
//!
//! On macOS the footprint column holds the physical footprint, which counts
//! what the system compressed or swapped out; the resident size alone drops
//! when it does and hides a program that holds gigabytes.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Options {
    pub binary: PathBuf,
    pub minutes: f64,
    pub every: f64,
    pub step: String,
    pub mode: String,
    pub at: Option<String>,
    pub home: Option<PathBuf>,
    pub csv: PathBuf,
    pub max_mb: f64,
}

/// Resident MB, threads and open files of `pid`, `None` once it is gone.
pub fn sample(pid: u32) -> Option<(f64, u32, u32)> {
    let out = |cmd: &mut Command| {
        cmd.output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let rss: f64 = out(Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]))?
        .trim()
        .parse()
        .ok()?;
    let threads = if cfg!(target_os = "macos") {
        out(Command::new("ps").args(["-M", "-p", &pid.to_string()])).map_or(0, |s| s.lines().count().saturating_sub(1))
    } else {
        out(Command::new("ps").args(["-o", "nlwp=", "-p", &pid.to_string()]))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    } as u32;
    let fds = out(Command::new("lsof").args(["-p", &pid.to_string()]))
        .map_or(0, |s| s.lines().count().saturating_sub(1)) as u32;
    Some((rss / 1024.0, threads, fds))
}

/// The macOS physical footprint of `pid` in MB; `None` elsewhere.
pub fn footprint(pid: u32) -> Option<f64> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let o = Command::new("footprint").args(["-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&o.stdout).lines().find_map(|l| {
        let l = l.trim();
        if !l.starts_with("phys_footprint:") || l.contains("peak") {
            return None;
        }
        let mut words = l.split_whitespace().skip(1);
        let n: f64 = words.next()?.parse().ok()?;
        Some(match words.next() {
            Some("KB") => n / 1024.0,
            Some("GB") => n * 1024.0,
            _ => n,
        })
    })
}

/// Least squares slope of `points`.
pub fn slope(points: &[(f64, f64)]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let n = points.len() as f64;
    let (mx, my) = (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let den: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    if den == 0.0 {
        0.0
    } else {
        points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / den
    }
}

/// The median of `v`.
pub fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    match s.len() {
        0 => 0.0,
        n if n % 2 == 1 => s[n / 2],
        n => (s[n / 2 - 1] + s[n / 2]) / 2.0,
    }
}

fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs the soak; the exit code: 0, 1 when the interface could not run, 2
/// when it passed `max_mb`.
pub fn run(o: &Options) -> i32 {
    let home = o
        .home
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("corvus-soak-{}", std::process::id())));
    if let Err(e) = std::fs::create_dir_all(&home) {
        eprintln!("{}: {e}", home.display());
        return 1;
    }
    let mut cmd = Command::new(&o.binary);
    cmd.env("HOME", &home)
        .env("CORVUS_ALLOW_MULTI", "1")
        .env("CORVUS_GUI_SOAK", &o.mode)
        .env("CORVUS_GUI_SOAK_STEP", &o.step)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(at) = &o.at {
        cmd.env("CORVUS_GUI_SOAK_AT", at);
    }
    let mut app = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {e}", o.binary.display());
            return 1;
        }
    };
    let mut csv = match std::fs::File::create(&o.csv) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{}: {e}", o.csv.display());
            stop(&mut app);
            return 1;
        }
    };
    let _ = writeln!(csv, "t_s,app_mb,app_threads,app_fds,app_footprint_mb");
    let t0 = Instant::now();
    let mut rows: Vec<(f64, f64, u32, u32, Option<f64>)> = Vec::new();
    while t0.elapsed().as_secs_f64() < o.minutes * 60.0 {
        if let Ok(Some(status)) = app.try_wait() {
            eprintln!(
                "the interface exited with {status} after {:.0} s",
                t0.elapsed().as_secs_f64()
            );
            break;
        }
        if let Some((mb, threads, fds)) = sample(app.id()) {
            let fp = footprint(app.id());
            let t = t0.elapsed().as_secs_f64().round();
            let line = format!(
                "{t},{mb:.1},{threads},{fds},{}",
                fp.map_or(String::new(), |f| format!("{f:.0}"))
            );
            let _ = writeln!(csv, "{line}");
            let _ = csv.flush();
            println!("{line}");
            rows.push((t, mb, threads, fds, fp));
        }
        std::thread::sleep(Duration::from_secs_f64(o.every));
    }
    stop(&mut app);
    if o.home.is_none() {
        let _ = std::fs::remove_dir_all(&home);
    }
    if rows.len() < 8 {
        return 0;
    }
    let q = rows.len() / 4;
    let mem: Vec<f64> = rows.iter().map(|r| r.4.unwrap_or(r.1)).collect();
    let tail: Vec<(f64, f64)> = rows
        .iter()
        .filter(|r| r.0 >= 120.0)
        .map(|r| (r.0 / 60.0, r.4.unwrap_or(r.1)))
        .collect();
    let peak = mem.iter().copied().fold(0.0, f64::max);
    println!(
        "memory: first quarter {:.0} MB, last quarter {:.0} MB, max {peak:.0} MB, slope after 2 min {:+.2} MB/min",
        median(&mem[..q]),
        median(&mem[mem.len() - q..]),
        slope(&tail)
    );
    println!(
        "threads {} -> {} (max {}), files {} -> {} (max {})",
        rows[0].2,
        rows[rows.len() - 1].2,
        rows.iter().map(|r| r.2).max().unwrap_or(0),
        rows[0].3,
        rows[rows.len() - 1].3,
        rows.iter().map(|r| r.3).max().unwrap_or(0)
    );
    if o.max_mb > 0.0 && peak > o.max_mb {
        eprintln!(
            "FAIL: the interface peaked at {peak:.0} MB, over the {:.0} MB bound",
            o.max_mb
        );
        return 2;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slope_and_median() {
        assert_eq!(slope(&[(0.0, 1.0), (1.0, 3.0), (2.0, 5.0)]), 2.0);
        assert_eq!(slope(&[(0.0, 1.0)]), 0.0);
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    #[cfg(unix)]
    #[test]
    fn this_process_can_be_sampled() {
        let (mb, threads, _) = sample(std::process::id()).expect("ps answers for this process");
        assert!(mb > 0.0 && threads >= 1);
    }
}
