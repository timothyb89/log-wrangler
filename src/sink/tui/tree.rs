use crossterm::event::{KeyCode, KeyModifiers};

use crate::filter::{FilterMode, FilterTarget, Matcher};
use crate::log::{Arena, LogView, ViewPath};

use super::filter::entry_matches_matcher;

use super::{App, ManagedSourceKind, OverlayMode, SourceDialogSourceType, TimezoneMode};

impl App {
    pub(super) fn enter_tree_select(&mut self) {
        let Ok(arena) = self.arena.lock() else { return };
        let mut flat: Vec<(ViewPath, String)> = Vec::new();
        let mut path: ViewPath = Vec::new();
        Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names, self.timezone_mode);
        let cursor = flat.iter().position(|(p, _)| *p == self.view_path).unwrap_or(0);
        self.overlay = OverlayMode::TreeSelect { cursor };
    }

    pub(super) fn handle_tree_select_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) {
        let cursor = match &self.overlay {
            OverlayMode::TreeSelect { cursor } => *cursor,
            _ => return,
        };
        match code {
            KeyCode::Esc => {
                self.overlay = OverlayMode::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = OverlayMode::TreeSelect { cursor: cursor.saturating_sub(1) };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let Ok(arena) = self.arena.lock() else { return };
                let mut flat: Vec<(ViewPath, String)> = Vec::new();
                let mut path: ViewPath = Vec::new();
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names, self.timezone_mode);
                self.overlay = OverlayMode::TreeSelect {
                    cursor: (cursor + 1).min(flat.len().saturating_sub(1)),
                };
            }
            KeyCode::Enter | KeyCode::Tab => {
                let Ok(arena) = self.arena.lock() else { return };
                let mut flat: Vec<(ViewPath, String)> = Vec::new();
                let mut path: ViewPath = Vec::new();
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names, self.timezone_mode);
                if let Some((selected_path, _)) = flat.get(cursor) {
                    if *selected_path != self.view_path {
                        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
                        self.view_path = selected_path.clone();
                        self.h_scroll = 0;
                        self.v_scroll = 0;
                        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
                    }
                    self.current_entry_count =
                        arena.view_at(&self.view_path).entries.len();
                }
                self.overlay = OverlayMode::None;
            }
            KeyCode::Char('!') => {
                let Ok(mut arena) = self.arena.lock() else { return };
                let mut flat: Vec<(ViewPath, String)> = Vec::new();
                let mut path: ViewPath = Vec::new();
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names, self.timezone_mode);
                let Some((selected_path, _)) = flat.get(cursor) else { return };
                // Can't invert the root view.
                if selected_path.is_empty() {
                    return;
                }
                let selected_path = selected_path.clone();
                Self::toggle_view_inversion(&mut arena, &selected_path);
                // Update entry count if we're currently viewing this path or a descendant.
                if self.view_path.starts_with(&selected_path) {
                    self.current_entry_count = arena.view_at(&self.view_path).entries.len();
                    let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
                    self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
                }
            }
            _ => {}
        }
    }

    pub(super) fn enter_source_select(&mut self) {
        self.overlay = OverlayMode::SourceSelect { cursor: 0 };
    }

    pub(super) fn handle_source_select_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) {
        let cursor = match &self.overlay {
            OverlayMode::SourceSelect { cursor } => *cursor,
            _ => return,
        };
        match code {
            KeyCode::Esc => {
                self.overlay = OverlayMode::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = OverlayMode::SourceSelect {
                    cursor: cursor.saturating_sub(1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.sources.len().saturating_sub(1);
                self.overlay = OverlayMode::SourceSelect {
                    cursor: (cursor + 1).min(max),
                };
            }
            KeyCode::Enter => {
                if let Some(source) = self.sources.get(cursor) {
                    let source_id = source.source_id;
                    self.overlay = OverlayMode::None;
                    self.apply_source_filter(source_id);
                }
            }
            KeyCode::Char('e') => {
                self.overlay = OverlayMode::None;
                self.open_edit_dialog_for_source_idx(cursor);
            }
            KeyCode::Char('a') => {
                self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                    mode: super::SourceDialogMode::Add,
                    source_type: SourceDialogSourceType::Loki,
                    fields: [String::new(), String::new(), String::new()],
                    cursors: [0, 0, 0],
                    active_field: 1,
                    error: None,
                    tls: None,
                });
            }
            KeyCode::Char('d') => {
                if cursor < self.sources.len() {
                    let source_id = self.sources[cursor].source_id;
                    // Dropping the source drops its kind, which closes the watch channel
                    // and causes the source task to exit.
                    self.sources.remove(cursor);
                    // Purge existing log entries from all views.
                    self.purge_source_entries(source_id);
                }
                let max = self.sources.len().saturating_sub(1);
                self.overlay = OverlayMode::SourceSelect { cursor: cursor.min(max) };
            }
            KeyCode::Char('c') => {
                if let Some(source) = self.sources.get(cursor) {
                    match &source.kind {
                        ManagedSourceKind::Loki { base_url, query, tls, .. } => {
                            let url_str = base_url.to_string();
                            let query = query.clone();
                            let tls = tls.clone();
                            self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                                mode: super::SourceDialogMode::Add,
                                source_type: SourceDialogSourceType::Loki,
                                fields: [String::new(), url_str.clone(), query.clone()],
                                cursors: [0, url_str.len(), query.len()],
                                active_field: 2,
                                error: None,
                                tls,
                            });
                        }
                        ManagedSourceKind::Subcommand { command, .. } => {
                            let command = command.clone();
                            self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                                mode: super::SourceDialogMode::Add,
                                source_type: SourceDialogSourceType::Subcommand,
                                fields: [String::new(), command.clone(), String::new()],
                                cursors: [0, command.len(), 0],
                                active_field: 1,
                                error: None,
                                tls: None,
                            });
                        }
                        ManagedSourceKind::Stdin => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Open the source edit dialog for a source by its index in `sources`.
    /// Currently only Loki sources have an editable query; other kinds are no-ops.
    pub(super) fn open_edit_dialog_for_source_idx(&mut self, source_idx: usize) {
        let Some(source) = self.sources.get(source_idx) else {
            return;
        };
        match &source.kind {
            ManagedSourceKind::Loki { query, .. } => {
                let query = query.clone();
                self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                    mode: super::SourceDialogMode::Edit { source_idx },
                    source_type: SourceDialogSourceType::Loki,
                    fields: [source.name.clone(), String::new(), query.clone()],
                    cursors: [0, 0, query.len()],
                    active_field: 2,
                    error: None,
                    tls: None,
                });
            }
            // Subcommand and Stdin sources don't support in-place editing.
            _ => {}
        }
    }

    /// Toggle inversion on a non-root view's matchers and rebuild its entries.
    fn toggle_view_inversion(arena: &mut Arena, path: &ViewPath) {
        // Toggle the inversion flag on each matcher.
        let view = arena.view_at_mut(path);
        let matchers = std::mem::take(&mut view.matchers);
        let matchers = matchers.into_iter().map(|m| match m {
            Matcher::Simple(mut f) => {
                f.inverted = !f.inverted;
                Matcher::Simple(f)
            }
            Matcher::Query(expr, text) => {
                let expr = match expr {
                    crate::query::QueryExpr::Not(inner) => *inner,
                    other => crate::query::QueryExpr::Not(Box::new(other)),
                };
                let text = if text.starts_with("not ") || text.starts_with("NOT ") {
                    text[4..].to_string()
                } else {
                    format!("not {}", text)
                };
                Matcher::Query(expr, text)
            }
        }).collect();
        arena.view_at_mut(path).matchers = matchers;

        // Rebuild entries for this view and its descendants using parent's entries.
        let parent_path = &path[..path.len() - 1];
        let parent_entries: Vec<usize> = arena.view_at(parent_path).entries.clone();
        Self::rebuild_subtree(arena, path, &parent_entries);
    }

    /// Rebuild a view's entries (and all descendants) by re-filtering from
    /// the given parent entry list.
    fn rebuild_subtree(arena: &mut Arena, path: &ViewPath, parent_entries: &[usize]) {
        // Clear this view's entries and recompute from parent.
        {
            let view = arena.view_at_mut(path);
            view.entries.clear();
        }

        // Re-filter parent entries through this view's matchers.
        let view = arena.view_at(path);
        let matchers: Vec<Matcher> = view.matchers.clone();
        for &arena_idx in parent_entries {
            let matches = matchers.iter().all(|m| entry_matches_matcher(arena, arena_idx, m));
            if matches {
                let view = arena.view_at_mut(path);
                view.entries.push(arena_idx);
            }
        }

        // Recursively rebuild children.
        let child_count = arena.view_at(path).children.len();
        let this_entries: Vec<usize> = arena.view_at(path).entries.clone();
        for i in 0..child_count {
            let mut child_path = path.to_vec();
            child_path.push(i);
            Self::rebuild_subtree(arena, &child_path, &this_entries);
        }
    }

    /// Flatten the view tree into (path, display-line) pairs for the overlay.
    /// `has_more[i]` = true means depth-i ancestor still has siblings after it.
    pub(super) fn flatten_view_tree(
        view: &LogView,
        path: &mut ViewPath,
        depth: usize,
        has_more: &[bool],
        out: &mut Vec<(ViewPath, String)>,
        source_names: &[String],
        tz_mode: TimezoneMode,
    ) {
        let label = if depth == 0 {
            format!("/ root  ({} entries)", view.entries.len())
        } else {
            let mut s = String::new();
            for i in 0..depth - 1 {
                s.push_str(if has_more[i] { "│   " } else { "    " });
            }
            s.push_str(if has_more[depth - 1] { "├── " } else { "└── " });
            let pat = view
                .matchers
                .first()
                .map(|m| match m {
                    Matcher::Query(_, source_text) => format!("{{{}}}", source_text),
                    Matcher::Simple(f) => {
                        // Display source filters as [source-name].
                        if let FilterTarget::Source(sid) = &f.target {
                            let name = source_names
                                .get(*sid as usize)
                                .map(|s| s.as_str())
                                .unwrap_or("?");
                            return format!("[{}]", name);
                        }
                        // Display time filters with direction and timestamp.
                        if let FilterTarget::After(ts) = &f.target {
                            let tz = match tz_mode {
                                TimezoneMode::Local => jiff::tz::TimeZone::system(),
                                TimezoneMode::Utc => jiff::tz::TimeZone::UTC,
                            };
                            let formatted = format!("{}", ts.to_zoned(tz).strftime("%H:%M:%S%.3f"));
                            return format!(">= {}", formatted);
                        }
                        if let FilterTarget::Before(ts) = &f.target {
                            let tz = match tz_mode {
                                TimezoneMode::Local => jiff::tz::TimeZone::system(),
                                TimezoneMode::Utc => jiff::tz::TimeZone::UTC,
                            };
                            let formatted = format!("{}", ts.to_zoned(tz).strftime("%H:%M:%S%.3f"));
                            return format!("<= {}", formatted);
                        }
                        let prefix = if f.inverted { "!" } else { "" };
                        match &f.mode {
                            FilterMode::Substring(p, _) => format!("{}\"{}\"", prefix, p),
                            FilterMode::Regex(r) => format!("{}/{}/", prefix, r.as_str()),
                        }
                    }
                })
                .unwrap_or_else(|| "(unfiltered)".to_string());
            s.push_str(&pat);
            s.push_str(&format!("  ({} entries)", view.entries.len()));
            s
        };
        out.push((path.clone(), label));

        let n = view.children.len();
        for (i, child) in view.children.iter().enumerate() {
            path.push(i);
            let mut next_has_more = has_more.to_vec();
            next_has_more.push(i < n - 1);
            Self::flatten_view_tree(child, path, depth + 1, &next_has_more, out, source_names, tz_mode);
            path.pop();
        }
    }
}
