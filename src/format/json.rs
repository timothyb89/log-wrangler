use serde_json::Value;

use super::{Classifier, ParseOutput};

fn json_value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// A configurable classifier for flat JSON log formats. Tries to parse the
/// input as a JSON object and extracts level, message, and structured fields
/// using the configured key name lists.
pub struct JsonClassifier {
    pub name: &'static str,
    pub level_keys: &'static [&'static str],
    pub message_keys: &'static [&'static str],
    /// Keys to exclude from structured fields (should include level_keys,
    /// message_keys, and any timestamp/metadata keys for this format).
    pub skip_keys: &'static [&'static str],
}

impl Classifier for JsonClassifier {
    fn name(&self) -> &'static str {
        self.name
    }

    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        let map = match serde_json::from_str::<Value>(input) {
            Ok(Value::Object(m)) => m,
            _ => return false,
        };

        for key in self.level_keys {
            if let Some(v) = map.get(*key) {
                out.level = Some(json_value_to_string(v));
                break;
            }
        }

        for key in self.message_keys {
            if let Some(v) = map.get(*key) {
                out.message = Some(json_value_to_string(v));
                break;
            }
        }

        for (k, v) in &map {
            if self.skip_keys.contains(&k.as_str()) {
                continue;
            }
            out.fields.push((k.clone(), json_value_to_string(v)));
        }

        true
    }
}

/// Classifier for Rust's `tracing_subscriber` JSON format, which nests all
/// application fields under a `fields` sub-object:
/// `{"timestamp":"...","level":"INFO","fields":{"message":"...","key":"val"},"target":"..."}`
pub struct RustTracingClassifier;

impl Classifier for RustTracingClassifier {
    fn name(&self) -> &'static str {
        "rust-tracing"
    }

    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        let map = match serde_json::from_str::<Value>(input) {
            Ok(Value::Object(m)) => m,
            _ => return false,
        };

        // Require a `fields` object — distinguishes tracing from generic JSON.
        let fields_obj = match map.get("fields").and_then(|v| v.as_object()) {
            Some(f) => f,
            None => return false,
        };

        if let Some(v) = map.get("level") {
            out.level = Some(json_value_to_string(v));
        }

        if let Some(v) = fields_obj.get("message") {
            out.message = Some(json_value_to_string(v));
        }

        for (k, v) in fields_obj {
            if k == "message" {
                continue;
            }
            out.fields.push((k.clone(), json_value_to_string(v)));
        }

        if let Some(v) = map.get("target") {
            out.fields.push(("target".to_string(), json_value_to_string(v)));
        }
        if let Some(span) = map.get("span").and_then(|v| v.as_object()) {
            if let Some(name) = span.get("name") {
                out.fields.push(("span".to_string(), json_value_to_string(name)));
            }
        }

        true
    }
}

/// Classifier for `journalctl -o json` output. Extracts `MESSAGE` as the inner
/// message, `PRIORITY` (syslog integer) as log level, and
/// `__REALTIME_TIMESTAMP` (microseconds since epoch) as the authoritative
/// timestamp. Only a small whitelist of fields are kept as structured fields.
pub struct JournaldJsonClassifier;

/// Structured fields to retain from journald JSON output. All other fields are
/// dropped to avoid the high noise of journald's many metadata keys.
const JOURNALD_KEEP_FIELDS: &[&str] = &[
    "_HOSTNAME",
    "SYSLOG_IDENTIFIER",
    "_SYSTEMD_UNIT",
    "_PID",
];

fn journald_priority_to_level(priority: &str) -> &'static str {
    match priority.trim() {
        "0" | "1" | "2" | "3" => "error",
        "4" => "warning",
        "5" | "6" => "info",
        "7" => "debug",
        _ => "info",
    }
}

impl Classifier for JournaldJsonClassifier {
    fn name(&self) -> &'static str {
        "journald-json"
    }

    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        let map = match serde_json::from_str::<Value>(input) {
            Ok(Value::Object(m)) => m,
            _ => return false,
        };

        // Require MESSAGE — the distinguishing field for journald JSON.
        let message = match map.get("MESSAGE") {
            Some(v) => json_value_to_string(v),
            None => return false,
        };
        out.message = Some(message);

        if let Some(v) = map.get("PRIORITY") {
            let p = json_value_to_string(v);
            out.level = Some(journald_priority_to_level(&p).to_string());
        }

        if let Some(v) = map.get("__REALTIME_TIMESTAMP") {
            let ts_str = json_value_to_string(v);
            if let Ok(micros) = ts_str.trim().parse::<i64>() {
                if let Ok(ts) = jiff::Timestamp::from_microsecond(micros) {
                    out.timestamp = Some(ts.to_zoned(jiff::tz::TimeZone::UTC));
                }
            }
        }

        for field in JOURNALD_KEEP_FIELDS {
            if let Some(v) = map.get(*field) {
                out.fields.push((field.to_string(), json_value_to_string(v)));
            }
        }

        true
    }
}

// --- Format presets ---

/// Default JSON classifier: matches the existing log-wrangler behavior.
pub fn default() -> JsonClassifier {
    JsonClassifier {
        name: "json",
        level_keys: &["level"],
        message_keys: &["message", "msg"],
        skip_keys: &["level", "message", "msg", "timestamp", "time", "ts"],
    }
}

/// Go `slog` JSON format (`{"time":"...","level":"INFO","msg":"...","key":"val"}`).
pub fn slog() -> JsonClassifier {
    JsonClassifier {
        name: "slog",
        level_keys: &["level"],
        message_keys: &["msg"],
        skip_keys: &["level", "msg", "time", "source"],
    }
}

/// Rust `tracing_subscriber` JSON format.
pub fn rust_tracing() -> RustTracingClassifier {
    RustTracingClassifier
}

/// `journalctl -o json` format.
pub fn journald_json() -> JournaldJsonClassifier {
    JournaldJsonClassifier
}
