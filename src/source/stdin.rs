use std::collections::HashMap;
use std::io::BufRead;
use std::sync::mpsc;

use serde::Deserialize;

use super::RawLog;

#[derive(Deserialize)]
struct LogCliEntry {
    #[serde(default)]
    labels: HashMap<String, String>,
    line: String,
    timestamp: String,
}

/// Read JSONL log entries from stdin and send them as RawLogs.
///
/// Each line is tried as logcli-format JSON (`{"labels":..., "line":..., "timestamp":...}`).
/// Lines that fail to parse as JSON are treated as plain text with the current
/// timestamp and no labels.
pub fn read_stdin(tx: mpsc::Sender<RawLog>) {
    let stdin = std::io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.is_empty() {
            continue;
        }

        let raw_log = match serde_json::from_str::<LogCliEntry>(&line) {
            Ok(entry) => {
                let timestamp = entry
                    .timestamp
                    .parse::<jiff::Zoned>()
                    .or_else(|_| entry.timestamp.parse::<jiff::Timestamp>().map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC)))
                    .unwrap_or_else(|_| jiff::Zoned::now());

                RawLog {
                    timestamp,
                    message: entry.line,
                    labels: entry.labels,
                }
            }
            Err(_) => RawLog {
                timestamp: jiff::Zoned::now(),
                message: line,
                labels: HashMap::new(),
            },
        };

        if tx.send(raw_log).is_err() {
            break;
        }
    }
}
