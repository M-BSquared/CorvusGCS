//! JSON helpers with the rules the Python routes follow.

use serde_json::Value;

/// A JSON object, in the order its keys were written.
pub type Map = serde_json::Map<String, Value>;

/// A number for the wire. `NaN` and the infinities become `0.0`, as
/// `_sanitize` in `state_store.py` does, because JSON has no spelling for them.
pub fn num(f: f64) -> Value {
    if f.is_finite() {
        Value::from(f)
    } else {
        Value::from(0.0)
    }
}

/// `{"error": message}`, the body of nearly every refusal.
pub fn error(message: &str) -> Value {
    let mut m = Map::new();
    m.insert("error".into(), Value::from(message));
    Value::Object(m)
}

/// `{"ok": false, "error": message}`, the other refusal shape.
pub fn not_ok(message: &str) -> Value {
    let mut m = Map::new();
    m.insert("ok".into(), Value::from(false));
    m.insert("error".into(), Value::from(message));
    Value::Object(m)
}

/// Why a request body was not a JSON object.
#[derive(Debug, PartialEq, Eq)]
pub enum BodyError {
    /// `json.loads` failed, or the text held `NaN` or an infinity.
    Invalid,
    /// It parsed, but to something other than an object.
    NotObject,
}

impl BodyError {
    /// The text the Python routes answer with (status 400).
    pub fn message(&self) -> &'static str {
        match self {
            BodyError::Invalid => "invalid json",
            BodyError::NotObject => "json payload must be an object",
        }
    }
}

/// A request body as the JSON routes read it: empty means `{}`, anything else
/// must be an object.
pub fn object_from_body(body: &[u8]) -> Result<Map, BodyError> {
    if body.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(BodyError::NotObject),
        Err(_) => Err(BodyError::Invalid),
    }
}

/// `isinstance(v, int) and not isinstance(v, bool)`.
pub fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// A string member, or `None` when it is absent or not a string.
pub fn str_of<'a>(m: &'a Map, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

/// A boolean member, only when it is a real boolean (a string `"true"` is not one).
pub fn bool_of(m: &Map, key: &str) -> Option<bool> {
    m.get(key).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_finite_numbers_are_zero() {
        assert_eq!(num(f64::NAN), json!(0.0));
        assert_eq!(num(f64::INFINITY), json!(0.0));
        assert_eq!(num(f64::NEG_INFINITY), json!(0.0));
        assert_eq!(num(1.5), json!(1.5));
    }

    #[test]
    fn bodies_follow_the_python_rules() {
        assert_eq!(object_from_body(b"").unwrap().len(), 0);
        assert_eq!(object_from_body(br#"{"a": 1}"#).unwrap()["a"], json!(1));
        assert_eq!(object_from_body(b"[1]"), Err(BodyError::NotObject));
        assert_eq!(object_from_body(b"7"), Err(BodyError::NotObject));
        assert_eq!(object_from_body(b"{"), Err(BodyError::Invalid));
        assert_eq!(object_from_body(b"{\"a\": NaN}"), Err(BodyError::Invalid));
        assert_eq!(BodyError::Invalid.message(), "invalid json");
        assert_eq!(BodyError::NotObject.message(), "json payload must be an object");
    }

    #[test]
    fn ints_are_not_floats_or_bools() {
        assert!(is_int(&json!(3)));
        assert!(!is_int(&json!(3.0)));
        assert!(!is_int(&json!(true)));
    }
}
