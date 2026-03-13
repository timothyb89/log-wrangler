use std::{
    collections::BinaryHeap,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use lasso::{Spur, ThreadedRodeo};

use crate::filter::Filter;
use crate::format::{normalize_level, ClassifierChain, ParseOutput};
use crate::source::SourceMessage;

/// Path from root to the currently active LogView.
/// Empty = root. [0, 2] = root's child 0, then that node's child 2.
pub(crate) type ViewPath = Vec<usize>;

#[derive(Eq, PartialEq, PartialOrd, Ord)]
pub(crate) struct LogEntry {
    pub timestamp: jiff::Zoned,
    pub message: Spur,

    /// Which source produced this entry (index into Arena.source_names).
    pub source_id: u16,

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
        // Reversed for min-heap: oldest log timestamp pops first.
        other.inner.timestamp.cmp(&self.inner.timestamp)
    }
}

impl PartialOrd for PendingEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for PendingEntry {
    fn eq(&self, other: &Self) -> bool {
        self.inner.timestamp == other.inner.timestamp
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

    /// Maps source_id -> display name.
    pub source_names: Vec<String>,
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
            source_names: Vec::new(),
        }))
    }

    /// Clear all entries, labels, structured fields, and the view tree.
    /// Replaces the rodeos with fresh instances so new entries use clean interners.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.labels.clear();
        self.structured_fields.clear();
        self.rodeo = MetaRodeo {
            messages: Arc::new(ThreadedRodeo::new()),
            label_keys: Arc::new(ThreadedRodeo::new()),
            label_values: Arc::new(ThreadedRodeo::new()),
        };
        self.root_view = LogView {
            filters: Vec::new(),
            children: Vec::new(),
            entries: Vec::new(),
        };
    }

    /// Remove all entries belonging to a specific source from every view.
    /// Dead entries remain in the flat vecs as unreferenced garbage.
    pub fn clear_source(&mut self, source_id: u16) {
        fn clear_view(view: &mut LogView, entries: &[LogEntry], source_id: u16) {
            view.entries.retain(|&idx| entries[idx].source_id != source_id);
            for child in &mut view.children {
                clear_view(child, entries, source_id);
            }
        }
        clear_view(&mut self.root_view, &self.entries, source_id);
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
                    let raw = filter.raw_matches(msg)
                        || (0..entry.labels_length).any(|i| {
                            let (_, v) = &labels[entry.labels_start + i];
                            let val = rodeo.label_values.resolve(v);
                            filter.raw_matches(val)
                        });
                    if filter.inverted { !raw } else { raw }
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
                crate::filter::FilterTarget::Source(sid) => {
                    let matches = entry.source_id == *sid;
                    if filter.inverted { !matches } else { matches }
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

/// Parse a RawLog into interned fields and store labels/structured_fields in the
/// arena. Returns a LogEntry ready for insertion into `arena.entries`.
///
/// `chain` is the classifier for this source (looked up by source_id). `out` is
/// a pre-allocated scratch buffer reused across calls; it is cleared on return.
fn parse_raw_log(
    incoming: crate::source::RawLog,
    rodeo: &MetaRodeo,
    arena: &Arc<Mutex<Arena>>,
    label_pairs: &mut Vec<(Spur, Spur)>,
    sf_pairs: &mut Vec<(Spur, Spur)>,
    chain: Option<&ClassifierChain>,
    out: &mut ParseOutput,
) -> LogEntry {
    // Run the classifier for this source (if any).
    if let Some(chain) = chain {
        chain.classify(&incoming.message, out);
    }

    // Child timestamp wins over the outer source timestamp (per encapsulation
    // field precedence rules).
    let timestamp = out.timestamp.take().unwrap_or(incoming.timestamp);

    let level = out
        .level
        .take()
        .map(|s| rodeo.label_values.get_or_intern(normalize_level(&s)));

    let inner_message = out
        .message
        .take()
        .map(|s| rodeo.messages.get_or_intern(s));

    for (k, v) in out.fields.drain(..) {
        let key = rodeo.label_keys.get_or_intern(&k);
        let val = rodeo.label_values.get_or_intern(&v);
        sf_pairs.push((key, val));
    }

    if let Some(classifier_name) = out.classifier.take() {
        let key = rodeo.label_keys.get_or_intern("_classifier");
        let val = rodeo.label_values.get_or_intern(classifier_name);
        sf_pairs.push((key, val));
    }
    // out is now fully consumed; no explicit clear needed.

    let message = rodeo.messages.get_or_intern(incoming.message);
    for (raw_key, raw_value) in incoming.labels {
        let k = rodeo.label_keys.get_or_intern(raw_key);
        let v = rodeo.label_values.get_or_intern(raw_value);
        label_pairs.push((k, v));
    }

    // Lock arena briefly to store labels and structured fields (indices are stable).
    let mut arena = arena.lock().unwrap();
    let start = arena.labels.len();
    let count = label_pairs.len();
    arena.labels.extend(label_pairs.drain(0..));

    let sf_start = arena.structured_fields.len();
    let sf_count = sf_pairs.len();
    arena.structured_fields.extend(sf_pairs.drain(0..));

    LogEntry {
        timestamp,
        message,
        source_id: incoming.source_id,
        labels_start: start,
        labels_length: count,
        level,
        inner_message,
        structured_fields_start: sf_start,
        structured_fields_length: sf_count,
    }
}

/// Insert a LogEntry into the arena and propagate through the view tree.
fn commit_entry(arena: &mut Arena, entry: LogEntry) {
    let idx = arena.entries.len();
    arena.entries.push(entry);
    let a = &mut *arena;
    let entry_ref = &a.entries[idx];
    a.root_view.ingest(&a.rodeo, &a.labels, entry_ref, idx);
}

/// Flush all pending entries whose `received` time exceeds the buffer duration.
fn flush_expired(heap: &mut BinaryHeap<PendingEntry>, buffer_duration: Duration, arena: &Arc<Mutex<Arena>>) {
    let now = Instant::now();
    let mut batch: Vec<LogEntry> = Vec::new();

    while let Some(oldest) = heap.peek() {
        if now.duration_since(oldest.received) >= buffer_duration {
            batch.push(heap.pop().unwrap().inner);
        } else {
            break;
        }
    }

    if batch.is_empty() {
        return;
    }

    // Batch is popped in min-timestamp order from the heap, so already sorted.
    let mut arena = arena.lock().unwrap();
    for entry in batch {
        commit_entry(&mut arena, entry);
    }
}

/// Flush all remaining pending entries (e.g. on channel disconnect).
fn flush_all(heap: &mut BinaryHeap<PendingEntry>, arena: &Arc<Mutex<Arena>>) {
    let mut batch: Vec<LogEntry> = Vec::new();
    while let Some(pending) = heap.pop() {
        batch.push(pending.inner);
    }

    if batch.is_empty() {
        return;
    }

    let mut arena = arena.lock().unwrap();
    for entry in batch {
        commit_entry(&mut arena, entry);
    }
}

pub(crate) fn ingest(
    rx: mpsc::Receiver<SourceMessage>,
    arena: Arc<Mutex<Arena>>,
    reorder_buffer: Option<Duration>,
    classifiers: Vec<ClassifierChain>,
    default_chain: ClassifierChain,
) {
    let rodeo = {
        let arena = arena.lock().unwrap();
        arena.rodeo.clone()
    };

    let mut label_pairs: Vec<(Spur, Spur)> = Vec::new();
    let mut sf_pairs: Vec<(Spur, Spur)> = Vec::new();
    let mut parse_out = ParseOutput::new();

    let get_chain = |source_id: u16| -> &ClassifierChain {
        classifiers.get(source_id as usize).unwrap_or(&default_chain)
    };

    if let Some(buffer_duration) = reorder_buffer {
        // Buffered path: hold messages and flush sorted by timestamp.
        let check_interval = Duration::from_millis(250);
        let mut heap: BinaryHeap<PendingEntry> = BinaryHeap::new();

        loop {
            match rx.recv_timeout(check_interval) {
                Ok(SourceMessage::Reset { source_id }) => {
                    // Discard buffered entries for this source, then clear arena.
                    let remaining: Vec<PendingEntry> = heap.drain()
                        .filter(|e| e.inner.source_id != source_id)
                        .collect();
                    heap.extend(remaining);

                    let mut arena = arena.lock().unwrap();
                    arena.clear_source(source_id);
                }
                Ok(SourceMessage::Log(raw)) => {
                    let chain = get_chain(raw.source_id);
                    let entry = parse_raw_log(raw, &rodeo, &arena, &mut label_pairs, &mut sf_pairs, Some(chain), &mut parse_out);
                    heap.push(PendingEntry {
                        inner: entry,
                        received: Instant::now(),
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    flush_all(&mut heap, &arena);
                    return;
                }
            }

            flush_expired(&mut heap, buffer_duration, &arena);
        }
    } else {
        // Unbuffered path: insert directly in receipt order.
        for msg in rx.iter() {
            match msg {
                SourceMessage::Reset { source_id } => {
                    let mut arena = arena.lock().unwrap();
                    arena.clear_source(source_id);
                }
                SourceMessage::Log(raw) => {
                    let chain = get_chain(raw.source_id);
                    let entry = parse_raw_log(raw, &rodeo, &arena, &mut label_pairs, &mut sf_pairs, Some(chain), &mut parse_out);
                    let mut arena = arena.lock().unwrap();
                    commit_entry(&mut arena, entry);
                }
            }
        }
    }
}
