//! The first start setup (`python/src/js/welcome.js`, `.welcome` of
//! `main.css`). It opens once, on a station that had no config file when
//! Corvus started (`GET /api/welcome`). Its first page has three ways out:
//! set the station up in four short steps (look, units and flight review,
//! workspace, controls), take the settings from a file exported on another
//! station, or skip straight to the map with the defaults.
//!
//! Every choice previews live, and nothing is written until Finish: the
//! config is posted once, then `POST /api/welcome` marks the setup done.
//! Skip puts back what was showing and marks it done as well.

pub mod model;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::settings::model as smodel;
use crate::ui::form::{self, Form, Switch};
use crate::ui::icons;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::optioncards::Card;
use crate::ui::style::{self, Weight};
use model::{Choices, STEPS};

/// What the window must do after a frame of the dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    None,
    /// Show the screen as this config says (a choice was made).
    Preview(Value),
    /// Skip: put the screen back to this config.
    Restore(Value),
    /// Finished: the config now in force, to show and to announce.
    Done(Value),
    /// Import settings from another station: the Settings page takes over.
    Import,
}

struct Session {
    server: Server,
    base: Value,
    initial: Choices,
    choices: Choices,
    /// -1 is the start page.
    step: i32,
    languages: Vec<Choice>,
    lang_load: Option<Pending<Result<Value, String>>>,
    message: Option<String>,
    saving: Option<Pending<Result<(), String>>>,
    height: f32,
}

/// The dialog and the check that decides whether it opens.
#[derive(Default)]
pub struct Welcome {
    check: Option<Pending<Option<Value>>>,
    session: Option<Session>,
}

/// Whether the backend says this station is new, and the config it holds.
fn ask(server: &Server) -> Option<Value> {
    let state = http::get_json(server, "/api/welcome").ok()?;
    if state.get("pending") != Some(&Value::Bool(true)) {
        return None;
    }
    Some(
        http::get_json(server, "/api/config")
            .map(|c| crate::app::unwrap_config(&c))
            .unwrap_or(Value::Null),
    )
}

fn mark_done(server: &Server) -> Result<(), String> {
    http::post_json(server, "/api/welcome", &json!({})).map(|_| ())
}

fn refused(answer: &Result<Value, String>, fallback: &str) -> Option<String> {
    match answer {
        Ok(d) if d.get("ok") == Some(&Value::Bool(false)) || d.get("error").is_some() => Some(
            d.get("error")
                .and_then(Value::as_str)
                .filter(|e| !e.is_empty())
                .unwrap_or(fallback)
                .to_string(),
        ),
        Ok(_) => None,
        Err(e) if e.is_empty() => Some(fallback.to_string()),
        Err(e) => Some(e.clone()),
    }
}

impl Welcome {
    /// Asks the backend, on a thread, whether the setup is due.
    pub fn start(server: &Server, ctx: &egui::Context) -> Welcome {
        let s = server.clone();
        Welcome {
            check: Some(Pending::spawn(ctx, move || ask(&s))),
            session: None,
        }
    }

    /// Opens the dialog at once, whatever the backend says (`--page welcome`).
    pub fn open_now(&mut self, server: &Server, ctx: &egui::Context, cfg: Value) {
        self.session = Some(Session::new(server, ctx, cfg));
    }

    /// Whether the dialog is up or about to be: the update notice waits for
    /// it, so it never lands on top of the setup.
    pub fn is_open(&self) -> bool {
        self.session.is_some() || self.check.is_some()
    }

    /// Draws the dialog when it is open; returns what the window must do.
    pub fn show(&mut self, ctx: &egui::Context, server: &Server, logo: &egui::ImageSource<'static>) -> Event {
        if let Some(cfg) = self.check.as_ref().and_then(Pending::take) {
            self.check = None;
            if let Some(cfg) = cfg {
                self.session = Some(Session::new(server, ctx, cfg));
            }
        }
        let Some(s) = self.session.as_mut() else {
            return Event::None;
        };
        let event = s.show(ctx, server, logo);
        if matches!(event, Event::Done(_) | Event::Restore(_) | Event::Import) {
            self.session = None;
        }
        event
    }
}

impl Session {
    fn new(server: &Server, ctx: &egui::Context, base: Value) -> Session {
        let initial = model::choices_from(&base);
        let s = server.clone();
        Session {
            server: server.clone(),
            base,
            choices: initial.clone(),
            initial,
            step: -1,
            languages: vec![Choice::new("en", "English")],
            lang_load: Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/languages"))),
            message: None,
            saving: None,
            height: 300.0,
        }
    }

