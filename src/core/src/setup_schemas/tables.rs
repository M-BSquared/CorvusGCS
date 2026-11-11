//! The static tables of the schemas (option lists, driver catalogues, hardware
//! presets), kept as data beside the code. Each file under `data/` is exported
//! from the Python module that owns the table by
//! `python/tools/dump_setup_schema_cases.py tables`, which also checks that the
//! file is current (`--check`).

use std::sync::LazyLock;

use serde_json::Value;

fn parse(name: &str, text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("setup_schemas/data/{name}: {e}"))
}

/// A table of a data file, by its name in the Python module.
pub struct Table(&'static Value);

impl Table {
    pub fn get(&self, name: &str) -> &'static Value {
        self.0
            .get(name)
            .unwrap_or_else(|| panic!("setup_schemas/data: no table {name}"))
    }

    /// An option list (`[{"value": n, "label": text}, ...]`).
    pub fn options(&self, name: &str) -> Vec<Value> {
        self.get(name).as_array().cloned().unwrap_or_default()
    }

    pub fn array(&self, name: &str) -> &'static [Value] {
        self.get(name).as_array().map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn text(&self, section: &str, key: &str) -> &'static str {
        self.get(section).get(key).and_then(Value::as_str).unwrap_or("")
    }
}

static SAFETY_DATA: LazyLock<Value> = LazyLock::new(|| parse("safety.json", include_str!("data/safety.json")));

/// The tables of `safety_config.py`.
pub fn safety() -> Table {
    Table(&SAFETY_DATA)
}

static ARDUPILOT_SAFETY_DATA: LazyLock<Value> =
    LazyLock::new(|| parse("ardupilot_safety.json", include_str!("data/ardupilot_safety.json")));

/// The tables of `ardupilot_safety.py`.
pub fn ardupilot_safety() -> Table {
    Table(&ARDUPILOT_SAFETY_DATA)
}

static MOUNTING_DATA: LazyLock<Value> = LazyLock::new(|| parse("mounting.json", include_str!("data/mounting.json")));

/// The tables of `mounting_config.py` and `ardupilot_mounting.py`.
pub fn mounting() -> Table {
    Table(&MOUNTING_DATA)
}

static MOTORS_DATA: LazyLock<Value> = LazyLock::new(|| parse("motors.json", include_str!("data/motors.json")));

/// The tables of `motor_config.py` (`"px4"`) and `ardupilot_motors.py` (`"ardupilot"`).
pub fn motors(stack: &str) -> Table {
    Table(&MOTORS_DATA[stack])
}
