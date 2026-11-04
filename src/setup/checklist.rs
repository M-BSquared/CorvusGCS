//! The preflight checklists (python/src/js/checklist.js, checklist-editor.js, the
//! card of setup-safety.js).
//!
//! What a list says lives in the backend config under `checklists`
//! (`corvus/checklists.py` bounds it); this module reads it, writes it back a
//! patch at a time, and draws the Setup card and the dialog the lists are
//! written in. Nothing here is written to the vehicle, so none of it is held
//! back while armed.
//!
//! The window on the Home map is `checklist_window.rs`.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::style::{self, Weight};

/// One row of a list: an item to tick, or a section heading.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub text: String,
    pub heading: bool,
}

/// A checklist.
#[derive(Debug, Clone, PartialEq)]
pub struct List {
    pub id: String,
    pub name: String,
    pub items: Vec<Item>,
}

/// The `checklists` block of the config, with every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub enabled: bool,
    pub home_window: bool,
    pub active: String,
    /// `None` until the operator has saved lists of their own: the standard
    /// list is offered then. An empty list is a deliberate "no lists".
    pub lists: Option<Vec<List>>,
}

impl Default for Block {
    fn default() -> Block {
        normalize(&Value::Null)
    }
}

/// `normalize`.
pub fn normalize(v: &Value) -> Block {
    let lists = v.get("lists").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter(|l| l.is_object())
            .map(|l| List {
                id: l
                    .get("id")
                    .map_or(String::new(), |i| i.as_str().map_or(i.to_string(), String::from)),
                name: l.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                items: l
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|it| Item {
                                text: it.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                                heading: it.get("heading") == Some(&Value::Bool(true)),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    });
    Block {
        enabled: v.get("enabled") == Some(&Value::Bool(true)),
        home_window: v.get("home_window") != Some(&Value::Bool(false)),
        active: v.get("active").and_then(Value::as_str).unwrap_or("").to_string(),
        lists,
    }
}

/// `STANDARD`: the list a station has before its operator writes one.
pub fn standard() -> List {
    List {
        id: "standard".into(),
        name: "Standard preflight".into(),
        items: [
            "Propellers and battery secure",
            "Weather, site and airspace checked",
            "GPS fix and home position set",
            "Failsafe and return altitude checked",
            "Area clear of people",
        ]
        .iter()
        .map(|t| Item {
            text: (*t).into(),
            heading: false,
        })
        .collect(),
    }
}

/// `newId`: an id no list has yet.
pub fn new_id(lists: &[List]) -> String {
    let mut n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64 | 1);
    loop {
        n = n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let id = format!("cl{:08x}", (n >> 32) as u32);
        if !lists.iter().any(|l| l.id == id) {
            return id;
        }
    }
}

/// `withDraft`: the lists after saving `draft`: replaced in place when it is
/// there, appended when new. Empty items are dropped.
pub fn with_draft(all: &[List], draft: &List) -> Vec<List> {
    let clean = List {
        id: draft.id.clone(),
        name: draft.name.trim().to_string(),
        items: draft
            .items
            .iter()
            .map(|i| Item {
                text: i.text.trim().to_string(),
                heading: i.heading,
            })
            .filter(|i| !i.text.is_empty())
            .collect(),
    };
    let mut out: Vec<List> = all
        .iter()
        .map(|l| if l.id == clean.id { clean.clone() } else { l.clone() })
        .collect();
    if !out.iter().any(|l| l.id == clean.id) {
        out.push(clean);
    }
    out
}

/// `draftProblem`: why a draft cannot be saved yet, or "".
pub fn draft_problem(d: &List) -> &'static str {
    if d.name.trim().is_empty() {
        return "Give the checklist a name.";
    }
    if !d.items.iter().any(|i| !i.heading && !i.text.trim().is_empty()) {
        return "Add at least one item to check.";
    }
    ""
}