    fn poll(&mut self) {
        if let Some(Ok(v)) = self.lang_load.as_ref().and_then(Pending::take) {
            self.lang_load = None;
            let list: Vec<Choice> = v
                .get("languages")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .map(|l| {
                            let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                            let (name, native) = (s("name"), s("native_name"));
                            let label = if name != native {
                                format!("{native} ({name})")
                            } else {
                                native
                            };
                            Choice::new(s("code"), label)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !list.is_empty() {
                self.languages = list;
            }
        }
    }

    fn preview(&self) -> Event {
        Event::Preview(model::preview(&self.base, &self.choices))
    }

    fn show(&mut self, ctx: &egui::Context, server: &Server, logo: &egui::ImageSource<'static>) -> Event {
        self.poll();
        let mut event = Event::None;
        if let Some(r) = self.saving.as_ref().and_then(Pending::take) {
            self.saving = None;
            match r {
                Ok(()) => {
                    let mut cfg = self.base.clone();
                    smodel::merge(&mut cfg, &model::config_patch(&self.choices));
                    return Event::Done(cfg);
                }
                Err(e) => {
                    self.message = Some(if e.is_empty() {
                        "The settings could not be saved.".to_string()
                    } else {
                        e
                    });
                }
            }
        }
        let screen = ctx.content_rect();
        let width = 560.0f32.min(screen.width() - 32.0);
        let top = ((screen.height() - self.height) / 2.0).max(16.0);
        let panel = Rect::from_min_size(
            Pos2::new(screen.center().x - width / 2.0, top),
            Vec2::new(width, self.height),
        );
        let mut height = self.height;
        egui::Area::new(egui::Id::NULL.with("welcome"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    0,
                    Color32::from_black_alpha(if style::is_dark() { 150 } else { 90 }),
                );
                let bg = ui.painter().add(egui::Shape::Noop);
                let (ix, iw) = (panel.left() + 26.0, width - 52.0);
                let y = panel.top() + 26.0;
                let bottom = if self.step < 0 {
                    self.start_page(ui, ix, y, iw, logo, &mut event)
                } else {
                    self.step_page(ui, server, ix, y, iw, &mut event)
                };
                let rect = Rect::from_min_max(panel.min, Pos2::new(panel.right(), bottom + 26.0));
                height = rect.height();
                ui.painter().set(
                    bg,
                    egui::Shape::Vec(vec![
                        egui::Shape::Rect(style::shadow_pop().as_shape(rect, 18)),
                        egui::Shape::rect_filled(rect, 18, style::glass(0.96)),
                        egui::Shape::rect_stroke(
                            rect,
                            18,
                            Stroke::new(1.0, style::tint(0.06)),
                            egui::StrokeKind::Inside,
                        ),
                    ]),
                );
            });
        self.height = height;
        event
    }

    /// The first page: the mark, a heading, and three ways out.
    fn start_page(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        logo: &egui::ImageSource<'static>,
        event: &mut Event,
    ) -> f32 {
        let painter = ui.painter().clone();
        let mut y = y + 6.0;
        let mark = Rect::from_center_size(Pos2::new(x + w / 2.0, y + 26.0), Vec2::splat(52.0));
        egui::Image::new(logo.clone()).paint_at(ui, mark);
        y += 52.0 + 14.0;
        let h = style::galley(ui, "Welcome to Corvus GCS", 24.0, Weight::W700, style::text_1(), -0.4);
        painter.galley(Pos2::new(x + w / 2.0 - h.size().x / 2.0, y), h.clone(), style::text_1());
        y += h.size().y * 1.0 + 8.0;
        let lede = kit::wrapped_weight(
            ui,
            "This station has not been set up yet. Make it yours in about a minute, or bring the settings over from another station.",
            13.0,
            Weight::W400,
            style::text_2(),
            w.min(400.0) - 24.0,
            1.5,
        );
        painter.galley(
            Pos2::new(x + w / 2.0 - lede.size().x / 2.0, y),
            lede.clone(),
            style::text_2(),
        );
        y += lede.size().y + 14.0 + 12.0;

        // The setup card.
        let card = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 88.0));
        let resp = ui.interact(card, ui.scope_id().with("welcome-setup"), egui::Sense::click());
        paint_card(&painter, card, resp.hovered());
        let chip = Rect::from_center_size(Pos2::new(card.left() + 18.0 + 24.0, card.center().y), Vec2::splat(48.0));
        painter.rect_filled(chip, 13, style::accent());
        icons::paint(
            ui,
            "sliders-horizontal",
            Rect::from_center_size(chip.center(), Vec2::splat(22.0)),
            style::text_inverted(),
        );
        let go = Rect::from_center_size(
            Pos2::new(card.right() - 20.0 - 17.0, card.center().y),
            Vec2::splat(34.0),
        );
        let go_on = resp.hovered();
        painter.circle_filled(
            go.center(),
            17.0,
            if go_on { style::accent() } else { style::tint(0.06) },
        );
        icons::paint(
            ui,
            "arrow-right",
            Rect::from_center_size(
                go.center() + Vec2::new(if go_on { 2.0 } else { 0.0 }, 0.0),
                Vec2::splat(16.0),
            ),
            if go_on { style::text_inverted() } else { style::text_1() },
        );
        let tx = chip.right() + 16.0;
        let t = style::galley(ui, "Set up this station", 16.0, Weight::W600, style::text_1(), 0.0);
        let d = kit::wrapped(
            ui,
            "Colour theme, units, flight review, workspace and controls.",
            12.0,
            style::text_2(),
            go.left() - 16.0 - tx,
        );
        let block = t.size().y + 4.0 + d.size().y;
        let ty = card.center().y - block / 2.0;
        painter.galley(Pos2::new(tx, ty), t.clone(), style::text_1());
        painter.galley(Pos2::new(tx, ty + t.size().y + 4.0), d, style::text_2());
        if resp.clicked() {
            self.step = 0;
        }
        y = card.bottom() + 12.0;

