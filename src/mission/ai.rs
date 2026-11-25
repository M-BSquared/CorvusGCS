//! The planner's side of an AI client: the plan it put here, the operator's plan that it
//! replaced, and the plan shown as the backend is told of it.

use eframe::egui::{self, Rect};
use serde_json::{json, Value};

use super::backend::Job;
use super::plan::Tool;
use super::{issues, Dialog, Level, MissionPage};
use crate::i18n::tr_fmt;
use crate::mcp::drawings::{pill, Planned};
use crate::net::bg::Pending;
use crate::net::http;
use crate::ui::kit::{Button, Variant};

/// The least time between two looks at whether the plan changed, in seconds.
const REPORT_EVERY_S: f64 = 0.4;
/// The least time between two reports of warnings that changed while the plan did not.
const WARNINGS_EVERY_S: f64 = 2.0;
/// The pill stands under the tool bar and its hint.
const PILL_BELOW: f32 = 128.0;

#[derive(Default)]
pub struct AiPlan {
    /// The number of the client's plan shown last.
    seq: u64,
    /// That plan as it was shown, to know whether the operator went on from it.
    shown: Option<Value>,
    /// The operator's plan it replaced.
    pub(crate) mine: Option<Value>,
    /// The plan told last, and whether the page was on.
    reported: Option<(Value, bool)>,
    checks: Value,
    looked_at: f64,
    told_at: f64,
    reporting: Option<Pending<Result<Value, String>>>,
}

impl AiPlan {
    /// Whether a client has put a plan here.
    pub fn placed(&self) -> bool {
        self.shown.is_some()
    }
}

/// A plan without what the page finds out about it by itself: the ground under the start, and
/// what the connected vehicle is.
fn bare(mut plan: Value) -> Value {
    if let Some(home) = plan.get_mut("home").and_then(Value::as_object_mut) {
        home.remove("elevation");
    }
    if let Some(o) = plan.as_object_mut() {
        o.remove("aircraft");
    }
    plan
}

impl MissionPage {
    /// Takes in what a client did with the planner. A line for the notification board when the
    /// operator does not see it here.
    pub fn take_planned(&mut self, what: Planned, state: &Value) -> Option<String> {
        match what {
            Planned::Plan { seq, plan } if seq > self.ai.seq => {
                let n = plan.get("items").and_then(Value::as_array).map_or(0, Vec::len);
                let now = self.plan.to_json();
                let has_work = !self.plan.items.is_empty() && self.ai.shown.as_ref() != Some(&bare(now.clone()));
                if has_work {
                    self.ai.mine = Some(now);
                }
                // whatever was half done, or asked about the plan that goes
                self.suspend();
                self.dialog = Dialog::None;
                self.begun = true;
                let start = super::vehicle_start(state);
                self.plan.load_json(&plan, self.vehicle_type(), start);
                self.set_tool(Tool::Select);
                self.refresh();
                self.fit_pending = !self.plan.items.is_empty();
                self.ai.seq = seq;
                self.ai.shown = Some(bare(self.plan.to_json()));
                let text = tr_fmt(
                    "An AI client changed the mission plan: {n} points.",
                    &[("n", &n.to_string())],
                );
                self.say(Level::Ok, text.clone());
                Some(text)
            }
            Planned::Sent { seq, revision } if seq == self.ai.seq => {
                if self
                    .ai
                    .reported
                    .as_ref()
                    .is_some_and(|(p, _)| *p == self.plan.to_json())
                {
                    let vehicle = state.get("vehicle_id").and_then(Value::as_str).unwrap_or("");
                    self.tracker.sync_with(&self.plan, revision, vehicle);
                }
                None
            }
            _ => None,
        }
    }

    /// Tells the backend which plan is shown, when it changed (`/api/mission/planner/mirror`).
    pub fn report_plan(&mut self, ctx: &egui::Context, page_on: bool, now: f64) {
        if page_on {
            self.begin(ctx);
        }
        if self.calls.busy(|j| matches!(j, Job::LoadPlan { .. })) {
            self.take_answers(ctx);
            return;
        }
        if self.ai.reporting.as_ref().is_some_and(|p| p.take().is_none()) {
            return;
        }
        self.ai.reporting = None;
        let rest = REPORT_EVERY_S - (now - self.ai.looked_at);
        if rest > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(rest));
            return;
        }
        self.ai.looked_at = now;
        let plan = self.plan.to_json();
        let same = self
            .ai
            .reported
            .as_ref()
            .is_some_and(|(p, on)| *p == plan && *on == page_on);
        // the warnings follow the vehicle, so alone they are told less often
        if same && now - self.ai.told_at < WARNINGS_EVERY_S {
            return;
        }
        let checks = self.checks();
        if same && self.ai.checks == checks {
            return;
        }
        self.ai.told_at = now;
        let body = json!({"plan": plan, "seq": self.ai.seq, "page": page_on, "checks": checks});
        self.ai.checks = checks;
        self.ai.reported = Some((plan, page_on));
        let server = self.calls.server().clone();
        self.ai.reporting = Some(Pending::spawn(ctx, move || {
            http::post_json(&server, "/api/mission/planner/mirror", &body)
        }));
    }

    /// What the sidebar says about the plan: the warnings and the summary.
    fn checks(&self) -> Value {
        let problems: Vec<String> = issues::problems(&self.plan, &self.units, self.ground.as_ref(), self.link.start)
            .into_iter()
            .map(|i| i.text)
            .collect();
        let summary: serde_json::Map<String, Value> = issues::summary(&self.plan, &self.units, self.ground.as_ref())
            .into_iter()
            .filter(|(label, _, _)| *label != "Items")
            .map(|(label, text, _)| (label.to_lowercase(), text.into()))
            .collect();
        json!({"problems": problems, "summary": summary})
    }

    /// The pill that gives the operator their own plan back.
    pub(crate) fn show_ai(&mut self, ctx: &egui::Context, map: Rect) {
        if self.ai.mine.is_none() {
            return;
        }
        let text = crate::i18n::tr("An AI client replaced your plan.").into_owned();
        let buttons = vec![
            Button::new("Take mine back", Variant::Secondary)
                .icon("rotate-ccw")
                .small(),
            Button::new("Keep this one", Variant::Ghost).small(),
        ];
        match pill(ctx, "ai-plan", map, PILL_BELOW, &text, buttons) {
            Some(0) => self.take_mine_back(),
            Some(_) => self.ai.mine = None,
            None => {}
        }
    }

    pub(crate) fn take_mine_back(&mut self) {
        let Some(mine) = self.ai.mine.take() else { return };
        self.suspend();
        self.dialog = Dialog::None;
        self.plan.load_json(&mine, self.vehicle_type(), self.link.start);
        self.set_tool(Tool::Select);
        self.refresh();
        self.fit_pending = true;
    }
}
