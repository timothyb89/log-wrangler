use std::sync::OnceLock;

use regex::Regex;

use super::{Classifier, ParseOutput};

/// Classifier for the default `journalctl` plaintext output format:
/// `MMM [D]D HH:MM:SS hostname unit[pid]: message`
///
/// Parses the syslog-style timestamp into a `jiff::Zoned` using the system
/// timezone (syslog timestamps are local time). Emits `hostname`, `unit`, and
/// optionally `pid` as structured fields. Sets `message` to the text after the
/// colon so an enclosing `Encapsulating` wrapper can further classify it.
pub struct SystemdClassifier;

fn parse_syslog_month(s: &str) -> Option<i8> {
    match s {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn parse_syslog_timestamp(month_str: &str, day_str: &str, time_str: &str) -> Option<jiff::Zoned> {
    let month = parse_syslog_month(month_str)?;
    let day: i8 = day_str.trim().parse().ok()?;

    let mut t = time_str.splitn(3, ':');
    let hour: i8 = t.next()?.parse().ok()?;
    let minute: i8 = t.next()?.parse().ok()?;
    let second: i8 = t.next()?.parse().ok()?;

    // Syslog format has no year. If the logged month is ahead of the current
    // month the entry is almost certainly from the previous year.
    let now = jiff::Zoned::now();
    let year = if month > now.month() {
        now.year() - 1
    } else {
        now.year()
    };

    let date = jiff::civil::Date::new(year, month, day).ok()?;
    let time = jiff::civil::Time::new(hour, minute, second, 0).ok()?;
    let dt = date.to_datetime(time);
    dt.to_zoned(jiff::tz::TimeZone::system()).ok()
}

impl Classifier for SystemdClassifier {
    fn name(&self) -> &'static str {
        "journald"
    }

    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            // Groups: (1)month (2)day (3)HH:MM:SS (4)hostname (5)unit (6)pid? (7)message
            Regex::new(
                r"^(\w{3})\s+(\d{1,2})\s+(\d{2}:\d{2}:\d{2})\s+(\S+)\s+([^\[:\s][^:]*?)(?:\[(\d+)\])?:\s*(.*)",
            )
            .unwrap()
        });

        let caps = match re.captures(input) {
            Some(c) => c,
            None => return false,
        };

        out.timestamp = parse_syslog_timestamp(
            caps.get(1).map_or("", |m| m.as_str()),
            caps.get(2).map_or("", |m| m.as_str()),
            caps.get(3).map_or("", |m| m.as_str()),
        );

        if let Some(m) = caps.get(4) {
            out.fields.push(("hostname".to_string(), m.as_str().to_string()));
        }
        if let Some(m) = caps.get(5) {
            let unit = m.as_str().trim();
            if !unit.is_empty() {
                out.fields.push(("unit".to_string(), unit.to_string()));
            }
        }
        if let Some(m) = caps.get(6) {
            out.fields.push(("pid".to_string(), m.as_str().to_string()));
        }

        out.message = Some(caps.get(7).map_or("", |m| m.as_str()).to_string());
        true
    }
}

/// Heuristic classifier for plaintext log lines. Scans the first 200 bytes for
/// a recognizable log level keyword. Does not extract a message (the whole line
/// is used as-is) or a timestamp (the outer source timestamp is kept).
pub struct GenericClassifier;

impl Classifier for GenericClassifier {
    fn name(&self) -> &'static str {
        "generic"
    }

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
    fn name(&self) -> &'static str {
        "regex"
    }

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
