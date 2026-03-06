use crate::filter::{Filter, FilterMode, FilterTarget};
use crate::log::{Arena, LogView, ViewPath};
use crate::source::loki::LokiSourceParams;

use super::{App, Direction, FilterEntryMode, ScrollState, ToolbarMode};

impl App {
    pub(super) fn submit_query(&mut self) {
        if self.query_input.is_empty() {
            self.toolbar_mode = ToolbarMode::Normal;
            return;
        }

        self.loki_query = self.query_input.clone();
        self.toolbar_mode = ToolbarMode::Normal;

        // Reset view state.
        self.view_path.clear();
        self.scroll = ScrollState::Tail;
        self.h_scroll = 0;
        self.current_entry_count = 0;
        self.search = None;
        self.tree_select_cursor = None;

        // Signal the Loki source to restart with the new query.
        if let Some(ref tx) = self.source_restart_tx {
            let now = jiff::Zoned::now();
            let start = now
                .checked_sub(jiff::SignedDuration::from_hours(1))
                .unwrap_or(now.clone());
            let params = LokiSourceParams {
                query: self.loki_query.clone(),
                start_ns: start.timestamp().as_nanosecond(),
                end_ns: None,
                follow: true,
            };
            let _ = tx.send(Some(params));
        }

        self.query_input.clear();
        self.query_cursor = 0;
    }

    pub(super) fn apply_filter(&mut self) {
        if self.filter_input.is_empty() {
            self.toolbar_mode = ToolbarMode::Normal;
            return;
        }

        let mode = match self.filter_entry_mode {
            FilterEntryMode::Substring => FilterMode::substring(self.filter_input.clone()),
            FilterEntryMode::Regex => match regex::Regex::new(&self.filter_input) {
                Ok(re) => FilterMode::Regex(re),
                Err(_) => {
                    // TODO: show error feedback in toolbar
                    return;
                }
            },
        };

        let filter = Filter {
            mode,
            target: FilterTarget::Any,
            inverted: self.filter_inverted,
        };

        let Ok(mut arena) = self.arena.lock() else {
            return;
        };

        // Build child LogView by testing parent entries against the filter.
        let parent = arena.view_at(&self.view_path);
        let mut child_entries = Vec::with_capacity(parent.entries.len());
        for &arena_idx in &parent.entries {
            if entry_matches_filter(&arena, arena_idx, &filter) {
                child_entries.push(arena_idx);
            }
        }

        let child = LogView {
            filters: vec![filter],
            children: Vec::new(),
            entries: child_entries,
        };

        let parent_mut = arena.view_at_mut(&self.view_path);
        let child_idx = parent_mut.children.len();
        parent_mut.children.push(child);

        // Navigate into the new child, preserving the selected entry if possible.
        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
        self.view_path.push(child_idx);
        self.h_scroll = 0;
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();

        self.filter_input.clear();
        self.filter_cursor = 0;
        self.filter_inverted = false;
        self.toolbar_mode = ToolbarMode::Normal;
    }

    pub(super) fn apply_search(&mut self) {
        if self.filter_input.is_empty() {
            self.search = None;
            self.toolbar_mode = ToolbarMode::Normal;
            return;
        }

        let mode = match self.filter_entry_mode {
            FilterEntryMode::Substring => FilterMode::substring(self.filter_input.clone()),
            FilterEntryMode::Regex => match regex::Regex::new(&self.filter_input) {
                Ok(re) => FilterMode::Regex(re),
                Err(_) => {
                    // TODO: show error feedback in toolbar
                    return;
                }
            },
        };

        self.search = Some(Filter {
            mode,
            target: FilterTarget::Any,
            inverted: self.filter_inverted,
        });

        self.filter_input.clear();
        self.filter_cursor = 0;
        self.filter_inverted = false;
        self.toolbar_mode = ToolbarMode::Normal;
    }

    pub(super) fn jump_to_search_match(&mut self, direction: Direction) {
        let Some(filter) = &self.search else { return };

        let Ok(arena) = self.arena.lock() else { return };
        let view = arena.view_at(&self.view_path);
        let total = view.entries.len();
        if total == 0 {
            return;
        }

        let current = match &self.scroll {
            ScrollState::Tail => total.saturating_sub(1),
            ScrollState::Selected(idx) => (*idx).min(total.saturating_sub(1)),
        };

        let count = total;
        match direction {
            Direction::Forward => {
                for offset in 1..=count {
                    let candidate = (current + offset) % total;
                    let arena_idx = view.entries[candidate];
                    if entry_matches_filter(&arena, arena_idx, filter) {
                        self.scroll = ScrollState::Selected(candidate);
                        self.h_scroll = 0;
                        return;
                    }
                }
            }
            Direction::Backward => {
                for offset in 1..=count {
                    let candidate = (current + total - offset) % total;
                    let arena_idx = view.entries[candidate];
                    if entry_matches_filter(&arena, arena_idx, filter) {
                        self.scroll = ScrollState::Selected(candidate);
                        self.h_scroll = 0;
                        return;
                    }
                }
            }
        }
    }

