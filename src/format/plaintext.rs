use std::sync::OnceLock;

use regex::Regex;

use super::{Classifier, ParseOutput};

/// Heuristic classifier for plaintext log lines. Scans the first 200 bytes for
/// a recognizable log level keyword. Does not extract a message (the whole line
/// is used as-is) or a timestamp (the outer source timestamp is kept).
pub struct GenericClassifier;

impl Classifier for GenericClassifier {
    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        static LEVEL_RE: OnceLock<Regex> = OnceLock::new();
        let re = LEVEL_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(TRACE|DEBUG|INFO|NOTICE|WARN(?:ING)?|ERROR|FATAL|CRIT(?:ICAL)?)\b")
                .unwrap()
        });

        let probe = &input[..input.len().min(200)];
        if let Some(cap) = re.find(probe) {
            out.level = Some(cap.as_str().to_string());
            return true;
        }

        false
    }
}

/// User-supplied regex classifier. The regex must contain named capture groups;
/// `timestamp` and `level` are reserved names mapped to the corresponding
/// `ParseOutput` fields. An optional `message` group, if present, is used as
/// the inner message (otherwise the whole line is used). All other named groups
/// become structured fields.
pub struct RegexClassifier {
    pub pattern: Regex,
}

impl Classifier for RegexClassifier {
    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        let caps = match self.pattern.captures(input) {
            Some(c) => c,
            None => return false,
        };

        if let Some(m) = caps.name("timestamp") {
            let s = m.as_str();
            let parsed = s
                .parse::<jiff::Zoned>()
                .ok()
                .or_else(|| {
                    s.parse::<jiff::Timestamp>()
                        .ok()
                        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC))
                });
            out.timestamp = parsed;
        }

        if let Some(m) = caps.name("level") {
            out.level = Some(m.as_str().to_string());
        }

        if let Some(m) = caps.name("message") {
            out.message = Some(m.as_str().to_string());
        }

        for name in self.pattern.capture_names().flatten() {
            if matches!(name, "timestamp" | "level" | "message") {
                continue;
            }
            if let Some(m) = caps.name(name) {
                out.fields.push((name.to_string(), m.as_str().to_string()));
            }
        }

        true
    }
}
