//! What Schwalby offers an AI client through Corvus' MCP server
//! (`mcpTools`, `mcpButton`, `headlessPress` of `schwalby.js`), when the
//! operator has allowed it twice: Plugin tools in Settings, Connections, MCP,
//! and one or both of Schwalby's own switches (Settings, Plugins, its gear).
//!
//! "May add buttons" lets it add buttons that run on this computer, and change
//! or remove the ones it added; such a button carries an AI mark, and a new or
//! changed one waits on the shelf until the operator presses it and allows it.
//! "May press buttons" lets it press any allowed button, as the operator
//! would. With the shelf closed a press goes the same way, through the same
//! endpoints and sessions, without the row to show it.

use serde_json::{json, Value};

use super::model::{describe_button, mcp_button, normalize_buttons, Button, MAX_BUTTONS, MCP_EDIT, MCP_RUN};
use super::Schwalby;
use corvus_api::plugin::{Host, McpTool, Reply};

enum Job {
    /// A change of the shelf, answered once the backend has saved it.
    Saving { token: u64, reply: Reply, ok: Value },
    /// A press, answered once its task is over.
    Pressing { token: u64, id: String, label: String },
}

#[derive(Default)]
pub struct State {
    jobs: Vec<Job>,
    done: Vec<(u64, Result<Value, String>)>,
}

impl State {
    pub fn idle(&self) -> bool {
        self.jobs.is_empty()
    }
}

fn shelf(h: &Host) -> Vec<Button> {
    normalize_buttons(&h.settings())
}

fn id_arg(args: &Value) -> Result<String, String> {
    match args.get("id") {
        None | Some(Value::Null) => Err("id is required".into()),
        Some(Value::String(s)) => Ok(s.trim().to_string()),
        Some(_) => Err("id must be a string".into()),
    }
}

fn button_fields() -> Value {
    json!({
        "label": {"type": "string", "description": "What the button says"},
        "command": {"type": "string", "description": "The shell command it runs on this computer"},
        "directory": {"type": "string", "description": "The folder it runs in, empty for home"},
        "mode": {"type": "string", "enum": ["terminal", "background"],
            "description": "terminal: typed into a terminal of its own. background: started without a window, stopped when Corvus closes"},
        "restart": {"type": "boolean", "description": "Pressed again, stop it with Ctrl-C and start it again"},
        "color": {"type": "string", "description": "#RRGGBB, empty for the theme's own"},
    })
}

impl Schwalby {
    /// The tools Schwalby offers an AI client, by its two switches.
    pub(crate) fn mcp_tools_for(h: &Host) -> Vec<McpTool> {
        let on = |k: &str| h.options().get(k) == Some(&Value::Bool(true));
        let (edit, run) = (on(MCP_EDIT), on(MCP_RUN));
        if !edit && !run {
            return Vec::new();
        }
        let tool = |name: &str, title: &str, description: &str, schema: Value, read_only: bool| McpTool {
            name: name.into(),
            title: title.into(),
            description: description.into(),
            input_schema: schema,
            confirm: false,
            read_only,
        };
        let mut tools = vec![tool(
            "list_buttons",
            "Schwalby buttons",
            "The launch buttons on the Schwalby shelf: what each runs and where, and whether it waits for the operator to allow it.",
            json!({"type": "object", "properties": {}}),
            true,
        )];
        if edit {
            let mut props = button_fields();
            tools.push(tool(
                "add_button",
                "Add a Schwalby button",
                "Add a launch button that runs a command on this computer. It waits on the shelf until the operator allows it; it cannot be pressed before.",
                json!({"type": "object", "properties": props.clone(), "required": ["label", "command"]}),
                false,
            ));
            if let Some(o) = props.as_object_mut() {
                o.insert("id".into(), json!({"type": "string"}));
            }
            tools.push(tool(
                "update_button",
                "Change a Schwalby button",
                "Change a button you added. Changing what it runs makes it wait for the operator again.",
                json!({"type": "object", "properties": props, "required": ["id"]}),
                false,
            ));
            tools.push(tool(
                "remove_button",
                "Remove a Schwalby button",
                "Remove a button you added.",
                json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
                false,
            ));
        }
        if run {
            tools.push(tool(
                "press_button",
                "Press a Schwalby button",
                "Press a launch button, as the operator would: its program starts in its terminal or in the background.",
                json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
                false,
            ));
        }
        tools
    }

    /// Starts one tool call; the answer comes from `mcp_take`.
    pub(crate) fn mcp_start(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        let r = self.mcp_call_now(name, args, token, h);
        if let Err(e) = r {
            self.mcp.done.push((token, Err(e)));
        }
    }

