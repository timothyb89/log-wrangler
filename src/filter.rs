use std::fmt;

use lasso::Spur;
use memchr::memmem;
use regex::Regex;

/// How the filter matches against log content.
#[derive(Clone)]
pub(crate) enum FilterMode {
    /// SIMD-accelerated substring search via `memchr::memmem`.
    Substring(String, memmem::Finder<'static>),
    Regex(Regex),
}

impl FilterMode {
    pub fn substring(pattern: String) -> Self {
        let finder = memmem::Finder::new(pattern.as_bytes()).into_owned();
        Self::Substring(pattern, finder)
    }
}

impl fmt::Debug for FilterMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Substring(pat, _) => f.debug_tuple("Substring").field(pat).finish(),
            Self::Regex(re) => f.debug_tuple("Regex").field(re).finish(),
        }
    }
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
    /// Match entries from a specific source by ID.
    Source(u16),
}

/// A single filter predicate.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub mode: FilterMode,
    pub target: FilterTarget,
    pub inverted: bool,
}

impl Filter {
    /// Test whether a resolved string matches this filter's pattern, ignoring
    /// the `inverted` flag. Useful when callers need to compose raw matches
    /// across multiple fields before applying inversion.
    pub fn raw_matches(&self, text: &str) -> bool {
        match &self.mode {
            FilterMode::Substring(_, finder) => finder.find(text.as_bytes()).is_some(),
            FilterMode::Regex(re) => re.is_match(text),
        }
    }

    /// Test whether a resolved string matches this filter, respecting
    /// the `inverted` flag. Suitable for single-field targets (Message, Label).
    pub fn matches(&self, text: &str) -> bool {
        let raw = self.raw_matches(text);
        if self.inverted { !raw } else { raw }
    }
}
