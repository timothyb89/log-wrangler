use crate::filter::{Filter, FilterMode, FilterTarget};
use crate::log::{Arena, LogView, ViewPath};
use crate::source::loki::LokiSourceParams;
use crate::source::teleport::TeleportTlsConfig;

use super::{App, Direction, FilterEntryMode, ManagedSource, ManagedSourceKind, OverlayMode, ScrollState, SourceDialogMode, SourceDialogSourceType, ToolbarMode};

impl App {
    pub(super) fn submit_source_dialog(&mut self) {
        let state = match &self.overlay {
            OverlayMode::SourceDialog(s) => s,
            _ => return,
        };

        match &state.mode {
            SourceDialogMode::Add => {
                match state.source_type {
                    SourceDialogSourceType::Subcommand => {
                        let command = state.fields[1].trim().to_string();
                        if command.is_empty() {
                            let state = match &mut self.overlay {
                                OverlayMode::SourceDialog(s) => s,
                                _ => return,
                            };
                            state.error = Some("Command is required".to_string());
                            return;
                        }
                        let name = {
                            let n = state.fields[0].trim();
                            if n.is_empty() {
                                format!("cmd-{}", self.next_source_id)
                            } else {
                                n.to_string()
                            }
                        };
                        self.spawn_subcommand_source(name, command);
                        self.overlay = OverlayMode::None;
                        return;
                    }
                    SourceDialogSourceType::Loki => {}
                }

                let url_str = state.fields[1].trim();
                let query = state.fields[2].trim();

                if url_str.is_empty() || query.is_empty() {
                    let state = match &mut self.overlay {
                        OverlayMode::SourceDialog(s) => s,
                        _ => return,
                    };
                    state.error = Some("URL and Query are required".to_string());
                    return;
                }

                let base_url = match url::Url::parse(url_str) {
                    Ok(u) => u,
                    Err(e) => {
                        let state = match &mut self.overlay {
                            OverlayMode::SourceDialog(s) => s,
                            _ => return,
                        };
                        state.error = Some(format!("Invalid URL: {}", e));
                        return;
                    }
                };

                let name = {
                    let n = state.fields[0].trim();
                    if n.is_empty() {
                        format!("loki-{}", self.next_source_id)
                    } else {
                        n.to_string()
                    }
                };
                let query = query.to_string();
                let tls = state.tls.clone();

                self.spawn_loki_source(name, base_url, query, tls);
                self.overlay = OverlayMode::None;
            }
            SourceDialogMode::Edit { source_idx } => {
                let source_idx = *source_idx;
                let new_query = state.fields[2].trim().to_string();

                if new_query.is_empty() {
                    let state = match &mut self.overlay {
                        OverlayMode::SourceDialog(s) => s,
                        _ => return,
                    };
                    state.error = Some("Query cannot be empty".to_string());
                    return;
                }

                // Signal restart with the new query.
                if let Some(source) = self.sources.get_mut(source_idx) {
                    if let ManagedSourceKind::Loki { tx, query, .. } = &mut source.kind {
                        *query = new_query.clone();
                        let now = jiff::Zoned::now();
                        let start = now
                            .checked_sub(jiff::SignedDuration::from_hours(1))
                            .unwrap_or(now.clone());
                        let params = LokiSourceParams {
                            query: new_query,
                            start_ns: start.timestamp().as_nanosecond(),
                            end_ns: None,
                            follow: true,
                        };
                        let _ = tx.send(Some(params));
                    }
                }

                self.overlay = OverlayMode::None;

                // Reset view to root in tail mode.
                self.view_path.clear();
                self.scroll = ScrollState::Tail;
                self.h_scroll = 0;
                self.v_scroll = 0;
                self.current_entry_count = 0;
                self.search = None;
            }
        }
    }

