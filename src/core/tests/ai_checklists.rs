//! The checklist block is kept as `checklists.py` keeps it
//! (`python/tools/dump_ai_cases.py checklists`).

use corvus_core::plugins::checklists::coerce;
use serde_json::Value;

#[test]
fn every_input_is_coerced_as_python_coerces_it() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("data/ai_checklists.json")).unwrap();
    assert!(cases.len() > 25);
    for case in &cases {
        let got = coerce(&case["in"]).map(Value::Object).unwrap_or(Value::Null);
        assert_eq!(got, case["out"], "input {}", case["in"]);
    }
}