    /// Build a temporary filter from the current filter input for live preview
    /// highlighting. Returns `None` when not in filter entry mode, the input is
    /// empty, or the regex is invalid.
    pub(super) fn preview_filter(&self) -> Option<Filter> {
        if !matches!(self.toolbar_mode, ToolbarMode::FilterEntry | ToolbarMode::SearchEntry)
            || self.filter_input.is_empty()
        {
            return None;
        }
        let mode = match self.filter_entry_mode {
            FilterEntryMode::Substring => FilterMode::substring(self.filter_input.clone()),
            FilterEntryMode::Regex => FilterMode::Regex(regex::Regex::new(&self.filter_input).ok()?),
        };
        Some(Filter {
            mode,
            target: FilterTarget::Any,
            inverted: self.filter_inverted,
        })
    }

    /// Return the arena index of the currently selected entry, if any.
    pub(super) fn selected_arena_idx(scroll: &ScrollState, arena: &Arena, view_path: &ViewPath) -> Option<usize> {
        if let ScrollState::Selected(view_idx) = *scroll {
            let view = arena.view_at(view_path);
            let clamped = view_idx.min(view.entries.len().saturating_sub(1));
            view.entries.get(clamped).copied()
        } else {
            None
        }
    }

    /// Try to find the entry with the given arena index in the given view,
    /// returning Selected if found, Tail otherwise.
    pub(super) fn reselect_scroll(arena: &Arena, view_path: &ViewPath, target: Option<usize>) -> ScrollState {
        if let Some(target) = target {
            let view = arena.view_at(view_path);
            if let Some(pos) = view.entries.iter().position(|&e| e == target) {
                ScrollState::Selected(pos)
            } else {
                ScrollState::Tail
            }
        } else {
            ScrollState::Tail
        }
    }

    pub(super) fn pop_filter(&mut self) {
        if self.view_path.is_empty() {
            return;
        }
        let Ok(arena) = self.arena.lock() else { return };
        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
        drop(arena);
        self.view_path.pop();
        self.h_scroll = 0;
        let Ok(arena) = self.arena.lock() else {
            self.scroll = ScrollState::Tail;
            return;
        };
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
        drop(arena);
        self.update_from_arena();
    }

    /// Navigate to the parent view and remove the current child branch from the
    /// tree. If an entry is selected, re-select it in the parent view.
    pub(super) fn pop_and_remove_filter(&mut self) {
        if self.view_path.is_empty() {
            return;
        }

        let Ok(mut arena) = self.arena.lock() else {
            return;
        };

        // Capture the arena index while still at the child view.
        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);

        let child_idx = self.view_path.pop().unwrap();
        self.h_scroll = 0;

        // Remove the child branch.
        {
            let parent = arena.view_at_mut(&self.view_path);
            parent.children.remove(child_idx);
        }

        // Re-select the same entry in the parent view, if possible.
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();
    }

    pub(super) fn navigate_sibling(&mut self, direction: i32) {
        if self.view_path.is_empty() {
            return;
        }

        let Ok(arena) = self.arena.lock() else {
            return;
        };

        let current_idx = *self.view_path.last().unwrap();
        let parent_path = &self.view_path[..self.view_path.len() - 1];
        let parent = arena.view_at(parent_path);
        let sibling_count = parent.children.len();

        if sibling_count == 0 {
            return;
        }

        let new_idx = if direction > 0 {
            (current_idx + 1) % sibling_count
        } else {
            if current_idx == 0 {
                sibling_count - 1
            } else {
                current_idx - 1
            }
        };

        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
        *self.view_path.last_mut().unwrap() = new_idx;
        self.h_scroll = 0;
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
    }
}

/// Test whether an arena entry matches a filter (for `FilterTarget::Any`).
/// Used by both filter application and search highlighting.
///
/// Works directly with interned `LogEntry` fields to avoid the per-entry
/// heap allocations that `resolve_entry()` would introduce.
pub(super) fn entry_matches_filter(arena: &Arena, arena_idx: usize, filter: &Filter) -> bool {
    let entry = &arena.entries[arena_idx];
    let rodeo = &arena.rodeo;

    let raw = match &filter.target {
        crate::filter::FilterTarget::Message => {
            let msg = rodeo.messages.resolve(&entry.message);
            filter.raw_matches(msg)
        }
        crate::filter::FilterTarget::Any => {
            let msg = rodeo.messages.resolve(&entry.message);
            filter.raw_matches(msg)
                || (0..entry.labels_length).any(|i| {
                    let (_, v) = &arena.labels[entry.labels_start + i];
                    filter.raw_matches(rodeo.label_values.resolve(v))
                })
                || (0..entry.structured_fields_length).any(|i| {
                    let (_, v) =
                        &arena.structured_fields[entry.structured_fields_start + i];
                    filter.raw_matches(rodeo.label_values.resolve(v))
                })
        }
        crate::filter::FilterTarget::Label(key_spur) => {
            (0..entry.labels_length).any(|i| {
                let (k, v) = &arena.labels[entry.labels_start + i];
                k == key_spur && filter.raw_matches(rodeo.label_values.resolve(v))
            })
        }
    };

    if filter.inverted { !raw } else { raw }
}
