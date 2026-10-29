//! The Connections pill: the links and vehicles, the saved SSH connections,
//! what Corvus is started with, and the MCP server (`renderVehiclesSection`,
//! `renderSSHSection`, `renderConnectionSection` of `src/js/sidenav.js`,
//! `src/js/settings-mcp.js`).

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::{model, refused, Dialog, SettingsPage};
use crate::ui::form::{self, Form, Tone, CONTROL_H};
use crate::ui::kit::{self, Button, Variant};
use crate::ui::style::{self, Weight};

const FLEET_HINT: &str = "One link is one vehicle. A second vehicle needs a radio of its own and a link of its own. Another autopilot heard on a link is ignored.";

const MCP_SERVER_HINT: &str = "Lets an AI client such as OpenCode read the vehicle and, as far as you allow it, fly it. Only this computer can connect, and only with its token.";

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list(v: Option<&Value>, key: &str) -> Vec<Value> {
    v.and_then(|d| d.get(key))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The link's state colour (`updateLinkRow`): connected is healthy, a link on
/// its way is a warning, anything else is off.
pub(super) fn link_level(status: &str) -> &'static str {
    match status {
        "connected" => "healthy",
        "degraded" | "connecting" | "reconnecting" => "warning",
        _ => "off",
    }
}

impl SettingsPage {
    pub(super) fn connections_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "Vehicles", |ui, f| self.links_card(ui, f));
        form::section(ui, f, "SSH Connections", |ui, f| self.ssh_card(ui, f));
        form::section(ui, f, "Connection", |ui, f| {
            form::card(ui, f, "", |ui, f| {
                let mav = self
                    .cfg
                    .get("mavlink_connection")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("\u{2014}")
                    .to_string();
                let port = self
                    .cfg
                    .get("http_port")
                    .filter(|p| !p.is_null())
                    .map_or("\u{2014}".to_string(), |p| p.to_string());
                form::row(ui, f, "MAVLink", &mav, false);
                form::row(ui, f, "HTTP Port", &port, true);
            });
        });
        self.mcp_card(ui, f);
    }

    fn links_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let fleet = self.live(ui.ctx(), "fleet", "/api/fleet", 2.0);
        let links = list(fleet.as_ref(), "links");
        let active = fleet
            .as_ref()
            .and_then(|d| d.get("active_link"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let msg = self.msgs.get("links").cloned();
        let mut remove: Option<String> = None;
        let mut open = false;
        form::card(ui, f, "Links", |ui, f| {
            for (i, l) in links.iter().enumerate() {
                if i > 0 {
                    f.skip(8.0);
                }
                let h = 35.0;
                let r = f.rect(h);
                let on = s(l, "id") == active;
                ui.painter().rect_filled(r, style::RADIUS, style::glass(0.82));
                ui.painter().rect_stroke(
                    r,
                    style::RADIUS,
                    Stroke::new(1.0, if on { style::accent() } else { style::tint(0.09) }),
                    egui::StrokeKind::Inside,
                );
                let color = match link_level(s(l, "status")) {
                    "healthy" => style::healthy(),
                    "warning" => style::warning(),
                    _ => style::text_3(),
                };
                kit::dot(
                    ui.painter(),
                    Pos2::new(r.left() + 12.0 + 3.5, r.center().y),
                    color,
                    link_level(s(l, "status")) != "off",
                );
                let label = style::galley(ui, s(l, "label"), 12.0, Weight::W600, style::text_1(), 0.0);
                let lx = r.left() + 12.0 + 7.0 + 8.0;
                ui.painter().galley(
                    Pos2::new(lx, r.center().y - label.size().y / 2.0),
                    label.clone(),
                    style::text_1(),
                );
                let count = if l.get("vehicles").and_then(Value::as_f64).unwrap_or(0.0) > 0.0 {
                    "1 vehicle"
                } else {
                    "No vehicle yet"
                };
                let c = style::galley(ui, count, 11.0, Weight::W400, style::text_3(), 0.0);
                let primary = l.get("primary").and_then(Value::as_bool).unwrap_or(false);
                let right = r.right() - 12.0 - if primary { 0.0 } else { 20.0 + 8.0 };
                ui.painter().galley(
                    Pos2::new(right - c.size().x, r.center().y - c.size().y / 2.0),
                    c.clone(),
                    style::text_3(),
                );
                let conn_x = lx + label.size().x + 8.0;
                let conn = style::galley_mono(ui, s(l, "connection"), 12.0, style::text_2());
                let clip = Rect::from_min_max(
                    Pos2::new(conn_x, r.top()),
                    Pos2::new(right - c.size().x - 8.0, r.bottom()),
                );
                ui.painter_at(clip).galley(
                    Pos2::new(conn_x, r.center().y - conn.size().y / 2.0),
                    conn,
                    style::text_2(),
                );
                if !primary
                    && kit::icon_btn(
                        ui,
                        Rect::from_center_size(Pos2::new(r.right() - 12.0 - 10.0, r.center().y), Vec2::splat(20.0)),
                        "x",
                        "Close this link",
                        false,
                        ("closelink", s(l, "id")),
                    )
                {
                    remove = Some(s(l, "id").to_string());
                }
                f.y = r.bottom();
            }
            if !links.is_empty() {
                f.skip(8.0);
            }
            form::note(ui, f, FLEET_HINT);
            f.skip(8.0);
            let busy = self.is_busy("fleet_add");
            let bw = {
                let b = Button::new("Open link", Variant::Primary).icon("plus").small();
                b.width(ui)
            };
            let y = f.y;
            let r = Rect::from_min_size(Pos2::new(f.x, y), Vec2::new(f.w - bw - 8.0, CONTROL_H));
            let buf = self.texts.entry("fleet_conn").or_default();
            let resp = crate::ui::field::TextField::new(buf)
                .hint("serial:/dev/ttyUSB1:57600 or udpin:0.0.0.0:14560")
                .mono(true)
                .pad(11.0, 11.0)
                .show(ui, r, "fleetconn");
            let b = Button::new("Open link", Variant::Primary)
                .icon("plus")
                .small()
                .enabled(!busy);
            let bh = b.height();
            let go = b.show(
                ui,
                Rect::from_min_size(
                    Pos2::new(r.right() + 8.0, y + (CONTROL_H - bh) / 2.0),
                    Vec2::new(bw, bh),
                ),
                "openlink",
            );
            open = go || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            f.y = y + CONTROL_H;
            if let Some((t, tone)) = &msg {
                f.skip(8.0);
                form::message(ui, f, t, *tone);
            }
        });
        if let Some(id) = remove {
            self.hush("links");
            self.act(
                ui.ctx(),
                "fleet_remove",
                &id,
                "/api/fleet/links/remove",
                json!({"id": id}),
            );
        }
        if open {
            let conn = self.text("fleet_conn").trim().to_string();
            if !conn.is_empty() {
                self.hush("links");
                self.act(
                    ui.ctx(),
                    "fleet_add",
                    "",
                    "/api/fleet/links/add",
                    json!({"connection": conn}),
                );
            }
        }
    }

    pub(super) fn fleet_done(&mut self, ctx: &egui::Context, name: &'static str, answer: Result<Value, String>) {
        match refused(&answer, "The link could not be opened.") {
            Some(e) => self.fail("links", &e),
            None => {
                if name == "fleet_add" {
                    self.texts.remove("fleet_conn");
                }
                self.res.remove("fleet");
                self.get(ctx, "fleet", "/api/fleet");
            }
        }
    }

    fn ssh_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let data = self.want(ui.ctx(), "ssh", "/api/ssh/connections");
        let failed = data.is_none() && self.failed("ssh");
        let conns = list(data.as_ref(), "connections");
        let msg = self.msgs.get("ssh").cloned();
        let mut connect: Option<Value> = None;
        let mut edit: Option<Value> = None;
        let mut remove: Option<String> = None;
        let mut add = false;
        form::card(ui, f, "", |ui, f| {
            if failed {
                form::desc(ui, f, "Could not load SSH connections.");
                return;
            }
            if data.is_none() {
                form::desc(ui, f, "Loading\u{2026}");
                return;
            }
            if conns.is_empty() {
                form::desc(ui, f, "No saved SSH connections. Click + to add one.");
            }
            for (i, c) in conns.iter().enumerate() {
                let line = style::galley(ui, &model::ssh_line(c), 12.0, Weight::W400, style::text_1(), 0.0);
                let top = f.y + 9.0;
                let h = 28.0;
                let name = s(c, "name");
                let mut x = f.x + f.w - 28.0;
                if kit::icon_btn(
                    ui,
                    Rect::from_min_size(Pos2::new(x, top), Vec2::splat(28.0)),
                    "trash-2",
                    &format!("Remove {name}"),
                    false,
                    ("sshrm", i),
                ) {
                    remove = Some(name.to_string());
                }
                x -= 34.0;
                if kit::icon_btn(
                    ui,
                    Rect::from_min_size(Pos2::new(x, top), Vec2::splat(28.0)),
                    "pencil",
                    &format!("Edit {name}"),
                    false,
                    ("sshedit", i),
                ) {
                    edit = Some(c.clone());
                }
                let b = Button::new("CONNECT", Variant::Primary).small();
                let bw = b.width(ui);
                x -= 6.0 + bw;
                let bh = b.height();
                if b.show(
                    ui,
                    Rect::from_min_size(Pos2::new(x, top + (h - bh) / 2.0), Vec2::new(bw, bh)),
                    ("sshconnect", i),
                ) {
                    connect = Some(c.clone());
                }
                let clip = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(x - 10.0, top + h));
                ui.painter_at(clip)
                    .galley(Pos2::new(f.x, top + (h - line.size().y) / 2.0), line, style::text_1());
                f.y = top + h + 9.0;
                if i + 1 < conns.len() {
                    ui.painter().hline(
                        egui::Rangef::new(f.x, f.x + f.w),
                        f.y + 0.5,
                        Stroke::new(1.0, style::border()),
                    );
                    f.y += 1.0;
                }
            }
            f.skip(10.0);
            let b = Button::new("ADD CONNECTION", Variant::Secondary).icon("plus");
            add = form::block_button(ui, f, b, "sshadd");
            if let Some((t, tone)) = &msg {
                f.skip(10.0);
                form::message(ui, f, t, *tone);
            }
        });
        if let Some(c) = connect {
            self.hush("ssh");
            self.ssh_request = Some(c);
            self.say("ssh", "Connecting in the SSH tab\u{2026}", Tone::Plain);
        }
        if let Some(c) = edit {
            for (k, key) in [
                ("ssh_name", "name"),
                ("ssh_host", "host"),
                ("ssh_user", "username"),
                ("ssh_key", "key_path"),
            ] {
                *self.text(k) = s(&c, key).to_string();
            }
            *self.text("ssh_port") = c
                .get("port")
                .map_or("22".to_string(), |p| p.to_string().trim_matches('"').to_string());
            self.text("ssh_password").clear();
            self.hush("ssh_dialog");
            self.dialog = Some(Dialog::Ssh {
                original: Some(s(&c, "name").to_string()),
            });
        }
        if add {
            for k in ["ssh_name", "ssh_host", "ssh_user", "ssh_key", "ssh_password"] {
                self.text(k).clear();
            }
            *self.text("ssh_port") = "22".into();
            self.hush("ssh_dialog");
            self.dialog = Some(Dialog::Ssh { original: None });
        }
        if let Some(name) = remove {
            self.dialog = Some(Dialog::SshRemove { name });
        }
    }

    pub(super) fn ssh_done(
        &mut self,
        ctx: &egui::Context,
        name: &'static str,
        _arg: &str,
        answer: Result<Value, String>,
    ) {
        let fallback = if name == "ssh_remove" {
            "Could not remove the connection."
        } else {
            "Could not save the connection."
        };
        match refused(&answer, fallback) {
            Some(e) => self.fail("ssh_dialog", &e),
            None => {
                self.dialog = None;
                self.res.remove("ssh");
                self.get(ctx, "ssh", "/api/ssh/connections");
            }
        }
    }

    pub(super) fn ssh_dialogs(&mut self, ctx: &egui::Context) -> bool {
        let msg = self.msgs.get("ssh_dialog").cloned();
        match self.dialog.as_ref() {
            Some(Dialog::Ssh { original }) => {
                let original = original.clone();
                let (mut go, mut close) = (false, false);
                let busy = self.is_busy("ssh_save");
                let t = &mut self.texts;
                let mut take = |k: &'static str| t.remove(k).unwrap_or_default();
                let (mut name, mut host, mut port, mut user, mut password, mut key) = (
                    take("ssh_name"),
                    take("ssh_host"),
                    take("ssh_port"),
                    take("ssh_user"),
                    take("ssh_password"),
                    take("ssh_key"),
                );
                let title = if original.is_some() {
                    "Edit SSH connection"
                } else {
                    "Add SSH connection"
                };
                let closed =
                    crate::ui::modal::show(ctx, "sshdialog", crate::ui::modal::Size::Md, title, |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::input(ui, &mut f, "Name", "", "", &mut name, "drone", false, false, "sshname");
                        f.skip(10.0);
                        form::input(
                            ui,
                            &mut f,
                            "Host",
                            "",
                            "",
                            &mut host,
                            "192.168.1.20",
                            true,
                            false,
                            "sshhost",
                        );
                        f.skip(10.0);
                        form::input(ui, &mut f, "Port", "", "", &mut port, "22", true, false, "sshport");
                        f.skip(10.0);
                        form::input(ui, &mut f, "Username", "", "", &mut user, "pi", false, false, "sshuser");
                        f.skip(10.0);
                        let hint = if original.is_some() {
                            "Leave empty to keep the saved password."
                        } else {
                            ""
                        };
                        form::input(
                            ui,
                            &mut f,
                            "Password",
                            "",
                            hint,
                            &mut password,
                            "",
                            false,
                            true,
                            "sshpassword",
                        );
                        f.skip(10.0);
                        form::input(
                            ui,
                            &mut f,
                            "Key file",
                            "",
                            "Optional. The path of a private key on this computer.",
                            &mut key,
                            "~/.ssh/id_ed25519",
                            true,
                            false,
                            "sshkey",
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
                                Button::new("CANCEL", Variant::Secondary),
                                Button::new("SAVE", Variant::Primary).icon("save").enabled(!busy),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    });
                self.texts.insert("ssh_name", name.clone());
                self.texts.insert("ssh_host", host.clone());
                self.texts.insert("ssh_port", port.clone());
                self.texts.insert("ssh_user", user.clone());
                self.texts.insert("ssh_password", password.clone());
                self.texts.insert("ssh_key", key.clone());
                if go {
                    let port_n = port.trim().parse::<u32>().ok().filter(|p| (1..=65535).contains(p));
                    if name.trim().is_empty() {
                        self.fail("ssh_dialog", "Enter a name.");
                    } else if host.trim().is_empty() {
                        self.fail("ssh_dialog", "Enter a host.");
                    } else if port_n.is_none() {
                        self.fail("ssh_dialog", "The port is a number from 1 to 65535.");
                    } else {
                        self.hush("ssh_dialog");
                        let mut body = json!({
                            "name": name.trim(), "host": host.trim(), "port": port_n,
                            "username": user.trim(), "key_path": key.trim(),
                        });
                        if !password.is_empty() || original.is_none() {
                            body["password"] = json!(password);
                        }
                        if let Some(o) = &original {
                            body["original_name"] = json!(o);
                        }
                        self.act(ctx, "ssh_save", "", "/api/ssh/connections", body);
                    }
                }
                closed || close
            }
            Some(Dialog::SshRemove { name }) => {
                let name = name.clone();
                let (mut go, mut close) = (false, false);
                let closed = crate::ui::modal::show(
                    ctx,
                    "sshremove",
                    crate::ui::modal::Size::Sm,
                    "Remove connection",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::note(ui, &mut f, &format!("Remove connection {name}?"));
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
                                Button::new("CANCEL", Variant::Secondary),
                                Button::new("REMOVE", Variant::Danger).icon("trash-2"),
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
                    self.hush("ssh_dialog");
                    self.act(
                        ctx,
                        "ssh_remove",
                        "",
                        "/api/ssh/connections/remove",
                        json!({"name": name}),
                    );
                }
                closed || close
            }
            _ => false,
        }
    }

    // ---- the MCP server ----

    fn mcp_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let data = self.live(ui.ctx(), "mcp", "/api/mcp", 2.0);
        form::section(ui, f, "MCP server", |ui, f| {
            let Some(d) = data.as_ref() else {
                let t = if self.failed("mcp") {
                    "Could not read the MCP settings."
                } else {
                    "Reading the MCP settings\u{2026}"
                };
                form::card(ui, f, "", |ui, f| form::desc(ui, f, t));
                return;
            };
            let settings = d.get("settings").cloned().unwrap_or(Value::Null);
            let on = settings.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let catalog = list(Some(d), "catalog");
            let status = d.get("status").cloned().unwrap_or(Value::Null);
            let mut perms = false;
            let mut connect = false;
            let mut toggled = None;
            form::card(ui, f, "", |ui, f| {
                toggled = form::switch(ui, f, "mcpserver", "MCP server", MCP_SERVER_HINT, on);
                f.skip(2.0);
                let who = kit::wrapped_weight(
                    ui,
                    &model::clients_line(&status),
                    12.0,
                    Weight::W400,
                    style::text_2(),
                    f.w,
                    1.21,
                );
                let h = who.size().y;
                ui.painter().galley(Pos2::new(f.x, f.y), who, style::text_2());
                f.y += h + 2.0;
                let sum = kit::wrapped_weight(
                    ui,
                    &model::permission_summary(&settings, &catalog),
                    12.0,
                    Weight::W400,
                    style::text_2(),
                    f.w,
                    1.21,
                );
                let h = sum.size().y;
                ui.painter().galley(Pos2::new(f.x, f.y), sum, style::text_2());
                f.y += h + 10.0;
                let mut buttons = vec![Button::new("Permissions", Variant::Secondary).icon("shield").small()];
                if on {
                    buttons.push(
                        Button::new("Connect an AI client", Variant::Secondary)
                            .icon("plug")
                            .small(),
                    );
                }
                match form::actions(ui, f, buttons) {
                    Some(0) => perms = true,
                    Some(1) => connect = true,
                    _ => {}
                }
                if let Some((t, tone)) = self.msgs.get("mcp").cloned() {
                    f.skip(10.0);
                    form::message(ui, f, &t, tone);
                }
            });
            if let Some(v) = toggled {
                self.hush("mcp");
                self.act(ui.ctx(), "mcp_save", "", "/api/mcp/settings", json!({"enabled": v}));
            }
            if perms {
                self.dialog = Some(Dialog::McpPerms);
            }
            if connect {
                self.hush("mcp_dialog");
                self.dialog = Some(Dialog::McpConnect { shown: false });
            }
        });
    }

    pub(super) fn mcp_done(&mut self, ctx: &egui::Context, name: &'static str, answer: Result<Value, String>) {
        if let Some(e) = refused(&answer, "Not saved.") {
            self.fail("mcp", &e);
            self.res.remove("mcp");
            self.get(ctx, "mcp", "/api/mcp");
            return;
        }
        let v = answer.unwrap_or(Value::Null);
        if let Some(d) = self.res.get_mut("mcp").and_then(|s| s.value.as_mut()) {
            if name == "mcp_token" {
                d["token"] = v.get("token").cloned().unwrap_or(Value::Null);
            } else {
                d["settings"] = v.get("settings").cloned().unwrap_or(Value::Null);
                d["status"] = v.get("status").cloned().unwrap_or(Value::Null);
            }
        }
        if name == "mcp_token" {
            self.say(
                "mcp_dialog",
                "New token made. Give it to your clients again.",
                Tone::Plain,
            );
        }
    }

    pub(super) fn mcp_dialogs(&mut self, ctx: &egui::Context) -> bool {
        let Some(d) = self.res.get("mcp").and_then(|s| s.value.clone()) else {
            return true;
        };
        let settings = d.get("settings").cloned().unwrap_or(Value::Null);
        let catalog = list(Some(&d), "catalog");
        let mut save: Vec<Value> = Vec::new();
        let mut close = false;
        match self.dialog.as_ref() {
            Some(Dialog::McpPerms) => {
                let max_s = d.get("control_max_s").and_then(Value::as_f64).unwrap_or(5.0);
                let on = |path: &[&str]| {
                    let mut cur = &settings;
                    for p in path {
                        cur = cur.get(*p).unwrap_or(&Value::Null);
                    }
                    cur.as_bool().unwrap_or(false)
                };
                let closed = crate::ui::modal::show(
                    ctx,
                    "mcpperms",
                    crate::ui::modal::Size::Lg,
                    "MCP permissions",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        if let Some(v) = form::switch(
                        ui,
                        &mut f,
                        "mcpfleet",
                        "Other vehicles of the fleet",
                        "Off, the AI client acts only on the vehicle you fly. On, it can name any vehicle Corvus hears.",
                        on(&["fleet"]),
                    ) {
                        save.push(json!({"fleet": v}));
                    }
                        f.skip(10.0);
                        let intro = format!(
                        "A tool that is off is not even offered to the client. With Ask me first, Corvus shows you each request and sends nothing until you allow it. Direct control lasts at most {} s per request; without a new one the vehicle holds.",
                        crate::data::units::js_number(max_s)
                    );
                        let g = kit::wrapped_weight(ui, &intro, 12.0, Weight::W400, style::text_2(), f.w, 1.5);
                        let h = g.size().y;
                        ui.painter().galley(f.rect(h).min, g, style::text_2());
                        f.y += h + 6.0;
                        for p in &catalog {
                            let id = s(p, "id");
                            if let Some(v) =
                                form::switch(ui, &mut f, ("mcpperm", id), s(p, "label"), "", on(&["permissions", id]))
                            {
                                save.push(json!({"permissions": {id: v}}));
                            }
                            let tools: Vec<&str> = p
                                .get("tools")
                                .and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(Value::as_str).collect())
                                .unwrap_or_default();
                            for (text, mono) in [(s(p, "hint").to_string(), false), (tools.join(", "), true)] {
                                if text.is_empty() {
                                    continue;
                                }
                                let g = if mono {
                                    kit::wrapped_weight(
                                        ui,
                                        &text,
                                        11.0,
                                        Weight::W400,
                                        style::text_3(),
                                        f.w - 50.0,
                                        1.45,
                                    )
                                } else {
                                    kit::wrapped_weight(
                                        ui,
                                        &text,
                                        11.0,
                                        Weight::W400,
                                        style::text_2(),
                                        f.w - 50.0,
                                        1.45,
                                    )
                                };
                                let h = g.size().y;
                                ui.painter().galley(
                                    Pos2::new(f.x, f.y + 4.0),
                                    g,
                                    if mono { style::text_3() } else { style::text_2() },
                                );
                                f.y += h + 4.0;
                            }
                            if p.get("writes").and_then(Value::as_bool).unwrap_or(false) && on(&["permissions", id]) {
                                if let Some(v) = form::switch_field(
                                    ui,
                                    &mut f,
                                    ("mcpask", id),
                                    form::Switch::new("Ask me first", "", on(&["confirm", id])),
                                    style::text_2(),
                                    12.0,
                                ) {
                                    save.push(json!({"confirm": {id: v}}));
                                }
                            }
                        }
                        f.skip(14.0);
                        let t = style::galley(ui, "Plugin tools", 11.0, Weight::W600, style::text_1(), 0.0);
                        let th = t.size().y;
                        ui.painter().galley(f.rect(th).min, t, style::text_1());
                        f.y += th + 6.0;
                        for text in [
                        "No plugin offers tools right now. A plugin's tools are listed here while Corvus is open, the server is on, and the plugin allows them in its own settings (Settings, Plugins).",
                        "Plugin tools also need Plugin tools switched on above.",
                    ] {
                        let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_2(), f.w, 1.5);
                        let h = g.size().y;
                        ui.painter().galley(f.rect(h).min, g, style::text_2());
                        f.y += h + 6.0;
                    }
                        f.skip(12.0);
                        let (hit, bottom) =
                            crate::ui::modal::actions(ui, f.x, f.y, f.w, vec![Button::new("Done", Variant::Primary)]);
                        close = hit.is_some();
                        bottom
                    },
                );
                close |= closed;
            }
            Some(Dialog::McpConnect { shown }) => {
                let shown = *shown;
                let token = s(&d, "token").to_string();
                let url = s(&d, "url").to_string();
                let local = d.get("local").and_then(Value::as_bool).unwrap_or(false);
                let msg = self.msgs.get("mcp_dialog").cloned();
                let mut flip = false;
                let mut copy: Option<String> = None;
                let mut renew = false;
                let closed = crate::ui::modal::show(
                    ctx,
                    "mcpconnect",
                    crate::ui::modal::Size::Lg,
                    "Connect an AI client",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        let mut url_buf = url.clone();
                        form::field(
                        ui,
                        &mut f,
                        "Address",
                        "",
                        "The port is the one Corvus runs on. If Corvus has to move to another port, the address changes with it.",
                        |ui, x, y, w| {
                            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w - 36.0, CONTROL_H));
                            crate::ui::field::TextField::new(&mut url_buf).mono(true).pad(11.0, 11.0).show(ui, r, "mcpurl");
                            if kit::icon_btn(ui, Rect::from_min_size(Pos2::new(x + w - 28.0, y + 2.0), Vec2::splat(28.0)), "copy", "Copy the address", false, "mcpcopyurl") {
                                copy = Some(url.clone());
                            }
                            CONTROL_H
                        },
                    );
                        f.skip(10.0);
                        if local {
                            let mut tok = if shown { token.clone() } else { "\u{2022}".repeat(24) };
                            form::field(
                            ui,
                            &mut f,
                            "Token",
                            "",
                            "Like a password for the MCP server. It is kept in its own file, never in a settings export.",
                            |ui, x, y, w| {
                                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w - 3.0 * 34.0, CONTROL_H));
                                crate::ui::field::TextField::new(&mut tok).mono(true).pad(11.0, 11.0).show(ui, r, "mcptoken");
                                let mut bx = r.right() + 6.0;
                                for (icon, title, which) in [
                                    (if shown { "eye-off" } else { "eye" }, if shown { "Hide the token" } else { "Show the token" }, 0),
                                    ("copy", "Copy the token", 1),
                                    ("refresh-cw", "Make a new token. Clients holding the old one are refused from now on.", 2),
                                ] {
                                    if kit::icon_btn(ui, Rect::from_min_size(Pos2::new(bx, y + 2.0), Vec2::splat(28.0)), icon, title, false, ("mcptok", which)) {
                                        match which {
                                            0 => flip = true,
                                            1 => copy = Some(token.clone()),
                                            _ => renew = true,
                                        }
                                    }
                                    bx += 34.0;
                                }
                                CONTROL_H
                            },
                        );
                        } else {
                            form::desc(ui, &mut f, "The token is shown only on the computer Corvus runs on.");
                        }
                        f.skip(12.0);
                        for t in [
                        "1. Switch on what the client may do, under Permissions.",
                        "2. Put this in opencode.json in your project folder, or in ~/.config/opencode/opencode.json for every project:",
                    ] {
                        let g = kit::wrapped_weight(ui, t, 11.0, Weight::W400, style::text_2(), f.w, 1.45);
                        let h = g.size().y;
                        ui.painter().galley(f.rect(h).min, g, style::text_2());
                        f.y += h + 4.0;
                    }
                        let cfg = model::opencode_config(&url, if shown { &token } else { "<token>" });
                        let g = style::galley_mono(ui, &cfg, 11.0, style::text_2());
                        let ch = g.size().y + 20.0;
                        let r = f.rect(ch);
                        ui.painter().rect_filled(r, style::RADIUS_BTN, style::input(0.6));
                        ui.painter().rect_stroke(
                            r,
                            style::RADIUS_BTN,
                            Stroke::new(1.0, style::border()),
                            egui::StrokeKind::Inside,
                        );
                        ui.painter().galley(r.min + Vec2::new(10.0, 10.0), g, style::text_2());
                        f.y = r.bottom() + 8.0;
                        if form::actions(
                            ui,
                            &mut f,
                            vec![Button::new("Copy for OpenCode", Variant::Secondary)
                                .icon("copy")
                                .small()],
                        )
                        .is_some()
                        {
                            copy = Some(model::opencode_config(&url, &token));
                        }
                        f.skip(8.0);
                        for t in [
                        "3. Start OpenCode in that folder and ask, for example, what the battery says. Run /mcp in OpenCode to see that corvus is connected.",
                        "Any other client that speaks MCP over HTTP (Streamable HTTP) takes the same address with the header Authorization: Bearer and the token.",
                        "These switches decide what the client may do through MCP. A client that may also run commands on this computer could reach Corvus another way, so allow those commands only where you trust it.",
                    ] {
                        let g = kit::wrapped_weight(ui, t, 11.0, Weight::W400, style::text_2(), f.w, 1.45);
                        let h = g.size().y;
                        ui.painter().galley(f.rect(h).min, g, style::text_2());
                        f.y += h + 8.0;
                    }
                        if let Some((m, tone)) = &msg {
                            form::message(ui, &mut f, m, *tone);
                        }
                        f.skip(12.0);
                        let (hit, bottom) =
                            crate::ui::modal::actions(ui, f.x, f.y, f.w, vec![Button::new("Done", Variant::Primary)]);
                        close = hit.is_some();
                        bottom
                    },
                );
                close |= closed;
                if flip {
                    self.dialog = Some(Dialog::McpConnect { shown: !shown });
                }
                if let Some(text) = copy {
                    ctx.copy_text(text);
                    self.say("mcp_dialog", "Copied to the clipboard.", Tone::Plain);
                }
                if renew {
                    self.act(ctx, "mcp_token", "", "/api/mcp/token", json!({}));
                }
            }
            _ => return false,
        }
        for patch in save {
            self.act(ctx, "mcp_save", "", "/api/mcp/settings", patch);
        }
        close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_row_takes_the_colour_of_its_state() {
        assert_eq!(link_level("connected"), "healthy");
        for s in ["degraded", "connecting", "reconnecting"] {
            assert_eq!(link_level(s), "warning");
        }
        assert_eq!(link_level("disconnected"), "off");
        assert_eq!(link_level(""), "off");
    }
}
