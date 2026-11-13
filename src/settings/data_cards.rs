//! The Data pill: where Corvus writes parameter files, the parameter defaults
//! copy, how the flight review reads a log, and export, import and reset
//! (`renderFilesSection`, `renderAnalysisSection`, `paramDefaultsCacheCard` of
//! `python/src/js/sidenav.js`, `python/src/js/settings-transfer.js`).

use eframe::egui::{self};
use serde_json::{json, Value};

use super::{model, refused, Dialog, SettingsPage};
use crate::ui::form::{self, Form, Switch};
use crate::ui::kit::{Button, Variant};
use crate::ui::message::Tone;
use crate::ui::optioncards::Card;
use crate::ui::pathfield::{Mode, PathField, Picked};
use crate::ui::style;

/// The parts a settings file is chosen by (`SECTIONS` of settings-transfer.js).
pub(super) const PARTS: [(&str, &str, &str); 7] = [
    (
        "interface",
        "Interface and controls",
        "Language, theme, size, units, top bar, joystick and keys, RC transmitter, company logo, flight review sensitivity.",
    ),
    (
        "layout",
        "Window layout",
        "Where the flight HUD, the virtual joystick and the checklist sit, and which cards are folded.",
    ),
    ("map", "Map", "Map service, base layer, 3D mode and map keys."),
    (
        "connections",
        "Connections",
        "Default link, auto connect, forwarding to a second station, SSH connections, the MCP server's switches (never its token).",
    ),
    (
        "vehicle",
        "Vehicle",
        "Battery estimate, Remote ID, RTK base, cameras, extra safety parameters, preflight checklists.",
    ),
    (
        "folders",
        "Folders and port",
        "Where Corvus keeps tiles, logs, parameters, firmware and missions. Paths from another machine may not exist on this one.",
    ),
    (
        "plugins",
        "Plugins",
        "Each plugin's saved settings, and the plugins you installed yourself.",
    ),
];

const LABELS: [(&str, &str); 4] = [
    ("http_port", "HTTP port"),
    ("tile_cache_dir", "tile cache folder"),
    ("tlog_dir", "telemetry log folder"),
    ("firmware_dir", "firmware folder"),
];

const CACHE_WARNING: &str = "Use with care. A copy is matched to the vehicle by a checksum only, and in rare cases two different files share one: the defaults, descriptions and ranges shown would then belong to another firmware. If defaults ever look wrong, clear the copies below and switch this off.";

/// The sections a settings file carries, in the order the dialog lists them.
pub(super) fn sections_in(bundle: &Value) -> Vec<&'static str> {
    let present: Vec<&str> = bundle
        .get("sections")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    PARTS.iter().map(|p| p.0).filter(|id| present.contains(id)).collect()
}

/// `parseBundle`: the file's JSON, or the sentence an operator can act on.
pub(super) fn parse_bundle(text: &str) -> Result<Value, String> {
    let bad = || "This is not a Corvus GCS settings file.".to_string();
    let data: Value = serde_json::from_str(text).map_err(|_| bad())?;
    let ok = data.get("kind").and_then(Value::as_str) == Some("corvus-settings")
        && data.get("config").is_some_and(Value::is_object);
    ok.then_some(data).ok_or_else(bad)
}

/// "file, from Corvus GCS 2026.10.01, exported ..." (`describeSource`).
pub(super) fn describe_source(bundle: &Value, name: &str) -> String {
    let mut parts = Vec::new();
    if let Some(v) = bundle.get("version").and_then(Value::as_str).filter(|v| !v.is_empty()) {
        parts.push(format!("Corvus GCS {v}"));
    }
    if let Some(t) = bundle
        .get("exported_at")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        parts.push(format!("exported {t}"));
    }
    if parts.is_empty() {
        format!("{name}.")
    } else {
        format!("{name}, from {}.", parts.join(", "))
    }
}

