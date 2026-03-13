use std::cell::Cell;

pub mod json;
pub mod plaintext;

/// Output struct pre-allocated per ingest thread and reused across messages to
/// avoid per-message heap allocation. Classifiers append into `fields`; the
/// caller is responsible for clearing between messages via `clear()`.
pub struct ParseOutput {
    pub message: Option<String>,
    pub level: Option<String>,
    pub timestamp: Option<jiff::Zoned>,
    /// Structured key/value fields extracted by the classifier.
    pub fields: Vec<(String, String)>,
    /// Name of the classifier that matched, set by `ClassifierChain::classify`.
    pub classifier: Option<&'static str>,
}

impl ParseOutput {
    pub fn new() -> Self {
        ParseOutput {
            message: None,
            level: None,
            timestamp: None,
            fields: Vec::new(),
            classifier: None,
        }
    }

    pub fn clear(&mut self) {
        self.message = None;
        self.level = None;
        self.timestamp = None;
        self.fields.clear();
        self.classifier = None;
    }
}

/// A classifier attempts to parse a raw log message string and populate a
/// `ParseOutput`. Returns `true` if the input was recognized, `false` if not.
/// On `false`, the classifier must not have modified `out`.
pub trait Classifier: Send + Sync {
    fn classify(&self, input: &str, out: &mut ParseOutput) -> bool;
    /// Short identifier written as the `_classifier` structured field on match.
    fn name(&self) -> &'static str;
}

/// A chain of classifiers tried in order. The first to return `true` wins.
/// Maintains a `last_hit` index to short-circuit the search on stable-format
/// streams; falls back to a full scan on a miss.
pub struct ClassifierChain {
    classifiers: Vec<Box<dyn Classifier>>,
    last_hit: Cell<usize>,
}

impl ClassifierChain {
    pub fn new(classifiers: Vec<Box<dyn Classifier>>) -> Self {
        ClassifierChain {
            classifiers,
            last_hit: Cell::new(0),
        }
    }

    /// Try classifiers in order, returning `true` if any matched.
    /// `out` is cleared before each failed attempt so partial output is never
    /// visible to the caller on a miss.
    pub fn classify(&self, input: &str, out: &mut ParseOutput) -> bool {
        if self.classifiers.is_empty() {
            return false;
        }

        let start = self.last_hit.get();
        if self.classifiers[start].classify(input, out) {
            out.classifier = Some(self.classifiers[start].name());
            return true;
        }
        out.clear();

        for (i, classifier) in self.classifiers.iter().enumerate() {
            if i == start {
                continue;
            }
            if classifier.classify(input, out) {
                self.last_hit.set(i);
                out.classifier = Some(classifier.name());
                return true;
            }
            out.clear();
        }

        false
    }
}

/// Map common level strings to canonical lowercase forms.
/// Returns the input unchanged for unrecognized values.
pub fn normalize_level(s: &str) -> &str {
    match s {
        "TRACE" | "trace" | "trc" | "TRC" => "trace",
        "DEBUG" | "debug" | "dbg" | "DBG" => "debug",
        "INFO" | "info" | "inf" | "INF" | "INFORMATION" | "information" | "NOTICE" | "notice" => {
            "info"
        }
        "WARN" | "warn" | "WARNING" | "warning" => "warning",
        "ERROR" | "error" | "err" | "ERR" | "FATAL" | "fatal" | "CRIT" | "crit"
        | "CRITICAL" | "critical" => "error",
        other => other,
    }
}
