use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use super::{App, Direction, DisplayMode, FilterEntryMode, OverlayMode, ScrollState, TimezoneMode, ToolbarMode};

impl App {
    pub(super) fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match &self.overlay {
                    OverlayMode::TreeSelect { .. } => {
                        self.handle_tree_select_key(key.code, key.modifiers);
                        return;
                    }
                    OverlayMode::SourceSelect { .. } => {
                        self.handle_source_select_key(key.code, key.modifiers);
                        return;
                    }
                    OverlayMode::SourceDialog(_) => {
                        self.handle_source_dialog_key(key.code, key.modifiers);
                        return;
                    }
                    OverlayMode::None => {}
                }
                match self.toolbar_mode {
                    ToolbarMode::Normal => self.handle_normal_key(key.code, key.modifiers),
                    ToolbarMode::FilterEntry => self.handle_filter_key(key.code, key.modifiers),
                    ToolbarMode::SearchEntry => self.handle_search_key(key.code, key.modifiers),
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown if mouse.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.scroll_proportional_down(2);
                }
                MouseEventKind::ScrollUp if mouse.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.scroll_proportional_up(2);
                }
                MouseEventKind::ScrollDown => self.scroll_down(),
                MouseEventKind::ScrollUp => self.scroll_up(),
                MouseEventKind::Down(MouseButton::Left) => {
                    if !matches!(self.overlay, OverlayMode::None) {
                        return;
                    }
                    if mouse.row >= self.log_list_body_y {
                        let offset = (mouse.row - self.log_list_body_y) as usize;
                        if let Some(&view_idx) = self.visible_row_map.get(offset) {
                            self.scroll = ScrollState::Selected(view_idx);
                            self.h_scroll = 0;
                            self.v_scroll = 0;
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::SHIFT) => {
                self.scroll_proportional_down(10);
            }
            (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::SHIFT) => {
                self.scroll_proportional_up(10);
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => self.scroll_down(),
            (KeyCode::Up | KeyCode::Char('k'), _) => self.scroll_up(),
            (KeyCode::Char('G') | KeyCode::End, _) => {
                self.scroll = ScrollState::Tail;
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
            (KeyCode::Char('g') | KeyCode::Home, _) => {
                if self.current_entry_count > 0 {
                    self.scroll = ScrollState::Selected(0);
                    self.h_scroll = 0;
                    self.v_scroll = 0;
                }
            }
            (KeyCode::Right | KeyCode::Char('l'), _) => {
                self.h_scroll = self.h_scroll.saturating_add(8);
            }
            (KeyCode::Left | KeyCode::Char('h'), _) => {
                self.h_scroll = self.h_scroll.saturating_sub(8);
            }
            (KeyCode::PageDown, _) => self.scroll_page_down(),
            (KeyCode::PageUp, _) => self.scroll_page_up(),
            (KeyCode::Char('/'), _) => {
                self.toolbar_mode = ToolbarMode::FilterEntry;
                self.filter_input.clear();
                self.filter_cursor = 0;
                self.filter_inverted = false;
            }
            (KeyCode::Backspace, _) => self.pop_filter(),
            (KeyCode::Char('p'), _) => self.pop_and_remove_filter(),
            (KeyCode::Char('['), _) => self.navigate_sibling(-1),
            (KeyCode::Char(']'), _) => self.navigate_sibling(1),
            (KeyCode::Tab, _) => self.enter_tree_select(),
            (KeyCode::Char('?'), _) => {
                self.toolbar_mode = ToolbarMode::SearchEntry;
                self.filter_input.clear();
                self.filter_cursor = 0;
                self.filter_inverted = false;
            }
            (KeyCode::Char('n'), KeyModifiers::NONE) => {
                self.jump_to_search_match(Direction::Forward);
            }
            (KeyCode::Char('N'), _) => {
                self.jump_to_search_match(Direction::Backward);
            }
            (KeyCode::Esc, _) => {
                self.search = None;
            }
            (KeyCode::Char('v'), _) => {
                self.display_mode = match self.display_mode {
                    DisplayMode::Raw => DisplayMode::Pretty,
                    DisplayMode::Pretty => DisplayMode::Raw,
                };
            }
            (KeyCode::Char('t'), _) => {
                self.timezone_mode = match self.timezone_mode {
                    TimezoneMode::Local => TimezoneMode::Utc,
                    TimezoneMode::Utc => TimezoneMode::Local,
                };
            }
            (KeyCode::Char('a'), _) => {
                self.overlay = OverlayMode::SourceDialog(super::SourceDialogState {
                    mode: super::SourceDialogMode::Add,
                    fields: [String::new(), String::new(), String::new()],
                    cursors: [0, 0, 0],
                    active_field: 1, // start on URL field
                    error: None,
                });
            }
            (KeyCode::Char('s'), _) => {
                self.enter_source_select();
            }
            _ => {}
        }
    }

    fn handle_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Esc, _) => {
                self.toolbar_mode = ToolbarMode::Normal;
                self.filter_input.clear();
                self.filter_cursor = 0;
                self.filter_inverted = false;
            }
            (KeyCode::Enter, _) => {
                self.apply_filter();
            }
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                self.filter_entry_mode = match self.filter_entry_mode {
                    FilterEntryMode::Substring => FilterEntryMode::Regex,
                    FilterEntryMode::Regex => FilterEntryMode::Substring,
                };
            }
            (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                self.filter_inverted = !self.filter_inverted;
            }
            (KeyCode::Backspace, _) => {
                if self.filter_cursor > 0 {
                    self.filter_cursor -= 1;
                    self.filter_input.remove(self.filter_cursor);
                }
            }
            (KeyCode::Delete, _) => {
                if self.filter_cursor < self.filter_input.len() {
                    self.filter_input.remove(self.filter_cursor);
                }
            }
            (KeyCode::Left, _) => {
                self.filter_cursor = self.filter_cursor.saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                self.filter_cursor = (self.filter_cursor + 1).min(self.filter_input.len());
            }
            (KeyCode::Char(c), _) => {
                self.filter_input.insert(self.filter_cursor, c);
                self.filter_cursor += 1;
            }
            _ => {}
        }
    }

    fn handle_search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Esc, _) => {
                self.toolbar_mode = ToolbarMode::Normal;
                self.filter_input.clear();
                self.filter_cursor = 0;
                self.filter_inverted = false;
            }
            (KeyCode::Enter, _) => {
                self.apply_search();
            }
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                self.filter_entry_mode = match self.filter_entry_mode {
                    FilterEntryMode::Substring => FilterEntryMode::Regex,
                    FilterEntryMode::Regex => FilterEntryMode::Substring,
                };
            }
            (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                self.filter_inverted = !self.filter_inverted;
            }
            (KeyCode::Backspace, _) => {
                if self.filter_cursor > 0 {
                    self.filter_cursor -= 1;
                    self.filter_input.remove(self.filter_cursor);
                }
            }
            (KeyCode::Delete, _) => {
                if self.filter_cursor < self.filter_input.len() {
                    self.filter_input.remove(self.filter_cursor);
                }
            }
            (KeyCode::Left, _) => {
                self.filter_cursor = self.filter_cursor.saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                self.filter_cursor = (self.filter_cursor + 1).min(self.filter_input.len());
            }
            (KeyCode::Char(c), _) => {
                self.filter_input.insert(self.filter_cursor, c);
                self.filter_cursor += 1;
            }
            _ => {}
        }
    }

    fn handle_source_dialog_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let state = match &mut self.overlay {
            OverlayMode::SourceDialog(s) => s,
            _ => return,
        };

        // Determine which fields are editable.
        let editable: &[usize] = match &state.mode {
            super::SourceDialogMode::Add => &[0, 1, 2],    // name, url, query
            super::SourceDialogMode::Edit { .. } => &[2],   // query only
        };

        match (code, modifiers) {
            (KeyCode::Esc, _) => {
                self.overlay = OverlayMode::None;
            }
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                if editable.len() <= 1 {
                    return;
                }
                let current_pos = editable.iter().position(|&f| f == state.active_field).unwrap_or(0);
                let next_pos = if code == KeyCode::BackTab {
                    (current_pos + editable.len() - 1) % editable.len()
                } else {
                    (current_pos + 1) % editable.len()
                };
                state.active_field = editable[next_pos];
            }
            (KeyCode::Enter, _) => {
                self.submit_source_dialog();
            }
            (KeyCode::Backspace, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                let f = state.active_field;
                if state.cursors[f] > 0 {
                    state.cursors[f] -= 1;
                    state.fields[f].remove(state.cursors[f]);
                }
            }
            (KeyCode::Delete, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                let f = state.active_field;
                if state.cursors[f] < state.fields[f].len() {
                    state.fields[f].remove(state.cursors[f]);
                }
            }
            (KeyCode::Left, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                let f = state.active_field;
                state.cursors[f] = state.cursors[f].saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                let f = state.active_field;
                state.cursors[f] = (state.cursors[f] + 1).min(state.fields[f].len());
            }
            (KeyCode::Home, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                state.cursors[state.active_field] = 0;
            }
            (KeyCode::End, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                let f = state.active_field;
                state.cursors[f] = state.fields[f].len();
            }
            (KeyCode::Char(c), _) => {
                let state = match &mut self.overlay {
                    OverlayMode::SourceDialog(s) => s,
                    _ => return,
                };
                let f = state.active_field;
                state.fields[f].insert(state.cursors[f], c);
                state.cursors[f] += 1;
            }
            _ => {}
        }
    }

    // --- Scrolling ---

    fn scroll_down(&mut self) {
        match &self.scroll {
            ScrollState::Tail => {}
            ScrollState::Selected(idx) => {
                // In pretty mode, scroll within a tall entry before moving on.
                if self.display_mode == DisplayMode::Pretty {
                    if let Some(entry_height) = self.selected_entry_height {
                        let data_height = self.viewport_height.saturating_sub(1);
                        if entry_height > data_height {
                            let max_v = entry_height.saturating_sub(data_height);
                            if self.v_scroll < max_v {
                                self.v_scroll += 1;
                                return;
                            }
                        }
                    }
                }

                let next = idx + 1;
                if next >= self.current_entry_count {
                    self.scroll = ScrollState::Tail;
                } else {
                    self.scroll = ScrollState::Selected(next);
                }
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
        }
    }

    fn scroll_up(&mut self) {
        match &self.scroll {
            ScrollState::Tail => {
                if self.current_entry_count > 0 {
                    self.scroll = ScrollState::Selected(self.current_entry_count - 1);
                    self.h_scroll = 0;
                    // Enter at bottom of entry; clamped during render.
                    self.v_scroll = usize::MAX;
                }
            }
            ScrollState::Selected(idx) => {
                // In pretty mode, scroll within a tall entry before moving on.
                if self.display_mode == DisplayMode::Pretty && self.v_scroll > 0 {
                    self.v_scroll -= 1;
                    return;
                }

                if *idx > 0 {
                    self.scroll = ScrollState::Selected(idx - 1);
                    self.h_scroll = 0;
                    // Enter at bottom of previous entry; clamped during render.
                    self.v_scroll = usize::MAX;
                }
            }
        }
    }

    fn scroll_page_down(&mut self) {
        let page = self.viewport_height.max(1);
        match &self.scroll {
            ScrollState::Tail => {}
            ScrollState::Selected(idx) => {
                // In pretty mode, page within a tall entry first.
                if self.display_mode == DisplayMode::Pretty {
                    if let Some(entry_height) = self.selected_entry_height {
                        let data_height = self.viewport_height.saturating_sub(1);
                        if entry_height > data_height {
                            let max_v = entry_height.saturating_sub(data_height);
                            if self.v_scroll < max_v {
                                self.v_scroll = (self.v_scroll + page).min(max_v);
                                return;
                            }
                        }
                    }
                }

                let next = idx + page;
                if next >= self.current_entry_count {
                    self.scroll = ScrollState::Tail;
                } else {
                    self.scroll = ScrollState::Selected(next);
                }
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
        }
    }

    fn scroll_page_up(&mut self) {
        let page = self.viewport_height.max(1);
        match &self.scroll {
            ScrollState::Tail => {
                if self.current_entry_count > 0 {
                    let target = self.current_entry_count.saturating_sub(page);
                    self.scroll = ScrollState::Selected(target);
                    self.h_scroll = 0;
                    self.v_scroll = 0;
                }
            }
            ScrollState::Selected(idx) => {
                // In pretty mode, page within a tall entry first.
                if self.display_mode == DisplayMode::Pretty && self.v_scroll > 0 {
                    self.v_scroll = self.v_scroll.saturating_sub(page);
                    return;
                }

                self.scroll = ScrollState::Selected(idx.saturating_sub(page));
                self.h_scroll = 0;
                // Enter at bottom of target entry; clamped during render.
                self.v_scroll = usize::MAX;
            }
        }
    }

    fn scroll_proportional_down(&mut self, percent: usize) {
        let jump = (self.current_entry_count * percent / 100).max(1);
        match &self.scroll {
            ScrollState::Tail => {}
            ScrollState::Selected(idx) => {
                let next = idx + jump;
                if next >= self.current_entry_count {
                    self.scroll = ScrollState::Tail;
                } else {
                    self.scroll = ScrollState::Selected(next);
                }
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
        }
    }

    fn scroll_proportional_up(&mut self, percent: usize) {
        let jump = (self.current_entry_count * percent / 100).max(1);
        match &self.scroll {
            ScrollState::Tail => {
                if self.current_entry_count > 0 {
                    let target = self.current_entry_count.saturating_sub(jump);
                    self.scroll = ScrollState::Selected(target);
                    self.h_scroll = 0;
                    self.v_scroll = 0;
                }
            }
            ScrollState::Selected(idx) => {
                self.scroll = ScrollState::Selected(idx.saturating_sub(jump));
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
        }
    }
}