    fn mcp_call_now(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) -> Result<(), String> {
        let offered = Self::mcp_tools_for(h);
        if !offered.iter().any(|t| t.name == name) {
            return Err(format!("The plugin offers no tool {name} right now."));
        }
        let list = shelf(h);
        let own = |id: &str| -> Result<Button, String> {
            let entry = list
                .iter()
                .find(|b| b.id == id)
                .ok_or_else(|| format!("No button {id}. list_buttons names them."))?;
            if !entry.by_mcp {
                return Err("That button is the operator's. You may change only the ones you added.".into());
            }
            Ok(entry.clone())
        };
        match name {
            "list_buttons" => {
                let on = |k: &str| h.options().get(k) == Some(&Value::Bool(true));
                let out = json!({
                    "buttons": list.iter().map(describe_button).collect::<Vec<_>>(),
                    "max": MAX_BUTTONS,
                    "you_may_add": on(MCP_EDIT),
                    "you_may_press": on(MCP_RUN),
                });
                self.mcp.done.push((token, Ok(out)));
            }
            "add_button" => {
                if list.len() >= MAX_BUTTONS {
                    return Err(format!("The shelf is full ({MAX_BUTTONS} buttons)."));
                }
                let button = mcp_button(args, None)?;
                let mut next = list.clone();
                let id = button.id.clone();
                next.push(button);
                let ok = json!({
                    "id": id, "waiting_for_operator": true,
                    "message": "Added. The operator has to allow it on the Schwalby shelf before it runs.",
                });
                self.save_shelf(&next, token, ok, h);
            }
            "update_button" => {
                let before = own(&id_arg(args)?)?;
                let after = mcp_button(args, Some(&before))?;
                let next: Vec<Button> = list
                    .iter()
                    .map(|b| if b.id == before.id { after.clone() } else { b.clone() })
                    .collect();
                let ok = json!({"id": after.id, "waiting_for_operator": after.pending});
                self.save_shelf(&next, token, ok, h);
            }
            "remove_button" => {
                let entry = own(&id_arg(args)?)?;
                let next: Vec<Button> = list.iter().filter(|b| b.id != entry.id).cloned().collect();
                self.save_shelf(&next, token, json!({"removed": entry.id}), h);
            }
            _ => {
                let id = id_arg(args)?;
                let entry = list
                    .iter()
                    .find(|b| b.id == id)
                    .cloned()
                    .ok_or_else(|| format!("No button {id}. list_buttons names them."))?;
                if entry.pending {
                    return Err("The operator has not allowed this button yet.".into());
                }
                self.launch(&entry, h);
                self.mcp.jobs.push(Job::Pressing {
                    token,
                    id: entry.id,
                    label: entry.label,
                });
            }
        }
        Ok(())
    }

    /// Saves the shelf from outside it, keeping the companion, and redraws an
    /// open one.
    fn save_shelf(&mut self, list: &[Button], token: u64, ok: Value, h: &mut Host) {
        let mut s = h.settings();
        if let Some(o) = s.as_object_mut() {
            o.insert(
                "buttons".into(),
                Value::Array(list.iter().map(Button::to_json).collect()),
            );
        }
        let reply = h.save_settings(&s, true);
        if self.is_open() && self.editor.is_none() {
            self.buttons = normalize_buttons(&h.settings());
        }
        self.mcp.jobs.push(Job::Saving { token, reply, ok });
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Moves the calls that wait on a save or a press.
    pub(crate) fn mcp_run_jobs(&mut self, _h: &mut Host) {
        let jobs = std::mem::take(&mut self.mcp.jobs);
        let mut keep = Vec::new();
        for job in jobs {
            match job {
                Job::Saving { token, reply, ok } => match reply.take() {
                    None => keep.push(Job::Saving { token, reply, ok }),
                    Some(Ok(_)) => self.mcp.done.push((token, Ok(ok))),
                    Some(Err(e)) => self.mcp.done.push((
                        token,
                        Err(if e.is_empty() {
                            "The shelf could not be saved.".to_string()
                        } else {
                            e
                        }),
                    )),
                },
                Job::Pressing { token, id, label } => {
                    if self.pressing(&id) {
                        keep.push(Job::Pressing { token, id, label });
                    } else if let Some(why) = self.errors.get(&id) {
                        self.mcp.done.push((token, Err(why.clone())));
                    } else {
                        let message = self
                            .status
                            .as_ref()
                            .map(|s| s.0.clone())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| format!("{label} started"));
                        self.mcp.done.push((token, Ok(json!({"ok": true, "message": message}))));
                    }
                }
            }
        }
        keep.append(&mut self.mcp.jobs);
        self.mcp.jobs = keep;
    }

    /// The answer of a call, once there is one.
    pub(crate) fn mcp_result(&mut self, token: u64) -> Option<Result<Value, String>> {
        let i = self.mcp.done.iter().position(|d| d.0 == token)?;
        Some(self.mcp.done.remove(i).1)
    }

    /// Whether any call waits on this plugin.
    pub(crate) fn mcp_idle(&self) -> bool {
        self.mcp.idle()
    }
}
