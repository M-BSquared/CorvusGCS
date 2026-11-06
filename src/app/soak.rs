//! `CORVUS_GUI_SOAK=1` drives the window by itself for a soak run: every few
//! seconds it moves on to the next step of a fixed cycle (the map panned and
//! zoomed, the 3D map opened and moved, every page, a setup tile at a time, the
//! language switched). Nothing is sent to the vehicle; it only changes what is
//! shown. `python/tools/gui_soak.py` starts it and samples the memory.
//! `CORVUS_GUI_SOAK_STEP=2.5` sets the seconds a step lasts (4 by default).

use eframe::egui;

use super::App;
use crate::app::nav::Page;

const TABS: [&str; 9] = [
    "interface",
    "layout",
    "flight",
    "map",
    "controls",
    "connections",
    "data",
    "plugins",
    "about",
];

/// Steps of one lap of the cycle.
const LAP: u64 = 12;
/// Seconds before the first step, for the window and the backend to settle.
const START_S: f64 = 12.0;

#[derive(Default)]
pub struct Soak {
    done: Option<u64>,
}

/// The step number at `now`, `None` before the start.
pub fn step_at(now: f64, step_s: f64) -> Option<u64> {
    (now >= START_S && step_s > 0.0).then(|| ((now - START_S) / step_s) as u64)
}

impl App {
    pub(super) fn soak_tick(&mut self, ctx: &egui::Context, now: f64) {
        if std::env::var_os("CORVUS_GUI_SOAK").is_none() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let step_s: f64 = std::env::var("CORVUS_GUI_SOAK_STEP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4.0);
        let Some(k) = step_at(now, step_s) else { return };
        if self.soak.done == Some(k) {
            return;
        }
        self.soak.done = Some(k);
        let lap = k / LAP;
        let wobble = (lap % 7) as f64 * 0.01;
        match k % LAP {
            0 => {
                self.page = Page::Home;
                self.map.set_three_d(false);
                self.map.go_to(11.5 + wobble, 47.4 + wobble, 13.0);
            }
            1 => self.map.go_to(11.52 + wobble, 47.41 + wobble, 17.0),
            2 => self.map.go_to(11.3 - wobble, 47.2 + wobble, 10.0),
            3 => {
                self.map.three_settings(ctx);
                self.map.set_three_d(true);
            }
            4 => {
                let (t, _) = self.map.three_settings(ctx);
                t.start_at(11.5 + wobble, 47.4, 14.0, 40.0 * (lap % 9) as f64);
            }
            5 => {
                let (t, _) = self.map.three_settings(ctx);
                t.start_at(11.6, 47.45 + wobble, 12.0, 200.0);
            }
            6 => {
                self.map.set_three_d(false);
                let ids = crate::setup::tile_ids();
                let id = ids[(lap as usize * 2) % ids.len().max(1)];
                self.page = Page::Setup;
                self.setup.open(id, ctx);
            }
            7 => self.page = Page::Analysis,
            8 => self.page = Page::Logs,
            9 => {
                self.page = Page::Settings;
                self.settings.open_tab(TABS[(lap % TABS.len() as u64) as usize]);
            }
            10 => self.page = Page::Mission,
            _ => {
                let next = if crate::i18n::language() == "de" { "en" } else { "de" };
                crate::i18n::set_language(next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cycle_starts_late_and_advances_with_time() {
        assert_eq!(step_at(3.0, 4.0), None);
        assert_eq!(step_at(START_S, 4.0), Some(0));
        assert_eq!(step_at(START_S + 9.0, 4.0), Some(2));
        assert_eq!(step_at(100.0, 0.0), None);
    }
}
