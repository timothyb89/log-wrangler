use crossterm::event::{KeyCode, KeyModifiers};

use crate::filter::FilterMode;
use crate::log::{LogView, ViewPath};

use super::App;

impl App {
    pub(super) fn enter_tree_select(&mut self) {
        let Ok(arena) = self.arena.lock() else { return };
        let mut flat: Vec<(ViewPath, String)> = Vec::new();
        let mut path: ViewPath = Vec::new();
        Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat);
        let cursor = flat.iter().position(|(p, _)| *p == self.view_path).unwrap_or(0);
        self.tree_select_cursor = Some(cursor);
    }

    pub(super) fn handle_tree_select_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) {
        let cursor = match self.tree_select_cursor {
            Some(c) => c,
            None => return,
        };
        match code {
            KeyCode::Esc => {
                self.tree_select_cursor = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.tree_select_cursor = Some(cursor.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let Ok(arena) = self.arena.lock() else { return };
                let mut flat: Vec<(ViewPath, String)> = Vec::new();
                let mut path: ViewPath = Vec::new();
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat);
                self.tree_select_cursor =
                    Some((cursor + 1).min(flat.len().saturating_sub(1)));
            }
            KeyCode::Enter | KeyCode::Tab => {
                let Ok(arena) = self.arena.lock() else { return };
                let mut flat: Vec<(ViewPath, String)> = Vec::new();
                let mut path: ViewPath = Vec::new();
                Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat);
                if let Some((selected_path, _)) = flat.get(cursor) {
                    if *selected_path != self.view_path {
                        let target = Self::selected_arena_idx(&self.scroll, &arena, &self.view_path);
                        self.view_path = selected_path.clone();
                        self.h_scroll = 0;
                        self.scroll = Self::reselect_scroll(&arena, &self.view_path, target);
                    }
                    self.current_entry_count =
                        arena.view_at(&self.view_path).entries.len();
                }
                self.tree_select_cursor = None;
            }
            _ => {}
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
            Self::flatten_view_tree(child, path, depth + 1, &next_has_more, out);
            path.pop();
        }
    }
}
