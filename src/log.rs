use std::{collections::BinaryHeap, sync::{Arc, Mutex, mpsc}, time::Instant};

use lasso::{Spur, ThreadedRodeo};

use crate::source::RawLog;

#[derive(Eq, PartialEq, PartialOrd, Ord)]
struct LogEntry {
    timestamp: jiff::Zoned,

    message: Spur,

    /// The start index of labels for this log entry in the arena's global
    /// labels vec.
    labels_start: usize,

    /// The count of labels for this entry in the global arena.
    labels_length: usize,
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

pub struct Arena {
    // A global list of log entries.
    entries: Vec<LogEntry>,

    /// A global list of interned label key/value pairs.
    labels: Vec<(Spur, Spur)>,

    /// A reference to the global interned string store. This should be cloned
    /// locally for efficient access outside of the mutex.
    rodeo: MetaRodeo,

    /// The root view.
    root_view: LogView
}

impl Arena {
    pub fn new() -> Arc<Mutex<Arena>> {
        Arc::new(Mutex::new(Arena {
            entries: Vec::new(),
            labels: Vec::new(),
            rodeo: MetaRodeo {
                messages: Arc::new(ThreadedRodeo::new()),
                label_keys: Arc::new(ThreadedRodeo::new()),
                label_values: Arc::new(ThreadedRodeo::new()),
            },
            root_view: LogView { filters: Vec::new(), children: Vec::new(), entries: Vec::new() },
        }))
    }

    //pub fn resolve_entry(idx: usize) ->
}

struct LogView {
    filters: Vec<()>, // TODO: filter datatype?
    children: Vec<Arc<LogView>>,

    /// A list of indicies included in this view, relative to the arena.
    entries: Vec<usize>,
}

impl LogView {
    pub fn ingest(&mut self, arena: &Arena, entry: &LogEntry, idx: usize) {
        // TODO: apply filters;

        for filter in &self.filters {
            // TODO: if filter is false, return
        }

        // Filters match, append this entry and pass to child views.
        self.entries.push(idx);

        for child in &mut self.children {
            child.ingest(arena, entry, idx);
        }
    }
}

#[derive(Clone)]
struct MetaRodeo {
    messages: Arc<ThreadedRodeo<Spur>>,
    label_keys: Arc<ThreadedRodeo<Spur>>,
    label_values: Arc<ThreadedRodeo<Spur>>,
}

fn ingest(rx: mpsc::Receiver<RawLog>, arena: Arc<Mutex<Arena>>) {
    // TODO: hold received messages as PendingEntries for reordering
    let rodeo = {
        let arena = arena.lock().unwrap();
        arena.rodeo.clone()
    };

    let mut label_pairs: Vec<(Spur, Spur)> = Vec::new();
    for incoming in rx.iter() {
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
        arena.entries.push(LogEntry {
            timestamp: incoming.timestamp,
            message: message,
            labels_start: start,
            labels_length: count,
        });

        label_pairs.clear();
    }
}