/// The lines the import notice shows (`showResult`).
pub(super) fn import_lines(res: &Value) -> Vec<String> {
    let mut lines = vec!["Settings imported.".to_string()];
    let installed = res.get("plugins_installed").and_then(Value::as_u64).unwrap_or(0);
    if installed > 0 {
        lines.push(if installed == 1 {
            "1 plugin installed.".into()
        } else {
            format!("{installed} plugins installed.")
        });
    }
    let restart = restart_names(res);
    if !restart.is_empty() {
        lines.push(format!("Restart Corvus GCS for the new {}.", restart.join(", ")));
    }
    lines.extend(
        res.get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    lines
}

fn restart_names(res: &Value) -> Vec<String> {
    res.get("restart")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|k| LABELS.iter().find(|l| l.0 == k).map_or(k, |l| l.1).to_string())
                .collect()
        })
        .unwrap_or_default()
}

impl SettingsPage {
    pub(super) fn data_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "Parameters", |ui, f| {
            self.params_dir_card(ui, f);
            self.defaults_card(ui, f);
        });
        form::section(ui, f, "Analysis", |ui, f| {
            let cur = model::review_of(&self.cfg);
            let opts: Vec<Card> = model::REVIEW.iter().map(|r| Card::new(r.0, r.1, r.2)).collect();
            let mut picked = None;
            form::card(ui, f, "Flight review", |ui, f| {
                form::field(
                    ui,
                    f,
                    "Sensitivity",
                    "",
                    "How readily a number in a flight log or a telemetry recording becomes a warning. Only time the aircraft was armed is measured. The plots are the same at every setting; only the findings above them change.",
                    |ui, x, y, w| {
                        let mut inner = Form::new(x, y, w);
                        picked = form::option_cards(ui, &mut inner, "review", &opts, cur, 3);
                        inner.y - y
                    },
                );
            });
            if let Some(id) = picked {
                self.write(ui.ctx(), json!({ "review": { "sensitivity": id } }), true);
            }
        });
        form::section(ui, f, "Export and import", |ui, f| {
            let mut hit = None;
            form::card(ui, f, "", |ui, f| {
                form::desc(
                    ui,
                    f,
                    "Save the settings of this station to one file, or load one saved here or on another station. You choose the parts, down to each plugin, which can travel with its settings or on its own.",
                );
                f.skip(12.0);
                hit = form::actions(
                    ui,
                    f,
                    vec![
                        Button::new("Export settings", Variant::Secondary).icon("download"),
                        Button::new("Import settings", Variant::Secondary).icon("upload"),
                        Button::new("Reset to factory settings", Variant::Danger).icon("rotate-ccw"),
                    ],
                );
            });
            match hit {
                Some(0) => {
                    self.ticks.retain(|k, _| !k.starts_with("export:"));
                    if !self.texts.contains_key("export_name") {
                        *self.text("export_name") = String::new();
                    }
                    self.hush("transfer_dialog");
                    self.res.remove("export_target");
                    self.get(ui.ctx(), "export_target", "/api/settings/export/target");
                    self.texts.remove("export_name");
                    self.texts.remove("export_dir");
                    self.dialog = Some(Dialog::Export);
                }
                Some(1) => {
                    self.hush("transfer_dialog");
                    self.text("import_path").clear();
                    self.import_bundle = None;
                    self.dialog = Some(Dialog::Import);
                }
                Some(2) => {
                    self.hush("transfer_dialog");
                    self.dialog = Some(Dialog::Reset);
                }
                _ => {}
            }
        });
    }

    fn params_dir_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let saved = self
            .cfg
            .get("params_dir")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !self.texts.contains_key("params_dir") {
            self.texts.insert("params_dir", saved.clone());
        }
        let mut picked = None;
        form::card(ui, f, "", |ui, f| {
            let buf = self.texts.entry("params_dir").or_default();
            let field = PathField::new(Mode::Folder, "Parameter export folder")
                .placeholder("~/.corvus/params")
                .optional()
                .backend();
            picked = form::path(
                ui,
                f,
                "Parameter export folder",
                "",
                "Where Export writes parameter files, on the machine running Corvus. Leave empty for ~/.corvus/params. The export dialog can still override it per file.",
                &field,
                buf,
                "paramsdir",
            );
        });
        if let Some(p) = picked {
            let v = p.text().trim().to_string();
            if v != saved {
                self.write(ui.ctx(), json!({ "params_dir": v }), false);
            }
        }
    }

    fn defaults_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let on = model::flag(&self.cfg, "parameters", "cache_defaults", false);
        let data = self.want(ui.ctx(), "param_cache", "/api/params/metadata/cache");
        let failed = data.is_none() && self.failed("param_cache");
        let n = data
            .as_ref()
            .and_then(|d| d.get("files"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let busy = self.is_busy("cache_clear");
        let mut toggled = None;
        let mut clear = false;
        form::card(ui, f, "Parameter defaults", |ui, f| {
            toggled = form::switch(
                ui,
                f,
                "paramcache",
                "Keep a copy on this computer",
                "The parameter editor reads each parameter's default from the vehicle. On PX4 that file can take half a minute over a telemetry radio. With this on, Corvus keeps a copy per firmware and reuses it whenever the vehicle reports the same checksum for its file. ArduPilot always reads from the vehicle.",
                on,
            );
            f.skip(10.0);
            form::message(ui, f, CACHE_WARNING, Tone::Warn);
            f.skip(10.0);
            let line = if failed {
                "Copies unavailable.".to_string()
            } else {
                data.as_ref().map_or(String::new(), model::cache_line)
            };
            let b = Button::new("Clear copies", Variant::Secondary)
                .icon("trash-2")
                .small()
                .enabled(n > 0.0 && !busy);
            let (bw, bh) = (b.width(ui), b.height());
            let g = style::galley(ui, &line, 12.0, style::Weight::W400, style::text_2(), 0.0);
            let y = f.y;
            ui.painter().galley(
                eframe::egui::Pos2::new(f.x, y + (bh - g.size().y) / 2.0),
                g,
                style::text_2(),
            );
            clear = b.show(
                ui,
                eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::new(f.x + f.w - bw, y),
                    eframe::egui::Vec2::new(bw, bh),
                ),
                "clearcopies",
            );
            f.y = y + bh;
        });
        if let Some(v) = toggled {
            self.write(ui.ctx(), json!({ "parameters": { "cache_defaults": v } }), true);
        }
        if clear {
            self.act(
                ui.ctx(),
                "cache_clear",
                "",
                "/api/params/metadata/cache/clear",
                json!({}),
            );
        }
    }

    pub(super) fn cache_done(&mut self, _ctx: &egui::Context, answer: Result<Value, String>) {
        if let Ok(v) = answer {
            let slot = self.res.entry("param_cache").or_default();
            slot.value = Some(v);
        }
    }

    // ---- export, import, reset ----

    pub(super) fn transfer_dialogs(&mut self, ctx: &egui::Context) -> bool {
        let msg = self.msgs.get("transfer_dialog").cloned();
        let busy = self.is_busy("export") || self.is_busy("import") || self.is_busy("reset");
        match self.dialog.as_ref() {
            Some(Dialog::Export) => {
                let target = self
                    .res
                    .get("export_target")
                    .and_then(|s| s.value.clone())
                    .unwrap_or(Value::Null);
                let mut name = self
                    .texts
                    .remove("export_name")
                    .unwrap_or_else(|| target.get("filename").and_then(Value::as_str).unwrap_or("").to_string());
                let mut dir = self
                    .texts
                    .remove("export_dir")
                    .unwrap_or_else(|| target.get("dir").and_then(Value::as_str).unwrap_or("").to_string());
                let mut parts: Vec<(&'static str, bool)> = PARTS
                    .iter()
                    .map(|p| (p.0, *self.ticks.get(&format!("export:{}", p.0)).unwrap_or(&true)))
                    .collect();
                let mut secrets = *self.ticks.get("export:secrets").unwrap_or(&false);
                let (mut go, mut close) = (false, false);
                let closed = crate::ui::modal::show(
                    ctx,
                    "export",
                    crate::ui::modal::Size::Lg,
                    "Export settings",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::desc(
                        ui,
                        &mut f,
                        "Choose what goes into the file. Everything is switched on, which is the whole station: a backup, or the setup for the next laptop.",
                    );
                        f.skip(12.0);
                        section_switches(ui, &mut f, &mut parts, "What goes into the file", &[]);
                        f.skip(14.0);
                        form::input(
                            ui,
                            &mut f,
                            "File name",
                            "",
                            "",
                            &mut name,
                            "corvus-settings.json",
                            true,
                            false,
                            "exportname",
                        );
                        f.skip(14.0);
                        form::path(
                            ui,
                            &mut f,
                            "Folder",
                            "",
                            "On the machine running Corvus. Created if it does not exist.",
                            &PathField::new(Mode::Folder, "Export folder")
                                .placeholder("~/Downloads")
                                .optional()
                                .backend(),
                            &mut dir,
                            "exportdir",
                        );
                        f.skip(14.0);
                        if let Some(v) = form::switch_field(
                        ui,
                        &mut f,
                        "exportsecrets",
                        Switch::new(
                            "Include passwords and map keys",
                            "SSH and camera passwords, the NTRIP password and map service keys. Leave off for a file you pass on.",
                            secrets,
                        ),
                        style::text_1(),
                        12.0,
                    ) {
                        secrets = v;
                    }
                        if let Some((m, tone)) = &msg {
                            f.skip(12.0);
                            form::message(ui, &mut f, m, *tone);
                        }
                        f.skip(18.0);
                        let any = parts.iter().any(|p| p.1);
                        let (hit, bottom) = crate::ui::modal::actions(
                            ui,
                            f.x,
                            f.y,
                            f.w,
                            vec![
                                Button::new("Cancel", Variant::Secondary),
                                Button::new("Save", Variant::Primary)
                                    .icon("download")
                                    .enabled(any && !busy),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    },
                );
                for (id, on) in &parts {
                    self.ticks.insert(format!("export:{id}"), *on);
                }
                self.ticks.insert("export:secrets".into(), secrets);
                self.texts.insert("export_name", name.clone());
                self.texts.insert("export_dir", dir.clone());
                if go {
                    self.hush("transfer_dialog");
                    let sections: Vec<&str> = parts.iter().filter(|p| p.1).map(|p| p.0).collect();
                    self.act(
                        ctx,
                        "export",
                        "",
                        "/api/settings/export",
                        json!({
                            "filename": name.trim(), "dir": dir.trim(), "include_secrets": secrets,
                            "browser": {}, "sections": sections,
                        }),
                    );
                }
                closed || close
            }
            Some(Dialog::Import) => {
                let bundle = self.import_bundle.clone();
                let mut path = self.texts.remove("import_path").unwrap_or_default();
                let present = bundle.as_ref().map(sections_in).unwrap_or_default();
                let mut parts: Vec<(&'static str, bool)> = PARTS
                    .iter()
                    .map(|p| {
                        (
                            p.0,
                            present.contains(&p.0) && *self.ticks.get(&format!("import:{}", p.0)).unwrap_or(&true),
                        )
                    })
                    .collect();
                let (mut go, mut close, mut read) = (false, false, false);
                let file_name = self.texts.get("import_name").cloned().unwrap_or_default();
                let closed = crate::ui::modal::show(
                    ctx,
                    "import",
                    crate::ui::modal::Size::Lg,
                    "Import settings",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        let json = crate::ui::filepicker::Filter {
                            name: "JSON",
                            extensions: &["json"],
                        };
                        if let Some(Picked::Chosen(_)) = form::path(
                            ui,
                            &mut f,
                            "Settings file",
                            "",
                            "A file written by Export settings.",
                            &PathField::new(Mode::File(Some(json)), "Settings file").placeholder("No file chosen"),
                            &mut path,
                            "importpath",
                        ) {
                            read = true;
                        }
                        if let Some(b) = &bundle {
                            f.skip(14.0);
                            form::desc(ui, &mut f, &describe_source(b, &file_name));
                            f.skip(4.0);
                            let carries = b.get("secrets").and_then(Value::as_bool).unwrap_or(false);
                            form::desc(
                                ui,
                                &mut f,
                                if carries {
                                    "The file carries passwords and map keys, and they replace the ones stored here."
                                } else {
                                    "The file carries no passwords or map keys. The ones stored here are kept wherever the connection, camera or map service is still the same."
                                },
                            );
                            f.skip(14.0);
                            let absent: Vec<&str> =
                                PARTS.iter().map(|p| p.0).filter(|id| !present.contains(id)).collect();
                            section_switches(ui, &mut f, &mut parts, "What to take from the file", &absent);
                            f.skip(12.0);
                            form::message(
                            ui,
                            &mut f,
                            "The parts switched on replace the settings of this station. The interface reloads afterwards.",
                            Tone::Warn,
                        );
                        }
                        if let Some((m, tone)) = &msg {
                            f.skip(10.0);
                            form::message(ui, &mut f, m, *tone);
                        }
                        f.skip(18.0);
                        let (hit, bottom) = crate::ui::modal::actions(
                            ui,
                            f.x,
                            f.y,
                            f.w,
                            vec![
                                Button::new("Cancel", Variant::Secondary),
                                Button::new("Import", Variant::Primary)
                                    .icon("upload")
                                    .enabled(bundle.is_some() && parts.iter().any(|p| p.1) && !busy),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    },
                );
                for (id, on) in &parts {
                    if present.contains(id) {
                        self.ticks.insert(format!("import:{id}"), *on);
                    }
                }
                self.texts.insert("import_path", path.clone());
                if read {
                    self.read_import(&path);
                }
                if go {
                    self.hush("transfer_dialog");
                    let sections: Vec<&str> = parts.iter().filter(|p| p.1).map(|p| p.0).collect();
                    self.act(
                        ctx,
                        "import",
                        "",
                        "/api/settings/import",
                        json!({"bundle": bundle, "sections": sections}),
                    );
                }
                closed || close
            }
            Some(Dialog::Reset) => {
                let (mut go, mut close) = (false, false);
                let closed = crate::ui::modal::show(
                    ctx,
                    "reset",
                    crate::ui::modal::Size::Sm,
                    "Reset to factory settings",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::desc(
                        ui,
                        &mut f,
                        "Every setting of this station goes back to its default: the interface, the layout, the map, the connections, the vehicle settings, the folders, the company logo and the settings of every plugin. Plugins you installed yourself stay installed. Missions, logs, parameter files and map tiles are not touched.",
                    );
                        f.skip(12.0);
                        form::message(
                        ui,
                        &mut f,
                        "This cannot be undone. Export your settings first if you may want them back. The interface then reloads into the first start setup.",
                        Tone::Warn,
                    );
                        if let Some((m, tone)) = &msg {
                            f.skip(10.0);
                            form::message(ui, &mut f, m, *tone);
                        }
                        f.skip(18.0);
                        let (hit, bottom) = crate::ui::modal::actions(
                            ui,
                            f.x,
                            f.y,
                            f.w,
                            vec![
                                Button::new("Cancel", Variant::Secondary),
                                Button::new("Reset", Variant::Danger).icon("rotate-ccw").enabled(!busy),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    },
                );
                if go {
                    self.hush("transfer_dialog");
                    self.act(ctx, "reset", "", "/api/settings/reset", json!({}));
                }
                closed || close
            }
            Some(Dialog::Notice { title, lines }) => {
                let (title, lines) = (title.clone(), lines.clone());
                let mut close = false;
                let closed =
                    crate::ui::modal::show(ctx, "notice", crate::ui::modal::Size::Sm, &title, |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        for l in &lines {
                            form::desc(ui, &mut f, l);
                            f.skip(6.0);
                        }
                        f.skip(12.0);
                        let (hit, bottom) =
                            crate::ui::modal::actions(ui, f.x, f.y, f.w, vec![Button::new("Done", Variant::Primary)]);
                        close = hit.is_some();
                        bottom
                    });
                closed || close
            }
            _ => false,
        }
    }

    fn read_import(&mut self, path: &str) {
        let expanded = path.trim().trim_matches('"');
        let expanded = expanded.strip_prefix("~/").map_or(expanded.to_string(), |r| {
            format!("{}/{r}", std::env::var("HOME").unwrap_or_default())
        });
        self.hush("transfer_dialog");
        match std::fs::read_to_string(&expanded) {
            Err(_) => {
                self.import_bundle = None;
                self.fail("transfer_dialog", "Could not read the settings file.");
            }
            Ok(text) => match parse_bundle(&text) {
                Ok(b) => {
                    let name = std::path::Path::new(&expanded)
                        .file_name()
                        .map_or("the file".to_string(), |n| n.to_string_lossy().into_owned());
                    self.texts.insert("import_name", name);
                    self.import_bundle = Some(b);
                }
                Err(e) => {
                    self.import_bundle = None;
                    self.fail("transfer_dialog", &e);
                }
            },
        }
    }

    pub(super) fn transfer_done(&mut self, ctx: &egui::Context, name: &'static str, answer: Result<Value, String>) {
        let fallback = match name {
            "export" => "Export failed",
            "import" => "Import failed",
            _ => "Reset failed",
        };
        if let Some(e) = refused(&answer, fallback) {
            self.fail("transfer_dialog", &e);
            return;
        }
        let res = answer.unwrap_or(Value::Null);
        match name {
            "export" => {
                self.dialog = None;
                let path = res.get("path").and_then(Value::as_str).unwrap_or("");
                self.notes.push(format!("Settings exported to {path}"));
            }
            "import" => {
                self.dialog = Some(Dialog::Notice {
                    title: "Import settings".into(),
                    lines: import_lines(&res),
                });
                self.reload_all(ctx);
            }
            _ => {
                let mut lines = Vec::new();
                let restart = restart_names(&res);
                if !restart.is_empty() {
                    lines.push(format!("Restart Corvus GCS for the default {}.", restart.join(", ")));
                }
                lines.extend(
                    res.get("warnings")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>())
                        .unwrap_or_default(),
                );
                lines.insert(0, "Settings are back to their defaults.".into());
                self.reset_request = true;
                self.dialog = Some(Dialog::Notice {
                    title: "Reset to factory settings".into(),
                    lines,
                });
                self.reload_all(ctx);
            }
        }
    }

    /// The imported or reset config is read again, and every list with it.
    fn reload_all(&mut self, ctx: &egui::Context) {
        self.res.clear();
        self.get(ctx, "config", "/api/config");
    }
}

/// The caption over a list of parts and a switch for each one; `absent` ones
/// are greyed with the reason.
fn section_switches(
    ui: &mut egui::Ui,
    f: &mut Form,
    parts: &mut [(&'static str, bool)],
    caption: &str,
    absent: &[&str],
) {
    let g = style::galley(
        ui,
        &caption.to_uppercase(),
        10.0,
        style::Weight::W600,
        style::text_3(),
        0.7,
    );
    let h = g.size().y;
    ui.painter().galley(f.rect(h).min, g, style::text_3());
    f.y += h + 8.0;
    for (i, p) in PARTS.iter().enumerate() {
        let missing = absent.contains(&p.0);
        let hint = if missing { "Not in this file." } else { p.2 };
        if let Some(v) = form::switch_field(
            ui,
            f,
            ("part", p.0),
            Switch::new(p.1, hint, parts[i].1 && !missing).enabled(!missing, ""),
            style::text_1(),
            12.0,
        ) {
            parts[i].1 = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_file_is_recognised_by_its_kind() {
        assert!(parse_bundle("{\"kind\": \"corvus-settings\", \"config\": {}}").is_ok());
        let bad = "This is not a Corvus GCS settings file.";
        assert_eq!(parse_bundle("nope").unwrap_err(), bad);
        assert_eq!(parse_bundle("{\"kind\": \"other\", \"config\": {}}").unwrap_err(), bad);
        assert_eq!(parse_bundle("{\"kind\": \"corvus-settings\"}").unwrap_err(), bad);
    }

    #[test]
    fn the_import_dialog_names_the_file_and_what_it_holds() {
        let b = json!({"version": "2026.10.03", "exported_at": "2026-10-29T10:00:00Z", "sections": ["map", "interface", "bogus"]});
        assert_eq!(
            describe_source(&b, "station.json"),
            "station.json, from Corvus GCS 2026.10.03, exported 2026-10-29T10:00:00Z."
        );
        assert_eq!(describe_source(&json!({}), "x.json"), "x.json.");
        assert_eq!(sections_in(&b), ["interface", "map"]);
        assert_eq!(PARTS.len(), 7);
    }

    #[test]
    fn the_import_result_lists_what_needs_a_restart() {
        let lines =
            import_lines(&json!({"plugins_installed": 2, "restart": ["http_port", "x"], "warnings": ["Careful."]}));
        assert_eq!(
            lines,
            [
                "Settings imported.",
                "2 plugins installed.",
                "Restart Corvus GCS for the new HTTP port, x.",
                "Careful."
            ]
        );
        assert_eq!(import_lines(&json!({"plugins_installed": 1}))[1], "1 plugin installed.");
        assert_eq!(import_lines(&json!({})), ["Settings imported."]);
    }
}
