//! `Corvus.firstSetup` (python/src/js/first-setup.js): noticing a controller nobody
//! has set up yet, and offering the wizard in a dialog.
//!
//! Two things put such a controller in front of the operator: a fresh board on
//! a live link (the backend says the airframe is unset or no sensor was ever
//! calibrated), and a board with no firmware, which shows up as a bootloader
//! port and never as a link. The dialog offers the wizard and never starts it
//! unasked: the operator may be holding a controller they only wanted to read a
//! parameter from. "Not now" lasts until the next connection; "Do not ask
//! again" is remembered for that controller only. The rules are in
//! `firstsetup_model`; this is the glue that asks the backend and draws.

use eframe::egui::{self, Pos2};
use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::firstsetup_model::{Job, Offer, Payload, Quiet, Watcher};
use crate::ui::kit::{self, Button, Variant};
use crate::ui::modal;
use crate::ui::style::{self, Weight};

type Answer = Pending<Result<Value, String>>;

/// The detector and its dialog.
pub struct Prompt {
    watcher: Watcher,
    fresh: Option<(bool, Answer)>,
    boot: Option<(String, Answer)>,
    offer: Option<Offer>,
}

impl Default for Prompt {
    fn default() -> Prompt {
        Prompt::new(Quiet::load(Quiet::default_path()))
    }
}

impl Prompt {
    pub fn new(quiet: Quiet) -> Prompt {
        Prompt {
            watcher: Watcher::new(quiet),
            fresh: None,
            boot: None,
            offer: None,
        }
    }

    /// Whether the dialog is up.
    #[cfg(test)]
    pub fn open(&self) -> bool {
        self.offer.is_some()
    }

    /// Runs the detector for one frame and draws the dialog. Returns the step
    /// to open the wizard on when "Start setup" was pressed (empty to let the
    /// wizard pick one).
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        server: &Server,
        state: &Value,
        now: f64,
        wizard_open: bool,
    ) -> Option<String> {
        self.step(ctx, server, state, now, wizard_open);
        self.dialog(ctx)
    }

    /// The detector for one frame: what the telemetry changed, the answers
    /// that came in, the check that is due.
    fn step(&mut self, ctx: &egui::Context, server: &Server, state: &Value, now: f64, wizard_open: bool) {
        self.watcher.on_state(state, now);
        if let Some((may_retry, req)) = self.fresh.as_ref() {
            if let Some(r) = req.take() {
                let may_retry = *may_retry;
                self.fresh = None;
                // a failed read is no reason to interrupt anyone
                if let Ok(v) = r {
                    let payload = Payload::from_json(&v);
                    if let Some(o) = self.watcher.fresh_answer(Some(&payload), may_retry, now) {
                        self.offer = Some(o);
                    }
                }
            }
        }
        if let Some((device, req)) = self.boot.as_ref() {
            if let Some(r) = req.take() {
                let device = device.clone();
                self.boot = None;
                if let Ok(v) = r {
                    let flash = v.get("state").and_then(Value::as_str).unwrap_or("");
                    if let Some(o) = self.watcher.bootloader_answer(&device, flash) {
                        self.offer = Some(o);
                    }
                }
            }
        }
        if self.fresh.is_none() && self.boot.is_none() {
            match self.watcher.due(now, wizard_open, self.offer.is_some()) {
                Some(Job::Fresh { may_retry }) => {
                    let s = server.clone();
                    self.fresh = Some((
                        may_retry,
                        Pending::spawn(ctx, move || http::get_json(&s, "/api/first-setup")),
                    ));
                }
                Some(Job::Bootloader(device)) => {
                    let s = server.clone();
                    self.boot = Some((
                        device,
                        Pending::spawn(ctx, move || http::get_json(&s, "/api/firmware/status")),
                    ));
                }
                None => {}
            }
        }
        // the next timer is looked at again soon
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    /// `offer`: the dialog.
    fn dialog(&mut self, ctx: &egui::Context) -> Option<String> {
        let offer = self.offer.clone()?;
        let mut hit: Option<Choice> = None;
        let closed = modal::show(ctx, "fsoffer", modal::Size::Md, offer.title, |ui, x, y, w| {
            let mut cy = y;
            let lg = kit::wrapped_weight(ui, &offer.lede, 13.0, Weight::W400, style::text_1(), w, 1.21);
            let lh = lg.size().y;
            ui.painter().galley(Pos2::new(x, cy), lg, style::text_1());
            cy += lh + 8.0;
            for reason in &offer.reasons {
                let g = kit::wrapped_weight(ui, reason, 12.0, Weight::W400, style::text_2(), w - 18.0, 1.21);
                let h = g.size().y;
                ui.painter()
                    .circle_filled(Pos2::new(x + 9.0, cy + 12.0 * 1.21 / 2.0), 2.0, style::text_2());
                ui.painter().galley(Pos2::new(x + 18.0, cy), g, style::text_2());
                cy += h;
            }
            cy += 10.0;
            let ng = kit::wrapped_weight(ui, offer.note, 12.0, Weight::W400, style::text_2(), w, 1.5);
            let nh = ng.size().y;
            ui.painter().galley(Pos2::new(x, cy), ng, style::text_2());
            cy += nh + 18.0;
            let mut kinds = vec![Choice::NotNow];
            let mut buttons = vec![Button::new("Not now", Variant::Ghost)];
            if !offer.fingerprint.is_empty() {
                kinds.push(Choice::Silence);
                buttons.push(Button::new("Do not ask again", Variant::Secondary));
            }
            kinds.push(Choice::Start);
            buttons.push(Button::new("Start setup", Variant::Primary).icon("wand-sparkles"));
            let (pressed, bottom) = modal::actions(ui, x, cy, w, buttons);
            hit = pressed.map(|i| kinds[i]);
            bottom
        });
        match hit {
            Some(Choice::Start) => {
                self.offer = None;
                Some(offer.step.to_string())
            }
            Some(Choice::Silence) => {
                self.watcher.quiet.silence(&offer.fingerprint);
                self.offer = None;
                None
            }
            Some(Choice::NotNow) => {
                self.offer = None;
                None
            }
            None => {
                if closed {
                    self.offer = None;
                }
                None
            }
        }
    }
}