    fn spawn_loki_source(
        &mut self,
        name: String,
        base_url: url::Url,
        query: String,
        tls: Option<TeleportTlsConfig>,
    ) {
        let source_id = self.next_source_id;
        self.next_source_id += 1;

        // Register in arena.
        {
            let mut arena = self.arena.lock().unwrap();
            if source_id as usize >= arena.source_names.len() {
                arena.source_names.resize(source_id as usize + 1, String::new());
            }
            arena.source_names[source_id as usize] = name.clone();
        }

        let (wtx, wrx) = tokio::sync::watch::channel(None);

        let now = jiff::Zoned::now();
        let start = now
            .checked_sub(jiff::SignedDuration::from_hours(1))
            .unwrap_or(now.clone());
        let params = LokiSourceParams {
            query: query.clone(),
            start_ns: start.timestamp().as_nanosecond(),
            end_ns: None,
            follow: true,
        };

        let http_client = tls
            .as_ref()
            .map(|t| t.http_client.clone())
            .unwrap_or_else(reqwest::Client::new);
        let ws_tls = tls.as_ref().map(|t| t.rustls_config.clone());

        let tx = self.ingest_tx.clone();
        tokio::spawn(crate::source::loki::run_loki_source(
            base_url.clone(), params, tx, wrx, source_id, http_client, ws_tls,
        ));

        self.sources.push(ManagedSource {
            source_id,
            name,
            kind: ManagedSourceKind::Loki {
                base_url,
                query,
                tx: wtx,
                tls,
            },
        });
    }

    pub(super) fn spawn_subcommand_source(&mut self, name: String, command: String) {
        let source_id = self.next_source_id;
        self.next_source_id += 1;

        {
            let mut arena = self.arena.lock().unwrap();
            if source_id as usize >= arena.source_names.len() {
                arena.source_names.resize(source_id as usize + 1, String::new());
            }
            arena.source_names[source_id as usize] = name.clone();
        }

        let kill_tx = crate::source::subcommand::run_subcommand_source(
            command.clone(),
            self.ingest_tx.clone(),
            source_id,
        );

        self.sources.push(ManagedSource {
            source_id,
            name,
            kind: ManagedSourceKind::Subcommand { command, kill_tx },
        });
    }