        // The two lesser ways out, side by side.
        let half = (w - 12.0) / 2.0;
        let small = |ui: &mut egui::Ui, at: Pos2, icon: &'static str, title: &str, desc: &str| -> bool {
            let r = Rect::from_min_size(at, Vec2::new(half, 56.0));
            let resp = ui.interact(r, ui.scope_id().with(("welcome-more", title)), egui::Sense::click());
            let p = ui.painter().clone();
            paint_card(&p, r, resp.hovered());
            let c = Rect::from_center_size(Pos2::new(r.left() + 11.0 + 16.0, r.center().y), Vec2::splat(32.0));
            p.rect_filled(c, 9, style::tint(0.06));
            icons::paint(
                ui,
                icon,
                Rect::from_center_size(c.center(), Vec2::splat(16.0)),
                style::text_1(),
            );
            let chev = Rect::from_center_size(
                Pos2::new(
                    r.right() - 12.0 - 8.0 + if resp.hovered() { 2.0 } else { 0.0 },
                    r.center().y,
                ),
                Vec2::splat(16.0),
            );
            icons::paint(ui, "chevron-right", chev, style::text_3());
            let tx = c.right() + 12.0;
            let t = style::galley(ui, title, 13.0, Weight::W600, style::text_1(), 0.0);
            let d = style::galley(ui, desc, 11.5, Weight::W400, style::text_3(), 0.0);
            let block = t.size().y + 1.0 + d.size().y;
            let ty = r.center().y - block / 2.0;
            let clip = Rect::from_min_max(Pos2::new(tx, r.top()), Pos2::new(chev.left() - 4.0, r.bottom()));
            p.painter_at_clip(clip, |p| {
                p.galley(Pos2::new(tx, ty), t.clone(), style::text_1());
                p.galley(Pos2::new(tx, ty + t.size().y + 1.0), d.clone(), style::text_3());
            });
            resp.clicked()
        };
        if small(
            ui,
            Pos2::new(x, y),
            "upload",
            "Import settings",
            "From another station.",
        ) {
            *event = Event::Import;
        }
        if small(
            ui,
            Pos2::new(x + half + 12.0, y),
            "skip-forward",
            "Skip for now",
            "Start with the defaults.",
        ) {
            *event = self.skip(ui.ctx());
        }
        y + 56.0
    }

    /// Skip: the language is written because the dialog was shown in it, then
    /// the setup is marked done (best effort: when the backend cannot take it
    /// the setup simply offers itself again on the next start); the screen
    /// goes back to what it was.
    fn skip(&mut self, ctx: &egui::Context) -> Event {
        let (server, language) = (self.server.clone(), self.initial.language.clone());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if language != "en" {
                let _ = http::post_json(&server, "/api/config", &json!({"language": language}));
            }
            let _ = mark_done(&server);
            ctx.request_repaint();
        });
        Event::Restore(self.base.clone())
    }

    /// A page of the four steps: the numbered pills, the page, the message and
    /// Back and Next.
    fn step_page(&mut self, ui: &mut egui::Ui, server: &Server, x: f32, y: f32, w: f32, event: &mut Event) -> f32 {
        let painter = ui.painter().clone();
        let title = style::galley(ui, "Set up Corvus GCS", 15.0, Weight::W700, style::text_1(), 0.3);
        painter.galley(Pos2::new(x, y), title.clone(), style::text_1());
        let mut y = y + title.size().y.max(28.0) + 16.0;

        // The pills.
        let n = STEPS.len() as f32;
        let pw = (w - 6.0 * (n - 1.0)) / n;
        for (i, (_, label)) in STEPS.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(x + i as f32 * (pw + 6.0), y), Vec2::new(pw, 32.0));
            let active = i as i32 == self.step;
            let done = (i as i32) < self.step;
            painter.rect_stroke(
                r,
                255,
                Stroke::new(1.0, if active { style::accent() } else { style::border_soft() }),
                egui::StrokeKind::Inside,
            );
            let c = Pos2::new(r.left() + 10.0 + 9.0, r.center().y);
            painter.circle_filled(
                c,
                9.0,
                if active {
                    style::accent()
                } else if done {
                    style::accent_dim()
                } else {
                    style::tint(0.08)
                },
            );
            let num = style::galley(
                ui,
                &(i + 1).to_string(),
                10.0,
                Weight::W600,
                if active {
                    style::text_inverted()
                } else if done {
                    style::accent_bright()
                } else {
                    style::text_2()
                },
                0.0,
            );
            painter.galley(c - num.size() / 2.0, num.clone(), style::text_1());
            let color = if active {
                style::text_1()
            } else if done {
                style::text_2()
            } else {
                style::text_3()
            };
            let g = style::galley(ui, label, 11.5, Weight::W600, color, 0.0);
            painter.galley(Pos2::new(c.x + 9.0 + 8.0, r.center().y - g.size().y / 2.0), g, color);
        }
        y += 32.0 + 16.0;

        let mut f = Form::new(x, y, w);
        let changed = match self.step {
            0 => self.look(ui, &mut f),
            1 => self.units(ui, &mut f),
            2 => self.workspace(ui, &mut f),
            _ => self.controls(ui, &mut f),
        };
        if changed {
            *event = self.preview();
        }
        let mut y = f.y;
        if let Some(m) = &self.message {
            y += 8.0;
            let mut f = Form::new(x, y, w);
            form::message(ui, &mut f, m, Tone::Err);
            y = f.y;
        }
        y += 18.0;
        let last = self.step as usize == STEPS.len() - 1;
        let busy = self.saving.is_some();
        let (hit, bottom) = crate::ui::modal::actions(
            ui,
            x,
            y,
            w,
            vec![
                Button::new("Back", Variant::Secondary)
                    .icon("arrow-left")
                    .enabled(!busy),
                Button::new(if last { "Finish" } else { "Next" }, Variant::Primary).enabled(!busy),
            ],
        );
        match hit {
            Some(0) => {
                self.step -= 1;
                self.message = None;
            }
            Some(_) if !last => {
                self.step += 1;
                self.message = None;
            }
            Some(_) => {
                self.message = None;
                let s = server.clone();
                let patch = model::config_patch(&self.choices);
                self.saving = Some(Pending::spawn(ui.ctx(), move || {
                    let answer = http::post_json(&s, "/api/config", &patch);
                    if let Some(e) = refused(&answer, "The settings could not be saved.") {
                        return Err(e);
                    }
                    mark_done(&s)
                }));
            }
            None => {}
        }
        bottom
    }

    fn look(&mut self, ui: &mut egui::Ui, f: &mut Form) -> bool {
        let mut changed = false;
        if self.languages.len() > 1 {
            let current = self.choices.language.clone();
            let langs = self.languages.clone();
            let mut picked = None;
            form::field(ui, f, "Language", "", "", |ui, x, y, w| {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(240.0f32.min(w), form::CONTROL_H));
                picked = kit::select(ui, r, &langs, &current, "", false, "", "welcomelang");
                form::CONTROL_H
            });
            f.skip(14.0);
            if let Some(code) = picked {
                self.choices.language = code;
                changed = true;
            }
        }
        let opts: Vec<Card> = smodel::THEMES
            .iter()
            .map(|t| Card::new(t.0, t.1, t.2).swatch(t.3))
            .collect();
        let mut picked = None;
        let current = self.choices.theme.clone();
        form::field(
            ui,
            f,
            "Colour theme",
            "",
            "The interface size (text, icons and panels) can be adjusted later in Settings under Interface.",
            |ui, x, y, w| {
                let mut inner = Form::new(x, y, w);
                picked = form::option_cards(ui, &mut inner, "welcometheme", &opts, &current, 3);
                inner.y - y
            },
        );
        if let Some(id) = picked {
            self.choices.theme = id;
            changed = true;
        }
        changed
    }

    fn units(&mut self, ui: &mut egui::Ui, f: &mut Form) -> bool {
        let mut changed = false;
        let preset = smodel::system_of(&self.choices.units).unwrap_or("").to_string();
        let opts: Vec<Card> = smodel::UNIT_SYSTEMS.iter().map(|s| Card::new(s.0, s.1, s.2)).collect();
        let mut picked = None;
        form::field(
            ui,
            f,
            "Unit system",
            "",
            "Sets the four below at once. You can still change any one of them.",
            |ui, x, y, w| {
                let mut inner = Form::new(x, y, w);
                picked = form::option_cards(ui, &mut inner, "welcomeunitsys", &opts, &preset, 3);
                inner.y - y
            },
        );
        f.skip(14.0);
        if let Some(id) = picked {
            if let Some(s) = smodel::UNIT_SYSTEMS.iter().find(|s| s.0 == id) {
                self.choices.units = s.3.map(String::from);
                changed = true;
            }
        }
        // The four quantities, two columns.
        let col = (f.w - 14.0) / 2.0;
        let top = f.y;
        let mut bottom = top;
        for (i, (q, label, _)) in smodel::UNIT_ROWS.iter().enumerate() {
            let (cx, cy) = (
                f.x + (i % 2) as f32 * (col + 14.0),
                top + (i / 2) as f32 * (form::CONTROL_H + 24.0 + 12.0),
            );
            let mut sub = Form::new(cx, cy, col);
            let choices: Vec<Choice> = smodel::unit_choices(q)
                .iter()
                .map(|(v, l)| Choice::new(*v, *l))
                .collect();
            let current = self.choices.units[i].clone();
            let mut picked = None;
            form::field(ui, &mut sub, label, "", "", |ui, x, y, w| {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, form::CONTROL_H));
                picked = kit::select(ui, r, &choices, &current, "", false, "", ("welcomeunit", *q));
                form::CONTROL_H
            });
            if let Some(v) = picked {
                self.choices.units[i] = v;
                changed = true;
            }
            bottom = bottom.max(sub.y);
        }
        f.y = bottom + 8.0;
        form::note(
            ui,
            f,
            "Display only. Parameters, mission files and everything sent to the aircraft stay in the units the autopilot uses.",
        );
        f.skip(14.0);
        form::ruled(ui, f);
        let opts: Vec<Card> = smodel::REVIEW.iter().map(|r| Card::new(r.0, r.1, r.2)).collect();
        let current = self.choices.sensitivity.clone();
        let mut picked = None;
        form::field(
            ui,
            f,
            "Flight review sensitivity",
            "",
            "How readily a number in a flight log or a telemetry recording becomes a warning. It can be changed later in Settings under Analysis.",
            |ui, x, y, w| {
                let mut inner = Form::new(x, y, w);
                picked = form::option_cards_wrapped(ui, &mut inner, "welcomereview", &opts, &current, 3);
                inner.y - y
            },
        );
        if let Some(id) = picked {
            self.choices.sensitivity = id;
            changed = true;
        }
        f.skip(10.0);
        caution(
            ui,
            f,
            "Errors in the analysis cannot be ruled out. A finding can be wrong, and a review without findings does not prove the aircraft is fit to fly. Check the plots and the aircraft yourself.",
        );
        changed
    }

    fn workspace(&mut self, ui: &mut egui::Ui, f: &mut Form) -> bool {
        let mut changed = false;
        let rows: [(&str, &str, bool); 4] = [
            (
                "Mission planner",
                "Adds MISSION to the left rail: plan takeoff, waypoints and landing on a map, then upload. Leave it off for a station flown by hand.",
                self.choices.mission_page,
            ),
            (
                "MAVLink console tab",
                "The CONSOLE tab: the autopilot's messages and a command line. Off, the tab is gone. Messages are still collected.",
                self.choices.console_tab,
            ),
            (
                "SSH tab",
                "The SSH tab: your saved connections and a terminal. Off, the tab is gone. Connections stay saved and a terminal opens in a window of its own.",
                self.choices.ssh_tab,
            ),
            (
                "Check for updates",
                "At each start, asks GitHub whether a newer release exists. Nothing is downloaded and nothing about this machine is sent. Without internet it stays silent.",
                self.choices.updates,
            ),
        ];
        for (i, (label, hint, on)) in rows.iter().enumerate() {
            if let Some(v) = form::switch_field(
                ui,
                f,
                ("welcomeswitch", i),
                Switch::new(label, hint, *on),
                style::text_1(),
                12.0,
            ) {
                match i {
                    0 => self.choices.mission_page = v,
                    1 => self.choices.console_tab = v,
                    2 => self.choices.ssh_tab = v,
                    _ => self.choices.updates = v,
                }
                changed = true;
            }
        }
        f.skip(14.0);
        form::desc(
            ui,
            f,
            "Connections, the map service and everything on these pages are under Settings, whenever you need them.",
        );
        changed
    }

    fn controls(&mut self, ui: &mut egui::Ui, f: &mut Form) -> bool {
        let mut changed = false;
        let rows: [(&str, &str, bool); 4] = [
            (
                "Virtual joystick",
                "Two-stick pad over the map: left is throttle and yaw, right is pitch and roll. Drag it by the grip bar.",
                self.choices.virtual_joystick,
            ),
            (
                "USB gamepad",
                "A game controller plugged in by USB. Push both sticks inward and hold for one second to arm.",
                self.choices.gamepad,
            ),
            (
                "Arrow keys",
                "Flies pitch and roll from the arrow keys, on screen and on your own keyboard.",
                self.choices.arrow_keys,
            ),
            ("WASD keys", "W and S are thrust, A and D are yaw.", self.choices.wasd_keys),
        ];
        for (i, (label, hint, on)) in rows.iter().enumerate() {
            if let Some(v) = form::switch_field(
                ui,
                f,
                ("welcomecontrol", i),
                Switch::new(label, hint, *on),
                style::text_1(),
                12.0,
            ) {
                match i {
                    0 => self.choices.virtual_joystick = v,
                    1 => self.choices.gamepad = v,
                    2 => self.choices.arrow_keys = v,
                    _ => self.choices.wasd_keys = v,
                }
                changed = true;
            }
        }
        f.skip(14.0);
        caution(
            ui,
            f,
            "These controls reach the aircraft only with COM_RC_IN_MODE 1, 2 or 3 and a mode that flies from the sticks. All three are off by default. The key strength is under Settings, Controls.",
        );
        changed
    }
}

