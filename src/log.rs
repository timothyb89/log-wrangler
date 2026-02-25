use std::{
    sync::{mpsc, Arc, Mutex},
    time::Instant,
};

use lasso::{Spur, ThreadedRodeo};

use crate::filter::Filter;
use crate::source::RawLog;

/// Path from root to the currently active LogView.
/// Empty = root. [0, 2] = root's child 0, then that node's child 2.
pub(crate) type ViewPath = Vec<usize>;

#[derive(Eq, PartialEq, PartialOrd, Ord)]
pub(crate) struct LogEntry {
    pub timestamp: jiff::Zoned,
    pub message: Spur,

    /// The start index of labels for this log entry in the arena's global
    /// labels vec.
    pub labels_start: usize,

    /// The count of labels for this entry in the global arena.
    pub labels_length: usize,

    /// Extracted log level from nested JSON message (e.g. "debug", "info").
    pub level: Option<Spur>,

    /// Extracted inner message from nested JSON message.
    pub inner_message: Option<Spur>,

    /// Start index of structured fields in the arena's structured_fields vec.
    pub structured_fields_start: usize,

    /// Count of structured fields for this entry.
    pub structured_fields_length: usize,
}

struct PendingEntry {
    inner: LogEntry,
    received: Instant,
}

impl Ord for PendingEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.received.cmp(&self.received)
    }
}

impl PartialOrd for PendingEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.received.partial_cmp(&other.received)
    }
}

impl PartialEq for PendingEntry {
    fn eq(&self, other: &Self) -> bool {
        self.received == other.received
    }
}

impl Eq for PendingEntry {}

pub(crate) struct Arena {
    /// A global list of log entries.
    pub entries: Vec<LogEntry>,

    /// A global list of interned label key/value pairs.
    pub labels: Vec<(Spur, Spur)>,

    /// A global list of structured fields parsed from nested JSON messages.
    pub structured_fields: Vec<(Spur, Spur)>,

    /// A reference to the global interned string store. This should be cloned
    /// locally for efficient access outside of the mutex.
    pub rodeo: MetaRodeo,

    /// The root view.
    pub root_view: LogView,
}

/// A resolved log entry with strings ready for display.
pub(crate) struct ResolvedEntry<'a> {
    pub timestamp: &'a jiff::Zoned,
    pub message: &'a str,
    pub labels: Vec<(&'a str, &'a str)>,
    /// Extracted log level from nested JSON (e.g. "debug", "info").
    pub level: Option<&'a str>,
    /// Extracted inner message from nested JSON.
    pub inner_message: Option<&'a str>,
    /// Key-value pairs parsed from nested JSON (excluding level/message/timestamp).
    pub structured_fields: Vec<(&'a str, &'a str)>,
}

impl Arena {
    pub fn new() -> Arc<Mutex<Arena>> {
        Arc::new(Mutex::new(Arena {
            entries: Vec::new(),
            labels: Vec::new(),
            structured_fields: Vec::new(),
            rodeo: MetaRodeo {
                messages: Arc::new(ThreadedRodeo::new()),
                label_keys: Arc::new(ThreadedRodeo::new()),
                label_values: Arc::new(ThreadedRodeo::new()),
            },
            root_view: LogView {
                filters: Vec::new(),
                children: Vec::new(),
                entries: Vec::new(),
            },
        }))
    }

    /// Navigate to a LogView by path.
    pub fn view_at(&self, path: &[usize]) -> &LogView {
        let mut current = &self.root_view;
        for &idx in path {
            current = &current.children[idx];
        }
        current
    }

    /// Navigate to a LogView by path (mutable).
    pub fn view_at_mut(&mut self, path: &[usize]) -> &mut LogView {
        let mut current = &mut self.root_view;
        for &idx in path {
            current = &mut current.children[idx];
        }
        current
    }

    /// Resolve a LogEntry's interned fields into displayable strings.
    pub fn resolve_entry(&self, idx: usize) -> ResolvedEntry<'_> {
        let entry = &self.entries[idx];
        let message = self.rodeo.messages.resolve(&entry.message);
        let timestamp = &entry.timestamp;
        let labels: Vec<(&str, &str)> = (0..entry.labels_length)
            .map(|i| {
                let (k, v) = &self.labels[entry.labels_start + i];
                (
                    self.rodeo.label_keys.resolve(k),
                    self.rodeo.label_values.resolve(v),
                )
            })
            .collect();
        let level = entry.level.map(|s| self.rodeo.label_values.resolve(&s));
        let inner_message = entry.inner_message.map(|s| self.rodeo.messages.resolve(&s));
        let structured_fields: Vec<(&str, &str)> = (0..entry.structured_fields_length)
            .map(|i| {
                let (k, v) = &self.structured_fields[entry.structured_fields_start + i];
                (
                    self.rodeo.label_keys.resolve(k),
                    self.rodeo.label_values.resolve(v),
                )
            })
            .collect();

