use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use super::action::{Action, COMMAND_REGISTRY};
use super::{App, CommandPaletteState, Direction, DisplayMode, FilterEntryMode, OverlayMode, ProfileLoadState, ProfileSaveState, ScrollState, SourceDialogMode, SourceDialogSourceType, SourceDialogState, TimezoneMode, ToolbarMode};

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
                    OverlayMode::CommandPalette(_) => {
                        self.handle_command_palette_key(key.code, key.modifiers);
                        return;
                    }
                    OverlayMode::ProfileSaveDialog(_) => {
                        self.handle_profile_save_key(key.code, key.modifiers);
                        return;
                    }
                    OverlayMode::ProfileLoadDialog(_) => {
                        self.handle_profile_load_key(key.code, key.modifiers);
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
            Event::Mouse(mouse) => {
                // Route mouse events to command palette when open.
                if let OverlayMode::CommandPalette(ref mut state) = self.overlay {
                    let max = state.filtered_indices().len();
                    match mouse.kind {
                        MouseEventKind::ScrollDown => {
                            state.selected = (state.selected + 1).min(max.saturating_sub(1));
                        }
                        MouseEventKind::ScrollUp => {
                            state.selected = state.selected.saturating_sub(1);
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            let area = state.list_area;
                            // Click inside the list area (excluding borders).
                            if mouse.row > area.y
                                && mouse.row < area.y + area.height.saturating_sub(1)
                                && mouse.column >= area.x
                                && mouse.column < area.x + area.width
                            {
                                let clicked = (mouse.row - area.y - 1) as usize;
                                if clicked < max {
                                    state.selected = clicked;
                                    // Double-purpose: select and execute.
                                    let indices = state.filtered_indices();
                                    if let Some(&reg_idx) = indices.get(clicked) {
                                        let action = COMMAND_REGISTRY[reg_idx].action;
                                        self.overlay = OverlayMode::None;
                                        self.dispatch_action(action);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    return;
                }

                match mouse.kind {
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
                }
            }
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.dispatch_action(Action::Quit),
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => self.dispatch_action(Action::OpenCommandPalette),
            (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::CONTROL) => self.scroll_to_next_day(),
            (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::CONTROL) => self.scroll_to_prev_day(),
            (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::SHIFT) => {
                self.scroll_proportional_down(10);
            }
            (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::SHIFT) => {
                self.scroll_proportional_up(10);
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => self.scroll_down(),
            (KeyCode::Up | KeyCode::Char('k'), _) => self.scroll_up(),
            (KeyCode::Char('G') | KeyCode::End, _) => self.dispatch_action(Action::ScrollToBottom),
            (KeyCode::Char('g') | KeyCode::Home, _) => self.dispatch_action(Action::ScrollToTop),
            (KeyCode::Right | KeyCode::Char('l'), _) => {
                self.h_scroll = self.h_scroll.saturating_add(8);
            }
            (KeyCode::Left | KeyCode::Char('h'), _) => {
                self.h_scroll = self.h_scroll.saturating_sub(8);
            }
            (KeyCode::PageDown, _) => self.scroll_page_down(),
            (KeyCode::PageUp, _) => self.scroll_page_up(),
            (KeyCode::Char('/'), _) => self.dispatch_action(Action::EnterFilterMode),
            (KeyCode::Backspace, _) => self.dispatch_action(Action::PopFilter),
            (KeyCode::Char('p'), _) => self.dispatch_action(Action::PopAndRemoveFilter),
            (KeyCode::Char('['), _) => self.dispatch_action(Action::NavigateSiblingPrev),
            (KeyCode::Char(']'), _) => self.dispatch_action(Action::NavigateSiblingNext),
            (KeyCode::Tab, _) => self.dispatch_action(Action::OpenTreeSelect),
            (KeyCode::Char('?'), _) => self.dispatch_action(Action::EnterSearchMode),
            (KeyCode::Char('n'), KeyModifiers::NONE) => self.dispatch_action(Action::SearchNext),
            (KeyCode::Char('N'), _) => self.dispatch_action(Action::SearchPrev),
            (KeyCode::Esc, _) => self.dispatch_action(Action::ClearSearch),
            (KeyCode::Char('v'), _) => self.dispatch_action(Action::ToggleDisplayMode),
            (KeyCode::Char('t'), _) => self.dispatch_action(Action::ToggleTimezone),
            (KeyCode::Char('s'), _) => self.dispatch_action(Action::OpenSourceSelect),
            (KeyCode::Char('>'), _) => self.dispatch_action(Action::TimeFilterAfter),
            (KeyCode::Char('<'), _) => self.dispatch_action(Action::TimeFilterBefore),
            (KeyCode::Char('q'), _) => self.dispatch_action(Action::Quit),
            _ => {}
        }
    }

    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::EnterFilterMode => {
                self.toolbar_mode = ToolbarMode::FilterEntry;
                self.filter_input.clear();
                self.filter_cursor = 0;
                self.filter_inverted = false;
            }
            Action::EnterSearchMode => {
                self.toolbar_mode = ToolbarMode::SearchEntry;
                self.filter_input.clear();
                self.filter_cursor = 0;
                self.filter_inverted = false;
            }
            Action::PopFilter => self.pop_filter(),
            Action::PopAndRemoveFilter => self.pop_and_remove_filter(),
            Action::NavigateSiblingPrev => self.navigate_sibling(-1),
            Action::NavigateSiblingNext => self.navigate_sibling(1),
            Action::OpenTreeSelect => self.enter_tree_select(),
            Action::OpenSourceSelect => self.enter_source_select(),
            Action::ToggleDisplayMode => {
                self.display_mode = match self.display_mode {
                    DisplayMode::Raw => DisplayMode::Pretty,
                    DisplayMode::Pretty => DisplayMode::Raw,
                };
            }
            Action::ToggleTimezone => {
                self.timezone_mode = match self.timezone_mode {
                    TimezoneMode::Local => TimezoneMode::Utc,
                    TimezoneMode::Utc => TimezoneMode::Local,
                };
            }
            Action::TimeFilterAfter => self.apply_time_filter(true),
            Action::TimeFilterBefore => self.apply_time_filter(false),
            Action::SearchNext => self.jump_to_search_match(Direction::Forward),
            Action::SearchPrev => self.jump_to_search_match(Direction::Backward),
            Action::ClearSearch => {
                self.search = None;
            }
            Action::ScrollToTop => {
                if self.current_entry_count > 0 {
                    self.scroll = ScrollState::Selected(0);
                    self.h_scroll = 0;
                    self.v_scroll = 0;
                }
            }
            Action::ScrollToBottom => {
                self.scroll = ScrollState::Tail;
                self.h_scroll = 0;
                self.v_scroll = 0;
            }
            Action::AddSourceLoki => {
                self.overlay = OverlayMode::SourceDialog(SourceDialogState {
                    mode: SourceDialogMode::Add,
                    source_type: SourceDialogSourceType::Loki,
                    fields: [String::new(), String::new(), String::new()],
                    cursors: [0, 0, 0],
                    active_field: 1,
                    error: None,
                    tls: None,
                });
            }
            Action::AddSourceSubcommand => {
                self.overlay = OverlayMode::SourceDialog(SourceDialogState {
                    mode: SourceDialogMode::Add,
                    source_type: SourceDialogSourceType::Subcommand,
                    fields: [String::new(), String::new(), String::new()],
                    cursors: [0, 0, 0],
                    active_field: 1,
                    error: None,
                    tls: None,
                });
            }
            Action::OpenCommandPalette => {
                self.overlay = OverlayMode::CommandPalette(CommandPaletteState::new());
            }
            Action::SaveProfile => {
                self.overlay = OverlayMode::ProfileSaveDialog(ProfileSaveState {
                    input: String::new(),
                    cursor: 0,
                    error: None,
                });
            }
            Action::LoadProfile => {
                self.overlay = OverlayMode::ProfileLoadDialog(
                    ProfileLoadState::new(crate::profile::ProfileLoadMode::All),
                );
            }
            Action::LoadProfileSourcesOnly => {
                self.overlay = OverlayMode::ProfileLoadDialog(
                    ProfileLoadState::new(crate::profile::ProfileLoadMode::Sources),
                );
            }
            Action::LoadProfileFiltersOnly => {
                self.overlay = OverlayMode::ProfileLoadDialog(
                    ProfileLoadState::new(crate::profile::ProfileLoadMode::Filters),
                );
            }
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

        // Ctrl+T toggles source type in Add mode, clearing type-specific fields.
        if code == KeyCode::Char('t') && modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(state.mode, super::SourceDialogMode::Add) {
                state.source_type = match state.source_type {
                    SourceDialogSourceType::Loki => SourceDialogSourceType::Subcommand,
                    SourceDialogSourceType::Subcommand => SourceDialogSourceType::Loki,
                };
                state.fields[1].clear();
                state.fields[2].clear();
                state.cursors[1] = 0;
                state.cursors[2] = 0;
                state.error = None;
                state.active_field = 1;
            }
            return;
        }

        // Determine which fields are editable.
        let editable: &[usize] = match (&state.mode, state.source_type) {
            (super::SourceDialogMode::Add, SourceDialogSourceType::Loki) => &[0, 1, 2],
            (super::SourceDialogMode::Add, SourceDialogSourceType::Subcommand) => &[0, 1],
            (super::SourceDialogMode::Edit { .. }, _) => &[2],
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

    fn handle_command_palette_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Esc, _) => {
                self.overlay = OverlayMode::None;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                state.selected = state.selected.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                let max = state.filtered_indices().len().saturating_sub(1);
                state.selected = (state.selected + 1).min(max);
            }
            (KeyCode::Enter, _) => {
                let state = match &self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                let indices = state.filtered_indices();
                if let Some(&reg_idx) = indices.get(state.selected) {
                    let action = COMMAND_REGISTRY[reg_idx].action;
                    self.overlay = OverlayMode::None;
                    self.dispatch_action(action);
                }
            }
            (KeyCode::Backspace, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                if state.cursor > 0 {
                    state.cursor -= 1;
                    state.input.remove(state.cursor);
                    state.selected = 0;
                }
            }
            (KeyCode::Delete, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                if state.cursor < state.input.len() {
                    state.input.remove(state.cursor);
                    state.selected = 0;
                }
            }
            (KeyCode::Left, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                state.cursor = state.cursor.saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                state.cursor = (state.cursor + 1).min(state.input.len());
            }
            (KeyCode::Home, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                state.cursor = 0;
            }
            (KeyCode::End, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                state.cursor = state.input.len();
            }
            (KeyCode::Char(c), _) => {
                let state = match &mut self.overlay {
                    OverlayMode::CommandPalette(s) => s,
                    _ => return,
                };
                state.input.insert(state.cursor, c);
                state.cursor += 1;
                state.selected = 0;
            }
            _ => {}
        }
    }

    fn handle_profile_save_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                self.overlay = OverlayMode::None;
            }
            KeyCode::Enter => {
                let input = match &self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s.input.trim().to_string(),
                    _ => return,
                };
                if input.is_empty() {
                    let state = match &mut self.overlay {
                        OverlayMode::ProfileSaveDialog(s) => s,
                        _ => return,
                    };
                    state.error = Some("Name is required".to_string());
                    return;
                }
                self.save_profile(&input);
            }
            KeyCode::Backspace => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                if state.cursor > 0 {
                    state.cursor -= 1;
                    state.input.remove(state.cursor);
                    state.error = None;
                }
            }
            KeyCode::Delete => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                if state.cursor < state.input.len() {
                    state.input.remove(state.cursor);
                    state.error = None;
                }
            }
            KeyCode::Left => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                state.cursor = state.cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                state.cursor = (state.cursor + 1).min(state.input.len());
            }
            KeyCode::Home => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                state.cursor = 0;
            }
            KeyCode::End => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                state.cursor = state.input.len();
            }
            KeyCode::Char(c) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileSaveDialog(s) => s,
                    _ => return,
                };
                state.input.insert(state.cursor, c);
                state.cursor += 1;
                state.error = None;
            }
            _ => {}
        }
    }

    fn handle_profile_load_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Esc, _) => {
                self.overlay = OverlayMode::None;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                state.cursor = state.cursor.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                let max = state.filtered_indices().len().saturating_sub(1);
                state.cursor = (state.cursor + 1).min(max);
            }
            (KeyCode::Enter, _) => {
                let (path, mode) = match &self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => {
                        let filtered = s.filtered_indices();
                        if filtered.is_empty() && !s.input.is_empty() {
                            // Treat input as a custom path.
                            (std::path::PathBuf::from(&s.input), s.load_mode.clone())
                        } else if let Some(&idx) = filtered.get(s.cursor) {
                            (s.profiles[idx].1.clone(), s.load_mode.clone())
                        } else {
                            return;
                        }
                    }
                    _ => return,
                };
                self.overlay = OverlayMode::None;
                self.load_profile(&path, &mode);
            }
            (KeyCode::Backspace, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                if state.input_cursor > 0 {
                    state.input_cursor -= 1;
                    state.input.remove(state.input_cursor);
                    state.cursor = 0;
                }
            }
            (KeyCode::Delete, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                if state.input_cursor < state.input.len() {
                    state.input.remove(state.input_cursor);
                    state.cursor = 0;
                }
            }
            (KeyCode::Left, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                state.input_cursor = state.input_cursor.saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                state.input_cursor = (state.input_cursor + 1).min(state.input.len());
            }
            (KeyCode::Home, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                state.input_cursor = 0;
            }
            (KeyCode::End, _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                state.input_cursor = state.input.len();
            }
            (KeyCode::Char(c), _) => {
                let state = match &mut self.overlay {
                    OverlayMode::ProfileLoadDialog(s) => s,
                    _ => return,
                };
                state.input.insert(state.input_cursor, c);
                state.input_cursor += 1;
                state.cursor = 0;
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

    fn scroll_to_next_day(&mut self) {
        let current = match &self.scroll {
            ScrollState::Selected(idx) => *idx,
            ScrollState::Tail => return,
        };
        if let Some(&target) = self.day_transitions.iter().find(|&&p| p > current) {
            self.scroll = ScrollState::Selected(target);
            self.h_scroll = 0;
            self.v_scroll = 0;
        }
    }

    fn scroll_to_prev_day(&mut self) {
        let current = match &self.scroll {
            ScrollState::Tail => self.current_entry_count,
            ScrollState::Selected(idx) => *idx,
        };
        if let Some(&target) = self.day_transitions.iter().rev().find(|&&p| p < current) {
            self.scroll = ScrollState::Selected(target);
            self.h_scroll = 0;
            self.v_scroll = 0;
        }
    }
}