/// The three answers the dialog takes.
#[derive(Clone, Copy)]
enum Choice {
    NotNow,
    Silence,
    Start,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A backend that answers `GET /api/first-setup` with what the test says.
    fn serve(body: &'static str) -> Server {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { return };
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        server
    }

    fn frame(p: &mut Prompt, ctx: &egui::Context, server: &Server, state: &Value, now: f64) {
        p.step(ctx, server, state, now, false);
    }

    #[test]
    fn a_fresh_controller_on_a_new_link_opens_the_offer_and_a_known_one_does_not() {
        let server = serve(
            r#"{"connected": true, "stack": "px4", "prompt": true, "reasons": ["No aircraft type is set."], "fingerprint": "px4|1|2|x", "facts": {}, "presets": []}"#,
        );
        let ctx = egui::Context::default();
        let mut p = Prompt::new(Quiet::default());
        let state = json!({"connected": true, "armed": false});
        frame(&mut p, &ctx, &server, &state, 0.0);
        assert!(!p.open(), "not before the parameters have settled");
        frame(&mut p, &ctx, &server, &state, 4000.0);
        let t = std::time::Instant::now();
        while !p.open() {
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "no offer");
            std::thread::sleep(std::time::Duration::from_millis(10));
            frame(&mut p, &ctx, &server, &state, 4100.0);
        }
        let offer = p.offer.clone().unwrap();
        assert_eq!(offer.title, "New controller detected");

        let server = serve(r#"{"connected": true, "prompt": false, "reasons": [], "fingerprint": "px4|1|2|y"}"#);
        let mut p = Prompt::new(Quiet::default());
        frame(&mut p, &ctx, &server, &state, 0.0);
        frame(&mut p, &ctx, &server, &state, 4000.0);
        std::thread::sleep(std::time::Duration::from_millis(300));
        frame(&mut p, &ctx, &server, &state, 4200.0);
        assert!(!p.open());
    }

    #[test]
    fn a_blank_board_opens_the_offer_on_the_firmware_step() {
        let server = serve(r#"{"state": "idle", "can_flash": false}"#);
        let ctx = egui::Context::default();
        let mut p = Prompt::new(Quiet::default());
        let state = json!({"connected": false, "link_bootloader": {"device": "/dev/ttyACM0"}});
        frame(&mut p, &ctx, &server, &state, 0.0);
        frame(&mut p, &ctx, &server, &state, 4500.0);
        let t = std::time::Instant::now();
        while !p.open() {
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "no offer");
            std::thread::sleep(std::time::Duration::from_millis(10));
            frame(&mut p, &ctx, &server, &state, 4600.0);
        }
        let o = p.offer.clone().unwrap();
        assert_eq!((o.title, o.step), ("Controller without firmware", "firmware"));
    }
}