    /// Remove all view references to entries from the given source, then
    /// clamp scroll if it's now out of range.
    pub(super) fn purge_source_entries(&mut self, source_id: u16) {
        let Ok(mut arena) = self.arena.lock() else { return };
        arena.clear_source(source_id);
        let count = arena.view_at(&self.view_path).entries.len();
        drop(arena);
        self.current_entry_count = count;
        if let ScrollState::Selected(idx) = self.scroll {
            if idx >= self.current_entry_count {
                self.scroll = ScrollState::Tail;
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
        }
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
        self.v_scroll = 0;
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
                        self.v_scroll = 0;
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
                        self.v_scroll = 0;
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

    /// Create a child LogView filtered to a specific source.
    pub(super) fn apply_source_filter(&mut self, source_id: u16) {
        let filter = Filter {
            mode: FilterMode::substring(String::new()),
            target: FilterTarget::Source(source_id),
            inverted: false,
        };

        let Ok(mut arena) = self.arena.lock() else {
            return;
        };

        let parent = arena.view_at(&self.view_path);
        let mut child_entries = Vec::with_capacity(parent.entries.len());
        for &arena_idx in &parent.entries {
            if arena.entries[arena_idx].source_id == source_id {
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

        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
        self.view_path.push(child_idx);
        self.h_scroll = 0;
        self.v_scroll = 0;
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();
    }

    /// Create a child LogView filtered to entries after (>=) or before (<=) a
    /// reference timestamp. If an entry is selected, its timestamp is used;
    /// otherwise the current wall-clock time is used.
    pub(super) fn apply_time_filter(&mut self, keep_after: bool) {
        let Ok(mut arena) = self.arena.lock() else {
            return;
        };

        let reference_ts = if let ScrollState::Selected(view_idx) = &self.scroll {
            let view = arena.view_at(&self.view_path);
            let clamped = (*view_idx).min(view.entries.len().saturating_sub(1));
            view.entries.get(clamped).map(|&idx| arena.entries[idx].timestamp.timestamp())
        } else {
            None
        };
        let reference_ts = reference_ts.unwrap_or_else(jiff::Timestamp::now);

        let target = if keep_after {
            FilterTarget::After(reference_ts)
        } else {
            FilterTarget::Before(reference_ts)
        };

        let filter = Filter {
            mode: FilterMode::substring(String::new()),
            target,
            inverted: false,
        };

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

        let selected = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
        self.view_path.push(child_idx);
        self.h_scroll = 0;
        self.v_scroll = 0;
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, selected);
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();
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
        self.v_scroll = 0;
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
        self.v_scroll = 0;

        // Remove the child branch.
        {
            let parent = arena.view_at_mut(&self.view_path);
            parent.children.remove(child_idx);
        }

        // Re-select the same entry in the parent view, if possible.
        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();
    }

    /// Save the current configuration as a profile.
    pub(super) fn save_profile(&mut self, name_or_path: &str) {
        let path = match crate::profile::resolve_profile_path(name_or_path) {
            Ok(p) => p,
            Err(e) => {
                if let OverlayMode::ProfileSaveDialog(ref mut state) = self.overlay {
                    state.error = Some(format!("{}", e));
                }
                return;
            }
        };

        let arena = match self.arena.lock() {
            Ok(a) => a,
            Err(_) => return,
        };

        let profile = crate::profile::Profile::from_app_state(&self.sources, &arena);
        drop(arena);

        if let Err(e) = crate::profile::save_profile(&profile, &path) {
            if let OverlayMode::ProfileSaveDialog(ref mut state) = self.overlay {
                state.error = Some(format!("{}", e));
            }
            return;
        }

        self.overlay = OverlayMode::None;
    }

    /// Load a profile and apply sources, filters, or both based on mode.
    pub(super) fn load_profile(&mut self, path: &std::path::Path, mode: &crate::profile::ProfileLoadMode) {
        let profile = match crate::profile::load_profile(path) {
            Ok(p) => p,
            Err(_) => return,
        };

        match mode {
            crate::profile::ProfileLoadMode::All => {
                self.apply_profile_sources(&profile);
                self.apply_profile_filters(&profile);
            }
            crate::profile::ProfileLoadMode::Sources => {
                self.apply_profile_sources(&profile);
            }
            crate::profile::ProfileLoadMode::Filters => {
                self.apply_profile_filters(&profile);
            }
        }
    }

    fn apply_profile_sources(&mut self, profile: &crate::profile::Profile) {
        let Some(sources) = &profile.sources else { return };

        for ps in sources {
            // Skip stdin sources at runtime (stdin is a one-shot resource).
            if ps.uri.starts_with("stdin") {
                continue;
            }

            let config = match crate::source::parse_source_uri(&ps.uri) {
                Ok(c) => c,
                Err(_) => continue,
            };

            match config {
                crate::source::SourceConfig::GrafanaLoki { base_url } => {
                    let query = match &ps.query {
                        Some(q) => q.clone(),
                        None => continue,
                    };
                    self.spawn_loki_source(ps.name.clone(), base_url, query, None);
                }
                crate::source::SourceConfig::Subcommand { command } => {
                    let cmd = ps.query.as_ref().or(command.as_ref());
                    if let Some(cmd) = cmd {
                        self.spawn_subcommand_source(ps.name.clone(), cmd.clone());
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_profile_filters(&mut self, profile: &crate::profile::Profile) {
        let Some(tree) = &profile.filters else { return };

        let Ok(mut arena) = self.arena.lock() else { return };

        let new_root = crate::profile::profile_to_view_tree(tree, &arena.rodeo, &arena.source_names);
        arena.root_view = new_root;
        arena.rebuild_views();

        self.view_path.clear();
        self.scroll = ScrollState::Tail;
        self.h_scroll = 0;
        self.v_scroll = 0;
        self.current_entry_count = arena.root_view.entries.len();
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
        self.v_scroll = 0;
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
        crate::filter::FilterTarget::Source(sid) => {
            return if filter.inverted {
                entry.source_id != *sid
            } else {
                entry.source_id == *sid
            };
        }
        crate::filter::FilterTarget::After(ts) => {
            return entry.timestamp.timestamp() >= *ts;
        }
        crate::filter::FilterTarget::Before(ts) => {
            return entry.timestamp.timestamp() <= *ts;
        }
    };

    if filter.inverted { !raw } else { raw }
}
