use crossterm::event::{KeyCode, KeyModifiers};

use crate::filter::{FilterMode, FilterTarget};
use crate::log::{LogView, ViewPath};

use super::{App, OverlayMode};

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
        let Ok(arena) = self.arena.lock() else { return };
        if arena.source_names.len() <= 1 {
            return;
        }
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
                let Ok(arena) = self.arena.lock() else { return };
                let max = arena.source_names.len().saturating_sub(1);
                self.overlay = OverlayMode::SourceSelect {
                    cursor: (cursor + 1).min(max),
                };
            }
            KeyCode::Enter => {
                let source_id = cursor as u16;
                self.overlay = OverlayMode::None;
                self.apply_source_filter(source_id);
            }
            KeyCode::Char('e') => {
                let source_id = cursor as u16;
                self.overlay = OverlayMode::None;
                self.open_edit_dialog_for_source(source_id);
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
            _ => {}
        }
    }

    /// Open the source edit dialog for a specific Loki source by its source_id.
    pub(super) fn open_edit_dialog_for_source(&mut self, source_id: u16) {
        let Some(loki_idx) = self.loki_restarts.iter().position(|r| r.source_id == source_id) else {
            return;
        };
        self.open_edit_dialog_for_loki_idx(loki_idx);
    }

    /// Open the source edit dialog for a Loki source by its index in `loki_restarts`.
    pub(super) fn open_edit_dialog_for_loki_idx(&mut self, loki_idx: usize) {
        let Some(restart) = self.loki_restarts.get(loki_idx) else {
            return;
        };
        self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
            mode: super::SourceDialogMode::Edit { loki_idx },
            fields: [
                restart.name.clone(),
                String::new(),
                restart.query.clone(),
            ],
            cursors: [0, 0, restart.query.len()],
            active_field: 2, // focus on query
            error: None,
        });
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
