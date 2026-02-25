use lasso::Spur;
use regex::Regex;

/// How the filter matches against log content.
#[derive(Clone, Debug)]
pub(crate) enum FilterMode {
    Substring(String),
    Regex(Regex),
}

/// What field the filter applies to.
#[derive(Clone, Debug)]
pub(crate) enum FilterTarget {
    /// Match against the message text.
    Message,
    /// Match against a specific label value, identified by its interned key.
    Label(Spur),
    /// Match against any field (message + all label values).
    Any,
}

/// A single filter predicate.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub mode: FilterMode,
    pub target: FilterTarget,
    pub inverted: bool,
}

impl Filter {
    /// Test whether a resolved string matches this filter.
    pub fn matches(&self, text: &str) -> bool {
        let raw_match = match &self.mode {
            FilterMode::Substring(pat) => text.contains(pat.as_str()),
            FilterMode::Regex(re) => re.is_match(text),
        };
        if self.inverted {
            !raw_match
        } else {
            raw_match
        }
    }
}
