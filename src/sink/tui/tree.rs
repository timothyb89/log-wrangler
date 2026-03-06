use crossterm::event::{KeyCode, KeyModifiers};

use crate::filter::{FilterMode, FilterTarget};
use crate::log::{LogView, ViewPath};

use super::{App, ManagedSourceKind, OverlayMode};

impl App {
    pub(super) fn enter_tree_select(&mut self) {
        let Ok(arena) = self.arena.lock() else { return };
        let mut flat: Vec<(ViewPath, String)> = Vec::new();
        let mut path: ViewPath = Vec::new();
        Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names);
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
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names);
                self.overlay = OverlayMode::TreeSelect {
                    cursor: (cursor + 1).min(flat.len().saturating_sub(1)),
                };
            }
            KeyCode::Enter | KeyCode::Tab => {
                let Ok(arena) = self.arena.lock() else { return };
                let mut flat: Vec<(ViewPath, String)> = Vec::new();
                let mut path: ViewPath = Vec::new();
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names);
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
                    fields: [String::new(), String::new(), String::new()],
                    cursors: [0, 0, 0],
                    active_field: 1,
                    error: None,
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
                    if let ManagedSourceKind::Loki { base_url, query, .. } = &source.kind {
                        let url_str = base_url.to_string();
                        let query = query.clone();
                        self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                            mode: super::SourceDialogMode::Add,
                            fields: [String::new(), url_str.clone(), query.clone()],
                            cursors: [0, url_str.len(), query.len()],
                            active_field: 2,
                            error: None,
                        });
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
        if let ManagedSourceKind::Loki { query, .. } = &source.kind {
            self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                mode: super::SourceDialogMode::Edit { source_idx },
                fields: [
                    source.name.clone(),
                    String::new(),
                    query.clone(),
                ],
                cursors: [0, 0, query.len()],
                active_field: 2, // focus on query
                error: None,
            });
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
                .filters
                .first()
                .map(|f| {
                    // Display source filters as [source-name].
                    if let FilterTarget::Source(sid) = &f.target {
                        let name = source_names
                            .get(*sid as usize)
                            .map(|s| s.as_str())
                            .unwrap_or("?");
                        return format!("[{}]", name);
                    }
                    let prefix = if f.inverted { "!" } else { "" };
                    match &f.mode {
                        FilterMode::Substring(p, _) => format!("{}\"{}\"", prefix, p),
                        FilterMode::Regex(r) => format!("{}/{}/", prefix, r.as_str()),
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
            Self::flatten_view_tree(child, path, depth + 1, &next_has_more, out, source_names);
            path.pop();
        }
    }
}