/// `.welcome-review-caution`: a plain sentence, one step louder than a hint.
fn caution(ui: &egui::Ui, f: &mut Form, text: &str) {
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W500, style::text_1(), f.w, 1.45);
    ui.painter().galley(Pos2::new(f.x, f.y), g.clone(), style::text_1());
    f.skip(g.size().y);
}

/// `.tile`: the glass card of the start page, lifted under the pointer.
fn paint_card(painter: &egui::Painter, r: Rect, hovered: bool) {
    let (fill, line) = if hovered {
        (style::glass(0.95), style::tint(0.15))
    } else {
        (style::glass(0.82), style::tint(0.09))
    };
    painter.rect_filled(r, style::RADIUS + 2, fill);
    painter.rect_stroke(r, style::RADIUS + 2, Stroke::new(1.0, line), egui::StrokeKind::Inside);
}

trait ClipPaint {
    fn painter_at_clip(&self, clip: Rect, f: impl FnOnce(&egui::Painter));
}

impl ClipPaint for egui::Painter {
    fn painter_at_clip(&self, clip: Rect, f: impl FnOnce(&egui::Painter)) {
        f(&self.with_clip_rect(clip.intersect(self.clip_rect())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_answer_is_the_backends_reason() {
        assert_eq!(refused(&Ok(json!({"ok": true})), "x"), None);
        assert_eq!(
            refused(&Ok(json!({"ok": false, "error": "no"})), "x"),
            Some("no".to_string())
        );
        assert_eq!(
            refused(&Ok(json!({"ok": false})), "fallback"),
            Some("fallback".to_string())
        );
        assert_eq!(refused(&Err(String::new()), "fallback"), Some("fallback".to_string()));
        assert_eq!(refused(&Err("down".into()), "fallback"), Some("down".to_string()));
    }
}