fn lists_json(lists: &[List]) -> Value {
    Value::Array(
        lists
            .iter()
            .map(|l| {
                json!({
                    "id": l.id, "name": l.name,
                    "items": l.items.iter().map(|i| json!({"text": i.text, "heading": i.heading})).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// What to put back when the backend refuses a change that was shown at once.
enum Undo {
    Enabled(bool),
    Home(bool),
    Active(String),
    /// A change made in the dialog: the dialog says what went wrong.
    Dialog,
}

type Status = (&'static str, String);

enum Dialog {
    Lists {
        notice: Status,
    },
    Edit {
        draft: List,
        is_new: bool,
        msg: Status,
        confirm: bool,
        focus: Option<usize>,
    },
}

/// The checklists as the station holds them, and the dialog to write them.
pub struct Checklist {
    server: Server,
    block: Block,
    loading: Option<Pending<Result<Value, String>>>,
    saving: Vec<(Pending<Result<Value, String>>, Undo)>,
    dialog: Option<Dialog>,
    errors: Vec<String>,
    /// The window on the Home map and what the station remembers of it.
    win: crate::home::checklist_window::WindowState,
}

impl Checklist {
    pub fn new(server: Server, ctx: &egui::Context) -> Checklist {
        let s = server.clone();
        Checklist {
            server,
            block: Block::default(),
            loading: Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/config"))),
            saving: Vec::new(),
            dialog: None,
            errors: Vec::new(),
            win: crate::home::checklist_window::WindowState::load_default(),
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    /// `lists`: the lists in force, the operator's or the standard one.
    pub fn lists(&self) -> Vec<List> {
        self.block.lists.clone().unwrap_or_else(|| vec![standard()])
    }

    /// `activeList`: the chosen list, else the first.
    pub fn active_list(&self) -> Option<List> {
        let all = self.lists();
        all.iter().find(|l| l.id == self.block.active).or(all.first()).cloned()
    }

    fn take_block(&mut self, v: &Value) {
        let cfg = v.get("config").unwrap_or(v);
        self.block = normalize(cfg.get("checklists").unwrap_or(&Value::Null));
        let lists = self.lists();
        self.win.prune(&lists);
    }

    fn persist(&mut self, ctx: &egui::Context, patch: Value, undo: Undo) {
        let s = self.server.clone();
        self.saving.push((
            Pending::spawn(ctx, move || {
                http::post_json(&s, "/api/config", &json!({ "checklists": patch }))
            }),
            undo,
        ));
    }

    fn take_answers(&mut self) {
        if let Some(r) = self.loading.as_ref().and_then(Pending::take) {
            self.loading = None;
            if let Ok(v) = r {
                if self.saving.is_empty() {
                    self.take_block(&v);
                }
            }
        }
        let mut i = 0;
        while i < self.saving.len() {
            let Some(r) = self.saving[i].0.take() else {
                i += 1;
                continue;
            };
            let (_, undo) = self.saving.remove(i);
            match r {
                Ok(v) if v.get("config").is_some() => {
                    self.take_block(&v);
                    if let Undo::Dialog = undo {
                        self.saved_in_dialog();
                    }
                }
                other => {
                    let msg = match other {
                        Err(e) if !e.is_empty() => e,
                        Ok(v) => v
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("The change could not be saved.")
                            .to_string(),
                        _ => "The change could not be saved.".to_string(),
                    };
                    match undo {
                        Undo::Enabled(prev) => self.block.enabled = prev,
                        Undo::Home(prev) => self.block.home_window = prev,
                        Undo::Active(prev) => self.block.active = prev,
                        Undo::Dialog => {}
                    }
                    self.failed_in_dialog(&msg);
                    self.errors.push(msg);
                }
            }
        }
    }

    fn saved_in_dialog(&mut self) {
        if let Some(Dialog::Edit { is_new, .. }) = &self.dialog {
            let notice = if *is_new {
                "Checklist added and chosen for the Home map."
            } else {
                "Checklist saved."
            };
            self.dialog = Some(Dialog::Lists {
                notice: ("ok", notice.into()),
            });
        }
    }

    fn failed_in_dialog(&mut self, msg: &str) {
        match &mut self.dialog {
            Some(Dialog::Edit { msg: m, .. }) => *m = ("err", msg.to_string()),
            Some(Dialog::Lists { notice }) => *notice = ("err", msg.to_string()),
            None => {}
        }
    }

    fn set_enabled(&mut self, ctx: &egui::Context, on: bool) {
        let prev = self.block.enabled;
        self.block.enabled = on;
        self.persist(ctx, json!({ "enabled": on }), Undo::Enabled(prev));
    }

    fn set_home(&mut self, ctx: &egui::Context, on: bool) {
        let prev = self.block.home_window;
        self.block.home_window = on;
        self.persist(ctx, json!({ "home_window": on }), Undo::Home(prev));
    }

    fn set_active(&mut self, ctx: &egui::Context, id: &str) {
        let prev = self.block.active.clone();
        self.block.active = id.to_string();
        self.persist(ctx, json!({ "active": id }), Undo::Active(prev));
    }

    /// Takes in the answers of the requests on their way.
    pub fn poll(&mut self) {
        self.take_answers();
    }

    /// The window on the Home map, when the feature and the window are on.
    /// `host` is the map's rectangle, `below` where the flight bar ends.
    pub fn window(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, host: Rect, below: f32) {
        self.take_answers();
        if !(self.block.enabled && self.block.home_window) {
            return;
        }
        let all = self.lists();
        let list = self.active_list();
        let view = crate::home::checklist_window::View {
            list: list.as_ref(),
            all: &all,
        };
        let act = crate::home::checklist_window::show(ui, &mut self.win, host, below, &view);
        if let (Some(key), Some(l)) = (act.toggle, &list) {
            self.win.toggle(&l.id, &key);
        }
        if act.reset {
            if let Some(l) = &list {
                self.win.reset(&l.id);
            }
        }
        if act.close {
            self.set_home(ctx, false);
        }
        if let Some(id) = act.pick {
            self.set_active(ctx, &id);
        }
    }

    /// `onTelemetry`: the warning to raise, when the vehicle has just been
    /// armed with the list unfinished.
    pub fn on_telemetry(&mut self, state: &Value) -> Option<String> {
        let list = self.active_list();
        self.win.arming_warning(state, self.block.enabled, list.as_ref())
    }

    /// The Setup card under its title, `x .. x + w` from `y`. Returns the
    /// bottom.
    pub fn card_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        self.take_answers();
        let painter = ui.painter().clone();
        let mut cy = y;
        let g = kit::wrapped_weight(
            ui,
            "Your own list in a window on the Home map, ticked off item by item before each \
             flight. Arming with items still open shows a warning and nothing more: the \
             aircraft is never held back.",
            10.5,
            Weight::W400,
            style::text_3(),
            w,
            1.4,
        );
        painter.galley(Pos2::new(x, cy), g.clone(), style::text_3());
        cy += g.size().y + 10.0;

        let rows = [
            (
                "Use a checklist",
                "Write as many lists as you like, in sections, and pick one in the window. Off by default.",
                self.block.enabled,
                true,
            ),
            (
                "Show on the Home map",
                "Drag the window by its bar and double-click the bar to send it back to its corner. Its \u{d7} turns this off.",
                self.block.home_window,
                self.block.enabled,
            ),
        ];
        for (i, (label, hint, on, enabled)) in rows.iter().enumerate() {
            if i > 0 {
                cy += 12.0;
                painter.hline(
                    egui::Rangef::new(x, x + w),
                    cy + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                cy += 1.0 + 12.0;
            }
            let lg = style::galley(ui, label, 12.0, Weight::W600, style::text_1(), 0.0);
            let row_h = lg.size().y.max(22.0);
            painter.galley(Pos2::new(x, cy + (row_h - lg.size().y) / 2.0), lg, style::text_1());
            if kit::switch(ui, Pos2::new(x + w - 38.0, cy), *on, *enabled, ("clswitch", i)) {
                if i == 0 {
                    self.set_enabled(ctx, !on);
                } else {
                    self.set_home(ctx, !on);
                }
            }
            cy += row_h + 8.0;
            let hg = kit::wrapped_weight(ui, hint, 10.5, Weight::W400, style::text_3(), w - 50.0, 1.4);
            painter.galley(Pos2::new(x, cy), hg.clone(), style::text_3());
            cy += hg.size().y;
        }
        cy += 10.0;
        painter.hline(
            egui::Rangef::new(x, x + w),
            cy + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        cy += 1.0 + 10.0;
        let all = self.lists();
        let summary = if all.is_empty() {
            "No checklists.".to_string()
        } else {
            format!(
                "{} {}{}",
                all.len(),
                if all.len() == 1 { "checklist" } else { "checklists" },
                self.active_list()
                    .map_or(".".to_string(), |a| format!(". On Home: {}.", a.name))
            )
        };
        let b = Button::new("Edit checklists", Variant::Secondary)
            .icon("list-checks")
            .small()
            .enabled(self.block.enabled);
        let (bw, bh) = (b.width(ui), b.height());
        let sg = style::galley(ui, &summary, 12.0, Weight::W400, style::text_2(), 0.0);
        painter.galley(Pos2::new(x, cy + (bh - sg.size().y) / 2.0), sg, style::text_2());
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(x + w - bw, cy), Vec2::new(bw, bh)),
            "cleditbtn",
        ) {
            self.dialog = Some(Dialog::Lists {
                notice: ("", String::new()),
            });
        }
        cy + bh
    }

    /// The dialog, when open: a dimmed scrim and a panel in the middle.
    pub fn dialog(&mut self, ctx: &egui::Context) {
        self.take_answers();
        if self.dialog.is_none() {
            return;
        }
        let screen = ctx.content_rect();
        let w = 560.0f32.min(screen.width() - 48.0);
        let hkey = egui::Id::unique("checklist-dialog-h");
        let last_h = ctx.data(|d| d.get_temp::<f32>(hkey)).unwrap_or(300.0);
        let top = ((screen.height() - last_h) / 2.0).max(24.0);
        let panel = Rect::from_min_size(Pos2::new(screen.center().x - w / 2.0, top), Vec2::new(w, last_h));
        let mut close = false;
        let mut height = last_h;
        egui::Area::new(egui::Id::unique("checklist-dialog"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                let scrim = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    0,
                    Color32::from_black_alpha(if style::is_dark() { 150 } else { 90 }),
                );
                let bg = ui.painter().add(egui::Shape::Noop);
                let (ix, iw) = (panel.left() + 20.0, w - 40.0);
                let mut cy = panel.top() + 20.0;
                let title = match &self.dialog {
                    Some(Dialog::Edit { is_new: true, .. }) => "New checklist",
                    Some(Dialog::Edit { .. }) => "Edit checklist",
                    _ => "Preflight checklists",
                };
                let tg = style::galley(ui, title, 15.0, Weight::W700, style::text_1(), 0.3);
                ui.painter().galley(Pos2::new(ix, cy), tg.clone(), style::text_1());
                let xr = Rect::from_min_size(
                    Pos2::new(ix + iw - 28.0, cy + tg.size().y / 2.0 - 14.0),
                    Vec2::splat(28.0),
                );
                if kit::icon_btn(ui, xr, "x", "Close", false, "clclose") {
                    close = true;
                }
                cy += tg.size().y.max(28.0) + 16.0 - (28.0 - tg.size().y).max(0.0) / 2.0;
                cy = self.dialog_body(ui, ctx, ix, cy, iw, screen.height());
                let rect = Rect::from_min_max(panel.min, Pos2::new(panel.right(), cy + 20.0));
                height = rect.height();
                ui.painter().set(
                    bg,
                    egui::Shape::Vec(vec![
                        egui::Shape::Rect(style::shadow_pop().as_shape(rect, 12)),
                        egui::Shape::rect_filled(rect, 12, style::glass(0.96)),
                        egui::Shape::rect_stroke(
                            rect,
                            12,
                            Stroke::new(1.0, style::tint(0.06)),
                            egui::StrokeKind::Inside,
                        ),
                    ]),
                );
                if scrim.clicked() && !rect.contains(scrim.interact_pointer_pos().unwrap_or(Pos2::ZERO)) {
                    close = true;
                }
            });
        ctx.data_mut(|d| d.insert_temp(hkey, height));
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if close {
            self.dialog = None;
        }
    }

    fn dialog_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, screen_h: f32) -> f32 {
        match self.dialog.take() {
            Some(Dialog::Lists { notice }) => {
                let (cy, next) = self.lists_view(ui, ctx, x, y, w, notice);
                if self.dialog.is_none() {
                    self.dialog = Some(next);
                }
                cy
            }
            Some(Dialog::Edit {
                draft,
                is_new,
                msg,
                confirm,
                focus,
            }) => {
                let (cy, next) = self.edit_view(ui, ctx, x, y, w, screen_h, draft, is_new, msg, confirm, focus);
                if self.dialog.is_none() {
                    self.dialog = next;
                }
                cy
            }
            None => y,
        }
    }

    fn lists_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        notice: Status,
    ) -> (f32, Dialog) {
        let painter = ui.painter().clone();
        let mut cy = y;
        if !notice.1.is_empty() {
            cy = message(ui, x, cy, w, &notice) + 12.0;
        }
        let all = self.lists();
        let active = self.active_list().map(|l| l.id);
        let mut open: Option<Dialog> = None;
        if all.is_empty() {
            cy = empty(
                ui,
                x,
                cy,
                w,
                "No checklists. Write a new one, or start from the standard list.",
            ) + 12.0;
        }
        for (i, l) in all.iter().enumerate() {
            if i > 0 {
                painter.hline(
                    egui::Rangef::new(x, x + w),
                    cy - 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                cy += 8.0;
            }
            let is_active = active.as_deref() == Some(l.id.as_str());
            let ng = style::galley(ui, &l.name, 13.0, Weight::W600, style::text_1(), 0.0);
            let count = l.items.iter().filter(|i| !i.heading).count();
            let sections = l.items.iter().filter(|i| i.heading).count();
            let detail = format!(
                "{count} {}{}",
                if count == 1 { "item" } else { "items" },
                if sections > 0 {
                    format!(", {sections} {}", if sections == 1 { "section" } else { "sections" })
                } else {
                    String::new()
                }
            );
            let dg = style::galley(ui, &detail, 11.0, Weight::W400, style::text_3(), 0.0);
            let row_h = (ng.size().y + 2.0 + dg.size().y).max(28.0);
            painter.galley(
                Pos2::new(x, cy + (row_h - ng.size().y - 2.0 - dg.size().y) / 2.0),
                ng.clone(),
                style::text_1(),
            );
            if is_active {
                let bg = style::galley(ui, "ON HOME", 9.0, Weight::W600, style::healthy(), 0.6);
                let br = Rect::from_min_size(
                    Pos2::new(
                        x + ng.size().x + 6.0,
                        cy + (row_h - ng.size().y - 2.0 - dg.size().y) / 2.0 + 2.0,
                    ),
                    Vec2::new(bg.size().x + 10.0, bg.size().y + 2.0),
                );
                painter.rect_stroke(br, 4, Stroke::new(1.0, style::healthy()), egui::StrokeKind::Inside);
                painter.galley(br.min + Vec2::new(5.0, 1.0), bg, style::healthy());
            }
            painter.galley(
                Pos2::new(
                    x,
                    cy + (row_h - ng.size().y - 2.0 - dg.size().y) / 2.0 + ng.size().y + 2.0,
                ),
                dg,
                style::text_3(),
            );
            let mut rx = x + w;
            let edit = Rect::from_min_size(Pos2::new(rx - 28.0, cy + (row_h - 28.0) / 2.0), Vec2::splat(28.0));
            if kit::icon_btn(ui, edit, "pencil", "Edit", false, ("cledit", &l.id)) {
                open = Some(Dialog::Edit {
                    draft: l.clone(),
                    is_new: false,
                    msg: ("", String::new()),
                    confirm: false,
                    focus: None,
                });
            }
            rx -= 28.0 + 10.0;
            if !is_active {
                let b = Button::new("Use on Home", Variant::Secondary).small();
                let (bw, bh) = (b.width(ui), b.height());
                if b.show(
                    ui,
                    Rect::from_min_size(Pos2::new(rx - bw, cy + (row_h - bh) / 2.0), Vec2::new(bw, bh)),
                    ("cluse", &l.id),
                ) {
                    self.set_active(ctx, &l.id);
                }
            }
            cy += row_h + 8.0;
        }
        if !all.is_empty() && self.block.lists.is_none() {
            cy = empty(
                ui,
                x,
                cy,
                w,
                "This is the standard list Corvus comes with. Edit it to make it yours.",
            ) + 12.0;
        }
        cy += if all.is_empty() { 0.0 } else { 4.0 };
        let b = Button::new("New checklist", Variant::Secondary).icon("plus");
        let (bw, bh) = (b.width(ui), b.height());
        if b.show(ui, Rect::from_min_size(Pos2::new(x, cy), Vec2::new(bw, bh)), "clnew") {
            open = Some(Dialog::Edit {
                draft: List {
                    id: new_id(&all),
                    name: String::new(),
                    items: vec![Item {
                        text: String::new(),
                        heading: false,
                    }],
                },
                is_new: true,
                msg: ("", String::new()),
                confirm: false,
                focus: Some(0),
            });
        }
        let s = Button::new("Add the standard list", Variant::Ghost)
            .icon("copy")
            .title("A general multicopter preflight list to start from and change");
        let sw = s.width(ui);
        if s.show(
            ui,
            Rect::from_min_size(Pos2::new(x + bw + 8.0, cy), Vec2::new(sw, bh)),
            "clstd",
        ) {
            let std = standard();
            open = Some(Dialog::Edit {
                draft: List {
                    id: new_id(&all),
                    name: std.name,
                    items: std.items,
                },
                is_new: true,
                msg: ("", String::new()),
                confirm: false,
                focus: None,
            });
        }
        cy += bh;
        match open {
            Some(d) => {
                self.dialog = Some(d);
                (
                    cy,
                    Dialog::Lists {
                        notice: ("", String::new()),
                    },
                )
            }
            None => (cy, Dialog::Lists { notice }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn edit_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        screen_h: f32,
        mut draft: List,
        is_new: bool,
        mut msg: Status,
        mut confirm: bool,
        mut focus: Option<usize>,
    ) -> (f32, Option<Dialog>) {
        let painter = ui.painter().clone();
        let mut cy = y;
        let cap = style::galley(ui, "NAME", 10.0, Weight::W600, style::text_3(), 0.7);
        painter.galley(Pos2::new(x, cy), cap.clone(), style::text_3());
        cy += cap.size().y + 5.0;
        kit::text_field(
            ui,
            Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, 28.52)),
            &mut draft.name,
            "Survey quad",
            false,
            "clname",
        );
        cy += 28.52 + 5.0;
        let hg = kit::wrapped_weight(
            ui,
            "What the Home window's bar says, and how you pick it there.",
            10.5,
            Weight::W400,
            style::text_3(),
            w,
            1.4,
        );
        painter.galley(Pos2::new(x, cy), hg.clone(), style::text_3());
        cy += hg.size().y + 12.0;
        let lg = style::galley(ui, "Items", 12.0, Weight::W600, style::text_2(), 0.0);
        painter.galley(Pos2::new(x, cy), lg.clone(), style::text_2());
        cy += lg.size().y + 12.0;

        // The rows scroll in a box of their own, as the dialog's list does.
        let row_h = 28.52f32;
        let total = draft.items.len() as f32 * (row_h + 4.0) - if draft.items.is_empty() { 0.0 } else { 4.0 };
        let max_h = 0.46 * screen_h;
        let box_h = total.min(max_h).max(if draft.items.is_empty() { 20.0 } else { 0.0 });
        let area = Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, box_h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let mut act: Option<(usize, &'static str)> = None;
        let mut enter_at: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("clitems")
            .max_height(max_h)
            .auto_shrink([false, true])
            .show(&mut child, |ui| {
                let origin = ui.max_rect().min;
                let n = draft.items.len();
                for i in 0..n {
                    let top = origin.y + i as f32 * (row_h + 4.0);
                    let heading = draft.items[i].heading;
                    let tid = ui.scope_id().with(("clitem", i));
                    let field_w = w - 4.0 - 4.0 * 28.0 - 8.0;
                    let r = Rect::from_min_size(Pos2::new(origin.x, top), Vec2::new(field_w, row_h));
                    let placeholder = if heading { "Section name" } else { "What to check" };
                    let resp = kit::text_field(ui, r, &mut draft.items[i].text, placeholder, false, ("clitem", i));
                    if focus == Some(i) {
                        ui.memory_mut(|m| m.request_focus(tid));
                        focus = None;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        enter_at = Some(i);
                    }
                    if resp.has_focus()
                        && draft.items[i].text.is_empty()
                        && n > 1
                        && ui.input(|i| i.key_pressed(egui::Key::Backspace))
                    {
                        act = Some((i, "backspace"));
                    }
                    let mut bx = origin.x + field_w + 4.0;
                    let mut btn = |name: &'static str,
                                   icon: &'static str,
                                   title: &str,
                                   active: bool,
                                   enabled: bool,
                                   ui: &mut egui::Ui| {
                        let r = Rect::from_min_size(Pos2::new(bx, top), Vec2::new(28.0, row_h));
                        bx += 28.0;
                        let r = Rect::from_center_size(r.center(), Vec2::splat(28.0));
                        if !enabled {
                            let _ = ui.interact(r, ui.scope_id().with((name, i)), egui::Sense::hover());
                            icons::paint(
                                ui,
                                icon,
                                Rect::from_center_size(r.center(), Vec2::splat(14.0)),
                                style::text_3().gamma_multiply(0.35),
                            );
                            return false;
                        }
                        kit::icon_btn(ui, r, icon, title, active, (name, i))
                    };
                    if btn(
                        "clhead",
                        "heading",
                        if heading {
                            "Section. Click to make it an item."
                        } else {
                            "Item. Click to make it a section."
                        },
                        heading,
                        true,
                        ui,
                    ) {
                        act = Some((i, "heading"));
                    }
                    if btn("clup", "arrow-up", "Move up", false, i > 0, ui) {
                        act = Some((i, "up"));
                    }
                    if btn("cldown", "arrow-down", "Move down", false, i + 1 < n, ui) {
                        act = Some((i, "down"));
                    }
                    if btn("clrm", "x", "Remove", false, true, ui) {
                        act = Some((i, "remove"));
                    }
                }
                if n == 0 {
                    let g = style::galley(ui, "No items yet.", 12.0, Weight::W400, style::text_3(), 0.0);
                    ui.painter().galley(origin + Vec2::new(4.0, 2.0), g, style::text_3());
                }
                ui.allocate_space(Vec2::new(w, total.max(20.0)));
            });
        cy += box_h + 12.0;
        if let Some(i) = enter_at {
            draft.items.insert(
                i + 1,
                Item {
                    text: String::new(),
                    heading: false,
                },
            );
            focus = Some(i + 1);
        }
        match act {
            Some((i, "heading")) => draft.items[i].heading = !draft.items[i].heading,
            Some((i, "up")) => draft.items.swap(i, i - 1),
            Some((i, "down")) => draft.items.swap(i, i + 1),
            Some((i, "remove")) => {
                draft.items.remove(i);
            }
            Some((i, "backspace")) => {
                draft.items.remove(i);
                focus = Some(i.saturating_sub(1));
            }
            _ => {}
        }
        let hg = kit::wrapped_weight(
            ui,
            "Enter adds the next item. A section groups the items under it, for example Before power on and After power on.",
            10.5,
            Weight::W400,
            style::text_3(),
            w,
            1.4,
        );
        painter.galley(Pos2::new(x, cy), hg.clone(), style::text_3());
        cy += hg.size().y + 12.0;
        let add = Button::new("Item", Variant::Secondary).icon("plus").small();
        let (aw, ah) = (add.width(ui), add.height());
        if add.show(
            ui,
            Rect::from_min_size(Pos2::new(x, cy), Vec2::new(aw, ah)),
            "cladditem",
        ) {
            draft.items.push(Item {
                text: String::new(),
                heading: false,
            });
            focus = Some(draft.items.len() - 1);
        }
        let sec = Button::new("Section", Variant::Ghost).icon("heading").small();
        let sw = sec.width(ui);
        if sec.show(
            ui,
            Rect::from_min_size(Pos2::new(x + aw + 8.0, cy), Vec2::new(sw, ah)),
            "claddhead",
        ) {
            draft.items.push(Item {
                text: String::new(),
                heading: true,
            });
            focus = Some(draft.items.len() - 1);
        }
        cy += ah + 12.0;
        if !msg.1.is_empty() {
            cy = message(ui, x, cy, w, &msg) + 12.0;
        }

        let problem = draft_problem(&draft);
        let busy = self.saving.iter().any(|(_, u)| matches!(u, Undo::Dialog));
        let save = Button::new("Save", Variant::Primary)
            .icon("check")
            .enabled(problem.is_empty() && !busy)
            .title(problem);
        let (bw, bh) = (save.width(ui), save.height());
        let mut next: Option<Dialog> = None;
        let mut bx = x;
        if save.show(ui, Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(bw, bh)), "clsave") {
            let lists = with_draft(&self.lists(), &draft);
            let mut patch = json!({ "lists": lists_json(&lists) });
            if is_new {
                patch["active"] = json!(draft.id);
            }
            self.persist(ctx, patch, Undo::Dialog);
            msg = ("pending", "Saving\u{2026}".into());
        }
        bx += bw + 8.0;
        if !is_new {
            let del = Button::new(if confirm { "Remove it" } else { "Delete" }, Variant::Danger)
                .icon("trash-2")
                .title("Remove this checklist")
                .enabled(!busy);
            let dw = del.width(ui);
            if del.show(
                ui,
                Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(dw, bh)),
                "cldelete",
            ) {
                if confirm {
                    let name = if draft.name.is_empty() {
                        "this checklist".to_string()
                    } else {
                        draft.name.clone()
                    };
                    let kept: Vec<List> = self.lists().into_iter().filter(|l| l.id != draft.id).collect();
                    let _ = name;
                    self.persist(ctx, json!({ "lists": lists_json(&kept) }), Undo::Dialog);
                    msg = ("pending", "Removing\u{2026}".into());
                    confirm = false;
                } else {
                    confirm = true;
                }
            }
            bx += dw + 8.0;
        }
        let cancel = Button::new("Cancel", Variant::Ghost);
        let cw = cancel.width(ui);
        if cancel.show(
            ui,
            Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(cw, bh)),
            "clcancel",
        ) {
            next = Some(Dialog::Lists {
                notice: ("", String::new()),
            });
        }
        cy += bh;
        if next.is_none() {
            next = Some(Dialog::Edit {
                draft,
                is_new,
                msg,
                confirm,
                focus,
            });
        }
        (cy, next)
    }
}

/// `ui.message`: a one line notice in the colour of its class.
fn message(ui: &egui::Ui, x: f32, y: f32, w: f32, m: &Status) -> f32 {
    let c = match m.0 {
        "ok" => style::healthy(),
        "err" => style::critical(),
        _ => style::nav(),
    };
    let g = kit::wrapped_weight(ui, &m.1, 12.0, Weight::W400, c, w, 1.4);
    ui.painter().galley(Pos2::new(x, y), g.clone(), c);
    y + g.size().y
}

/// `ui.empty`: a muted line where there is nothing to list.
fn empty(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_3(), w, 1.45);
    ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_3());
    y + g.size().y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(id: &str, items: &[(&str, bool)]) -> List {
        List {
            id: id.into(),
            name: format!("List {id}"),
            items: items
                .iter()
                .map(|(t, h)| Item {
                    text: (*t).into(),
                    heading: *h,
                })
                .collect(),
        }
    }

    #[test]
    fn the_block_has_its_defaults_when_the_config_says_nothing() {
        let b = normalize(&Value::Null);
        assert_eq!(
            (b.enabled, b.home_window, b.active.as_str(), b.lists),
            (false, true, "", None)
        );
        let b = normalize(&json!({"enabled": true, "home_window": false, "active": "a",
            "lists": [{"id": "a", "name": "A", "items": [{"text": "x", "heading": true}, {"text": "y"}]}, 5]}));
        assert!(b.enabled && !b.home_window);
        let lists = b.lists.unwrap();
        assert_eq!(lists.len(), 1);
        assert_eq!(
            lists[0].items,
            vec![
                Item {
                    text: "x".into(),
                    heading: true
                },
                Item {
                    text: "y".into(),
                    heading: false
                }
            ]
        );
        assert!(!normalize(&json!({"enabled": "true"})).enabled);
        assert_eq!(normalize(&json!({"lists": []})).lists, Some(vec![]));
    }

    #[test]
    fn a_station_without_lists_of_its_own_has_the_standard_one() {
        let ctx = egui::Context::default();
        let c = Checklist {
            server: Server::local(1),
            block: Block::default(),
            loading: None,
            saving: vec![],
            dialog: None,
            errors: vec![],
            win: crate::home::checklist_window::WindowState::default(),
        };
        let _ = ctx;
        assert_eq!(c.lists(), vec![standard()]);
        assert_eq!(c.active_list().unwrap().id, "standard");
        let mut c = c;
        c.block.lists = Some(vec![]);
        assert!(c.lists().is_empty() && c.active_list().is_none());
        c.block.lists = Some(vec![list("a", &[("x", false)]), list("b", &[("y", false)])]);
        c.block.active = "b".into();
        assert_eq!(c.active_list().unwrap().id, "b");
        c.block.active = "gone".into();
        assert_eq!(c.active_list().unwrap().id, "a");
    }

    #[test]
    fn a_saved_draft_replaces_its_list_or_joins_the_others_and_loses_empty_items() {
        let all = vec![list("a", &[("x", false)]), list("b", &[("y", false)])];
        let draft = List {
            id: "b".into(),
            name: "  New name ".into(),
            items: vec![
                Item {
                    text: " one ".into(),
                    heading: false,
                },
                Item {
                    text: "  ".into(),
                    heading: false,
                },
                Item {
                    text: "Section".into(),
                    heading: true,
                },
            ],
        };
        let out = with_draft(&all, &draft);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].name, "New name");
        assert_eq!(out[1].items.len(), 2);
        assert_eq!(out[1].items[0].text, "one");
        let fresh = List {
            id: "c".into(),
            ..draft
        };
        assert_eq!(with_draft(&all, &fresh).len(), 3);
    }

    #[test]
    fn a_draft_needs_a_name_and_one_item_to_tick() {
        let mut d = list("a", &[("Section", true)]);
        d.name = " ".into();
        assert_eq!(draft_problem(&d), "Give the checklist a name.");
        d.name = "Quad".into();
        assert_eq!(draft_problem(&d), "Add at least one item to check.");
        d.items.push(Item {
            text: "Props".into(),
            heading: false,
        });
        assert_eq!(draft_problem(&d), "");
    }

    #[test]
    fn a_new_id_is_never_one_a_list_has() {
        let mut lists = Vec::new();
        for _ in 0..50 {
            let id = new_id(&lists);
            assert!(!lists.iter().any(|l: &List| l.id == id));
            assert!(id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric()));
            lists.push(list(&id, &[]));
        }
    }
}