        ResolvedEntry {
            timestamp,
            message,
            labels,
            level,
            inner_message,
            structured_fields,
        }
    }
}

pub(crate) struct LogView {
    pub filters: Vec<Filter>,
    pub children: Vec<LogView>,

    /// A list of indices included in this view, relative to the arena.
    pub entries: Vec<usize>,
}

impl LogView {
    /// Ingest a new entry, applying filters. Takes arena fields separately to
    /// avoid borrow conflicts when called from the ingest function.
    pub fn ingest(
        &mut self,
        rodeo: &MetaRodeo,
        labels: &[(Spur, Spur)],
        entry: &LogEntry,
        idx: usize,
    ) {
        // Apply filters — all must match for the entry to be included.
        for filter in &self.filters {
            let matches = match &filter.target {
                crate::filter::FilterTarget::Message => {
                    let msg = rodeo.messages.resolve(&entry.message);
                    filter.matches(msg)
                }
                crate::filter::FilterTarget::Any => {
                    let msg = rodeo.messages.resolve(&entry.message);
                    if filter.matches(msg) {
                        true
                    } else {
                        (0..entry.labels_length).any(|i| {
                            let (_, v) = &labels[entry.labels_start + i];
                            let val = rodeo.label_values.resolve(v);
                            filter.matches(val)
                        })
                    }
                }
                crate::filter::FilterTarget::Label(key_spur) => {
                    (0..entry.labels_length).any(|i| {
                        let (k, v) = &labels[entry.labels_start + i];
                        if k == key_spur {
                            let val = rodeo.label_values.resolve(v);
                            filter.matches(val)
                        } else {
                            false
                        }
                    })
                }
            };

            if !matches {
                return;
            }
        }

        // Filters match, append this entry and pass to child views.
        self.entries.push(idx);

        for child in &mut self.children {
            child.ingest(rodeo, labels, entry, idx);
        }
    }
}

#[derive(Clone)]
pub(crate) struct MetaRodeo {
    pub messages: Arc<ThreadedRodeo<Spur>>,
    pub label_keys: Arc<ThreadedRodeo<Spur>>,
    pub label_values: Arc<ThreadedRodeo<Spur>>,
}

fn json_value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Fields to skip when collecting structured fields from nested JSON.
const NESTED_JSON_SKIP_FIELDS: &[&str] = &["level", "message", "msg", "timestamp", "time", "ts"];

pub(crate) fn ingest(rx: mpsc::Receiver<RawLog>, arena: Arc<Mutex<Arena>>) {
    // TODO: hold received messages as PendingEntries for reordering
    let rodeo = {
        let arena = arena.lock().unwrap();
        arena.rodeo.clone()
    };

    let mut label_pairs: Vec<(Spur, Spur)> = Vec::new();
    let mut sf_pairs: Vec<(Spur, Spur)> = Vec::new();
    for incoming in rx.iter() {
        // Try to parse the message as nested JSON to extract structured fields.
        let (level, inner_message) =
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(&incoming.message)
            {
                let level = map
                    .get("level")
                    .map(json_value_to_string)
                    .map(|s| rodeo.label_values.get_or_intern(s));

                let inner_msg = map
                    .get("message")
                    .or_else(|| map.get("msg"))
                    .map(json_value_to_string)
                    .map(|s| rodeo.messages.get_or_intern(s));

                for (k, v) in &map {
                    if NESTED_JSON_SKIP_FIELDS.contains(&k.as_str()) {
                        continue;
                    }
                    let key = rodeo.label_keys.get_or_intern(k.as_str());
                    let val = rodeo.label_values.get_or_intern(json_value_to_string(v));
                    sf_pairs.push((key, val));
                }

                (level, inner_msg)
            } else {
                (None, None)
            };

        let message = rodeo.messages.get_or_intern(incoming.message);
        for (raw_key, raw_value) in incoming.labels {
            let k = rodeo.label_keys.get_or_intern(raw_key);
            let v = rodeo.label_values.get_or_intern(raw_value);

            label_pairs.push((k, v));
        }

        let mut arena = arena.lock().unwrap();
        let start = arena.labels.len();

        let count = label_pairs.len();
        arena.labels.extend(label_pairs.drain(0..));

        let sf_start = arena.structured_fields.len();
        let sf_count = sf_pairs.len();
        arena.structured_fields.extend(sf_pairs.drain(0..));

        let entry = LogEntry {
            timestamp: incoming.timestamp,
            message,
            labels_start: start,
            labels_length: count,
            level,
            inner_message,
            structured_fields_start: sf_start,
            structured_fields_length: sf_count,
        };

        let idx = arena.entries.len();
        arena.entries.push(entry);

        // Propagate to the view tree. Deref the guard once so Rust can
        // split the borrow across distinct Arena fields.
        let a = &mut *arena;
        let entry_ref = &a.entries[idx];
        a.root_view.ingest(&a.rodeo, &a.labels, entry_ref, idx);

        label_pairs.clear();
    }
}
