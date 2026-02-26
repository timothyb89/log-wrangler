use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::filter::{Filter, FilterMode, FilterTarget};
use crate::log::{Arena, LogView, ViewPath};

/// The mode the bottom toolbar is in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolbarMode {
    Normal,
    FilterEntry,
}

/// Which filter matching mode is active during filter entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterEntryMode {
    Substring,
    Regex,
}

/// Display mode for log content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    /// Show raw message text as-is.
    Raw,
    /// Parse nested JSON and show level/message/labels columns.
    Pretty,
}

/// Scroll state for the main log list.
#[derive(Debug, Clone)]
enum ScrollState {
    /// No entry highlighted, auto-scroll to bottom showing newest entries.
    Tail,
    /// A specific entry index (within the current LogView's entries vec) is highlighted.
    Selected(usize),
}

pub(crate) struct App {
    arena: Arc<Mutex<Arena>>,

    /// Path to the currently viewed LogView node in the filter tree.
    view_path: ViewPath,

    /// Vertical scroll state.
    scroll: ScrollState,

    /// Horizontal scroll offset for the currently selected entry's message.
    h_scroll: usize,

    /// Current toolbar mode.
    toolbar_mode: ToolbarMode,

    /// Current filter entry mode (substring vs regex).
    filter_entry_mode: FilterEntryMode,

    /// Text buffer for filter input.
    filter_input: String,

    /// Cursor position within filter_input.
    filter_cursor: usize,

    /// Whether the filter being entered is inverted (excludes matches).
    filter_inverted: bool,

    /// Whether the app should exit.
    should_quit: bool,

    /// Cached count of entries in the current view (updated each tick).
    current_entry_count: usize,

    /// Viewport height from the last render, used for page scroll calculations.
    viewport_height: usize,

    /// Cursor position within the tree-select overlay (None = overlay closed).
    tree_select_cursor: Option<usize>,

    /// Current display mode (raw vs pretty).
    display_mode: DisplayMode,

    /// Map from screen row offset (relative to log list body top) to view
    /// entry index. Populated each render frame; used by mouse click handler.
    visible_row_map: Vec<usize>,

    /// The absolute Y coordinate where the log list body starts (after header).
    log_list_body_y: u16,
}

impl App {
    fn new(arena: Arc<Mutex<Arena>>) -> Self {
        Self {
            arena,
            view_path: Vec::new(),
            scroll: ScrollState::Tail,
            h_scroll: 0,
            toolbar_mode: ToolbarMode::Normal,
            filter_entry_mode: FilterEntryMode::Substring,
            filter_input: String::new(),
            filter_cursor: 0,
            filter_inverted: false,
            should_quit: false,
            current_entry_count: 0,
            viewport_height: 0,
            tree_select_cursor: None,
            display_mode: DisplayMode::Pretty,
            visible_row_map: Vec::new(),
            log_list_body_y: 0,
        }
    }

    fn update_from_arena(&mut self) {
        if let Ok(arena) = self.arena.lock() {
            self.current_entry_count = arena.view_at(&self.view_path).entries.len();
        }
    }

    // --- Event handling ---

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                if self.tree_select_cursor.is_some() {
                    self.handle_tree_select_key(key.code, key.modifiers);
                    return;
                }
                match self.toolbar_mode {
                    ToolbarMode::Normal => self.handle_normal_key(key.code, key.modifiers),
                    ToolbarMode::FilterEntry => self.handle_filter_key(key.code, key.modifiers),
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => self.scroll_down(),
                MouseEventKind::ScrollUp => self.scroll_up(),
                MouseEventKind::Down(MouseButton::Left) => {
                    if self.tree_select_cursor.is_some() {
                        return;
                    }
                    if mouse.row >= self.log_list_body_y {
                        let offset = (mouse.row - self.log_list_body_y) as usize;
                        if let Some(&view_idx) = self.visible_row_map.get(offset) {
                            self.scroll = ScrollState::Selected(view_idx);
                            self.h_scroll = 0;
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
            (KeyCode::Down | KeyCode::Char('j'), _) => self.scroll_down(),
            (KeyCode::Up | KeyCode::Char('k'), _) => self.scroll_up(),
            (KeyCode::Char('G') | KeyCode::End, _) => {
                self.scroll = ScrollState::Tail;
                self.h_scroll = 0;
            }
            (KeyCode::Char('g') | KeyCode::Home, _) => {
                if self.current_entry_count > 0 {
                    self.scroll = ScrollState::Selected(0);
                    self.h_scroll = 0;
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
            (KeyCode::Char('v'), _) => {
                self.display_mode = match self.display_mode {
                    DisplayMode::Raw => DisplayMode::Pretty,
                    DisplayMode::Pretty => DisplayMode::Raw,
                };
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

    // --- Scrolling ---

    fn scroll_down(&mut self) {
        match &self.scroll {
            ScrollState::Tail => {}
            ScrollState::Selected(idx) => {
                let next = idx + 1;
                if next >= self.current_entry_count {
                    self.scroll = ScrollState::Tail;
                } else {
                    self.scroll = ScrollState::Selected(next);
                }
                self.h_scroll = 0;
            }
        }
    }

    fn scroll_up(&mut self) {
        match &self.scroll {
            ScrollState::Tail => {
                if self.current_entry_count > 0 {
                    self.scroll = ScrollState::Selected(self.current_entry_count - 1);
                    self.h_scroll = 0;
                }
            }
            ScrollState::Selected(idx) => {
                if *idx > 0 {
                    self.scroll = ScrollState::Selected(idx - 1);
                    self.h_scroll = 0;
                }
            }
        }
    }

    fn scroll_page_down(&mut self) {
        let page = self.viewport_height.max(1);
        match &self.scroll {
            ScrollState::Tail => {}
            ScrollState::Selected(idx) => {
                let next = idx + page;
                if next >= self.current_entry_count {
                    self.scroll = ScrollState::Tail;
                } else {
                    self.scroll = ScrollState::Selected(next);
                }
                self.h_scroll = 0;
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
                }
            }
            ScrollState::Selected(idx) => {
                self.scroll = ScrollState::Selected(idx.saturating_sub(page));
                self.h_scroll = 0;
            }
        }
    }

    // --- Filter operations ---

    fn apply_filter(&mut self) {
        if self.filter_input.is_empty() {
            self.toolbar_mode = ToolbarMode::Normal;
            return;
        }

        let mode = match self.filter_entry_mode {
            FilterEntryMode::Substring => FilterMode::Substring(self.filter_input.clone()),
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
        let mut child_entries = Vec::new();
        for &arena_idx in &parent.entries {
            let resolved = arena.resolve_entry(arena_idx);
            // Compute raw match across all fields, then apply inversion.
            let raw = filter.raw_matches(resolved.message)
                || resolved
                    .labels
                    .iter()
                    .any(|(_, v)| filter.raw_matches(v));
            let matches = if filter.inverted { !raw } else { raw };
            if matches {
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

        // Navigate into the new child.
        self.view_path.push(child_idx);
        self.scroll = ScrollState::Tail;
        self.h_scroll = 0;
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();

        self.filter_input.clear();
        self.filter_cursor = 0;
        self.filter_inverted = false;
        self.toolbar_mode = ToolbarMode::Normal;
    }

    fn pop_filter(&mut self) {
        if self.view_path.is_empty() {
            return;
        }
        self.view_path.pop();
        self.scroll = ScrollState::Tail;
        self.h_scroll = 0;
        self.update_from_arena();
    }

    /// Navigate to the parent view and remove the current child branch from the
    /// tree. If an entry is selected, re-select it in the parent view.
    fn pop_and_remove_filter(&mut self) {
        if self.view_path.is_empty() {
            return;
        }

        let child_idx = *self.view_path.last().unwrap();

        // Move to the parent path first so we can address the child through it.
        self.view_path.pop();
        self.h_scroll = 0;

        let Ok(mut arena) = self.arena.lock() else {
            self.scroll = ScrollState::Tail;
            return;
        };

        // Capture the arena index of the currently selected entry (if any)
        // before the child view is destroyed.
        let selected_arena_idx: Option<usize> = if let ScrollState::Selected(view_idx) = self.scroll {
            let parent = arena.view_at(&self.view_path);
            if let Some(child) = parent.children.get(child_idx) {
                let clamped = view_idx.min(child.entries.len().saturating_sub(1));
                child.entries.get(clamped).copied()
            } else {
                None
            }
        } else {
            None
        };

        // Remove the child branch.
        {
            let parent = arena.view_at_mut(&self.view_path);
            parent.children.remove(child_idx);
        }

        // Re-select the same entry in the parent view, if possible.
        let new_scroll = if let Some(target) = selected_arena_idx {
            let parent = arena.view_at(&self.view_path);
            if let Some(pos) = parent.entries.iter().position(|&e| e == target) {
                ScrollState::Selected(pos)
            } else {
                ScrollState::Tail
            }
        } else {
            ScrollState::Tail
        };

        self.scroll = new_scroll;
        self.current_entry_count = arena.view_at(&self.view_path).entries.len();
    }

    fn navigate_sibling(&mut self, direction: i32) {
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

        *self.view_path.last_mut().unwrap() = new_idx;
        self.scroll = ScrollState::Tail;
        self.h_scroll = 0;
    }

    // --- Tree select ---

    fn enter_tree_select(&mut self) {
        let Ok(arena) = self.arena.lock() else { return };
        let mut flat: Vec<(ViewPath, String)> = Vec::new();
        let mut path: ViewPath = Vec::new();
        Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat);
        let cursor = flat.iter().position(|(p, _)| *p == self.view_path).unwrap_or(0);
        self.tree_select_cursor = Some(cursor);
    }

    fn handle_tree_select_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) {
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
                    self.view_path = selected_path.clone();
                    self.scroll = ScrollState::Tail;
                    self.h_scroll = 0;
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
    fn flatten_view_tree(
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
                        FilterMode::Substring(p) => format!("{}\"{}\"", prefix, p),
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

    // --- Rendering ---

    fn render(&mut self, frame: &mut Frame) {
        let arena_ref = self.arena.clone();
        let Ok(arena) = arena_ref.lock() else {
            return;
        };

        let view = arena.view_at(&self.view_path);
        self.current_entry_count = view.entries.len();

        let chunks = Layout::vertical([
            Constraint::Min(1),    // log list
            Constraint::Length(1), // toolbar
        ])
        .split(frame.area());

        self.viewport_height = chunks[0].height as usize;
        self.render_log_list(frame, chunks[0], &arena, view);
        self.render_toolbar(frame, chunks[1], &arena, view);

        if self.tree_select_cursor.is_some() {
            self.render_tree_select_overlay(frame, frame.area(), &arena);
        }
    }

    fn render_log_list(
        &mut self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
        view: &LogView,
    ) {
        match self.display_mode {
            DisplayMode::Raw => self.render_log_list_raw(frame, area, arena, view),
            DisplayMode::Pretty => self.render_log_list_pretty(frame, area, arena, view),
        }
    }

    fn render_log_list_raw(
        &mut self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
        view: &LogView,
    ) {
        let visible_height = area.height as usize;
        let entries = &view.entries;
        let total = entries.len();

        if total == 0 {
            let empty = Paragraph::new("No log entries")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty, area);
            return;
        }

        let (start_idx, selected_view_idx) = match &self.scroll {
            ScrollState::Tail => {
                let start = total.saturating_sub(visible_height);
                (start, None)
            }
            ScrollState::Selected(sel) => {
                let sel = (*sel).min(total.saturating_sub(1));
                let half = visible_height / 2;
                let start = sel
                    .saturating_sub(half)
                    .min(total.saturating_sub(visible_height));
                (start, Some(sel))
            }
        };

        let end_idx = (start_idx + visible_height).min(total);

        // Populate row map for mouse click support (all rows height 1 in raw mode).
        self.visible_row_map.clear();
        self.log_list_body_y = area.y + 1; // +1 for header row
        for view_idx in start_idx..end_idx {
            self.visible_row_map.push(view_idx);
        }

        let rows: Vec<Row> = (start_idx..end_idx)
            .map(|view_idx| {
                let arena_idx = entries[view_idx];
                let resolved = arena.resolve_entry(arena_idx);

                let timestamp_str = format!("{}", resolved.timestamp.strftime("%H:%M:%S%.3f"));

                let message = if Some(view_idx) == selected_view_idx {
                    resolved.message.chars().skip(self.h_scroll).collect()
                } else {
                    resolved.message.to_string()
                };

                Row::new(vec![Cell::from(timestamp_str), Cell::from(message)])
            })
            .collect();

        let widths = [Constraint::Length(15), Constraint::Min(1)];

        let table = Table::new(rows, widths)
            .header(
                Row::new(vec!["Time", "Message"])
                    .style(Style::default().bold().fg(Color::Cyan)),
            )
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White));

        let mut table_state = TableState::default();
        if let Some(sel) = selected_view_idx {
            table_state.select(Some(sel - start_idx));
        }

        frame.render_stateful_widget(table, area, &mut table_state);
    }

    fn render_log_list_pretty(
        &mut self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
        view: &LogView,
    ) {
        let visible_height = area.height as usize;
        let entries = &view.entries;
        let total = entries.len();

        if total == 0 {
            let empty = Paragraph::new("No log entries")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty, area);
            return;
        }

        let selected_view_idx = match &self.scroll {
            ScrollState::Tail => None,
            ScrollState::Selected(sel) => Some((*sel).min(total.saturating_sub(1))),
        };

        // Compute start_idx. For Tail mode, work backward accounting for row
        // heights (non-selected entries may still be multi-line from newlines).
        // For Selected mode, use a centering heuristic then clamp.
        let start_idx = match &self.scroll {
            ScrollState::Tail => {
                let mut acc = 0usize;
                let mut start = total;
                while start > 0 {
                    let h = pretty_row_height(arena, entries[start - 1], false);
                    if acc + h > visible_height {
                        break;
                    }
                    acc += h;
                    start -= 1;
                }
                start
            }
            ScrollState::Selected(sel) => {
                let sel = (*sel).min(total.saturating_sub(1));
                let half = visible_height / 2;
                sel.saturating_sub(half).min(total.saturating_sub(1))
            }
        };

        // Build rows from start_idx, accumulating heights until viewport full.
        self.visible_row_map.clear();
        self.log_list_body_y = area.y + 1; // +1 for header row
        let mut rows: Vec<Row> = Vec::new();
        let mut accumulated = 0usize;
        let mut table_select_idx: Option<usize> = None;

        for view_idx in start_idx..total {
            if accumulated >= visible_height {
                break;
            }

            let arena_idx = entries[view_idx];
            let resolved = arena.resolve_entry(arena_idx);
            let is_selected = Some(view_idx) == selected_view_idx;

            let display_msg = resolved.inner_message.unwrap_or(resolved.message);
            let msg_lines: Vec<&str> = display_msg.lines().collect();
            let msg_line_count = msg_lines.len().max(1);

            // Merge logcli labels + structured fields for display.
            let all_labels: Vec<(&str, &str)> = resolved
                .labels
                .iter()
                .chain(resolved.structured_fields.iter())
                .copied()
                .collect();

            let label_lines = if is_selected { all_labels.len() } else { 0 };
            let row_height = msg_line_count + label_lines;

            // Indicator column: mark multi-line entries.
            let indicator = if row_height > 1 {
                let lines: Vec<Line> = (0..row_height).map(|_| Line::from("│")).collect();
                Cell::from(Text::from(lines))
            } else {
                Cell::from(" ")
            };

            // Timestamp.
            let timestamp_str = format!("{}", resolved.timestamp.strftime("%H:%M:%S%.3f"));

            // Level with semantic color.
            let level_cell = if let Some(lvl) = resolved.level {
                Cell::from(Span::styled(level_display(lvl), level_style(lvl)))
            } else {
                Cell::from("")
            };

            // Content column.
            let content = if is_selected {
                // Expanded: message lines + one line per label.
                let mut lines: Vec<Line> = msg_lines
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        if i == 0 {
                            let chars: String = l.chars().skip(self.h_scroll).collect();
                            Line::from(chars)
                        } else {
                            Line::from(l.to_string())
                        }
                    })
                    .collect();
                for (k, v) in &all_labels {
                    lines.push(Line::from(Span::styled(
                        format!("  {}: {}", k, v),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                Cell::from(Text::from(lines))
            } else if msg_line_count > 1 {
                // Multi-line message (not selected): show all lines, abridged labels on first.
                let mut lines: Vec<Line> = Vec::with_capacity(msg_line_count);
                for (i, l) in msg_lines.iter().enumerate() {
                    if i == 0 && !all_labels.is_empty() {
                        let mut label_suffix = String::new();
                        for (k, v) in &all_labels {
                            label_suffix.push_str(&format!("  {}={}", k, v));
                        }
                        lines.push(Line::from(vec![
                            Span::raw(l.to_string()),
                            Span::styled(label_suffix, Style::default().fg(Color::DarkGray)),
                        ]));
                    } else {
                        lines.push(Line::from(l.to_string()));
                    }
                }
                Cell::from(Text::from(lines))
            } else {
                // Single-line: message + inline abridged labels.
                if all_labels.is_empty() {
                    Cell::from(display_msg)
                } else {
                    let mut label_suffix = String::new();
                    for (k, v) in &all_labels {
                        label_suffix.push_str(&format!("  {}={}", k, v));
                    }
                    Cell::from(Line::from(vec![
                        Span::raw(display_msg.to_string()),
                        Span::styled(label_suffix, Style::default().fg(Color::DarkGray)),
                    ]))
                }
            };

            if is_selected {
                table_select_idx = Some(rows.len());
            }

            // Extend row map: each screen row within this entry maps to view_idx.
            for _ in 0..row_height {
                self.visible_row_map.push(view_idx);
            }

            rows.push(
                Row::new(vec![indicator, Cell::from(timestamp_str), level_cell, content])
                    .height(row_height as u16),
            );
            accumulated += row_height;
        }

        let widths = [
            Constraint::Length(1),  // indicator
            Constraint::Length(15), // time
            Constraint::Length(5),  // level
            Constraint::Min(1),    // content
        ];

        let table = Table::new(rows, widths)
            .header(
                Row::new(vec![" ", "Time", "Level", "Message"])
                    .style(Style::default().bold().fg(Color::Cyan)),
            )
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White));

        let mut table_state = TableState::default();
        table_state.select(table_select_idx);

        frame.render_stateful_widget(table, area, &mut table_state);
    }

    fn render_toolbar(
        &self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
        view: &LogView,
    ) {
        match &self.toolbar_mode {
            ToolbarMode::Normal => {
                let filter_depth = self.view_path.len();
                let entry_count = view.entries.len();
                let total_count = arena.entries.len();
                let mode_indicator = match self.scroll {
                    ScrollState::Tail => "TAIL",
                    ScrollState::Selected(_) => "SCROLL",
                };

                let display_label = match self.display_mode {
                    DisplayMode::Raw => "RAW",
                    DisplayMode::Pretty => "PRETTY",
                };

                let status = format!(
                    " {} | {} | Filters: {} | View: {}/{} entries | q:quit /:filter v:view",
                    mode_indicator, display_label, filter_depth, entry_count, total_count,
                );

                let paragraph = Paragraph::new(Line::from(status))
                    .style(Style::default().bg(Color::Blue).fg(Color::White));
                frame.render_widget(paragraph, area);
            }
            ToolbarMode::FilterEntry => {
                let mode_label = match self.filter_entry_mode {
                    FilterEntryMode::Substring => "SUB",
                    FilterEntryMode::Regex => "RGX",
                };

                let base_style = Style::default().bg(Color::Yellow).fg(Color::Black);

                let mut spans = vec![
                    Span::styled(format!(" [{}]", mode_label), base_style),
                ];

                if self.filter_inverted {
                    spans.push(Span::styled(
                        " NOT",
                        Style::default().bg(Color::Red).fg(Color::White).bold(),
                    ));
                }

                let filter_text = format!(" Filter: {}", self.filter_input);
                spans.push(Span::styled(filter_text, base_style));

                // Fill remaining width with background color.
                let used: usize = spans.iter().map(|s| s.content.len()).sum();
                let remaining = (area.width as usize).saturating_sub(used);
                if remaining > 0 {
                    spans.push(Span::styled(" ".repeat(remaining), base_style));
                }

                let paragraph = Paragraph::new(Line::from(spans));
                frame.render_widget(paragraph, area);

                // Position cursor within the filter input.
                // prefix spans: " [MODE]" + optional " NOT" + " Filter: "
                let prefix_len = 2 + mode_label.len() + 1
                    + if self.filter_inverted { 4 } else { 0 }
                    + 9;
                let cursor_x =
                    area.x + prefix_len as u16 + self.filter_cursor as u16;
                frame.set_cursor_position((cursor_x, area.y));
            }
        }
    }

    fn render_tree_select_overlay(
        &self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
    ) {
        let cursor = match self.tree_select_cursor {
            Some(c) => c,
            None => return,
        };

        let mut flat: Vec<(ViewPath, String)> = Vec::new();
        let mut path: ViewPath = Vec::new();
        Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat);

        let popup_area = centered_rect(72, 65, area);
        frame.render_widget(Clear, popup_area);

        let items: Vec<ListItem> = flat
            .iter()
            .map(|(p, label)| {
                let style = if *p == self.view_path {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };
                ListItem::new(label.as_str()).style(style)
            })
            .collect();

        let cursor = cursor.min(flat.len().saturating_sub(1));
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Views ")
                    .title_bottom(" ↑↓ / j k : navigate   Enter / Tab : go   Esc : cancel "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
            .highlight_symbol("> ");

        let mut list_state = ListState::default();
        list_state.select(Some(cursor));
        frame.render_stateful_widget(list, popup_area, &mut list_state);
    }
}

/// Compute the row height for an entry in pretty mode.
fn pretty_row_height(arena: &Arena, arena_idx: usize, is_selected: bool) -> usize {
    let resolved = arena.resolve_entry(arena_idx);
    let msg = resolved.inner_message.unwrap_or(resolved.message);
    let msg_lines = msg.lines().count().max(1);
    let label_lines = if is_selected {
        resolved.labels.len() + resolved.structured_fields.len()
    } else {
        0
    };
    msg_lines + label_lines
}

fn level_style(level: &str) -> Style {
    match level.to_ascii_lowercase().as_str() {
        "trace" => Style::default().fg(Color::DarkGray),
        "debug" => Style::default().fg(Color::Cyan),
        "info" => Style::default().fg(Color::Green),
        "warn" | "warning" => Style::default().fg(Color::Yellow),
        "error" | "err" => Style::default().fg(Color::Red),
        "fatal" | "panic" | "critical" | "dpanic" => Style::default().fg(Color::Red).bold(),
        _ => Style::default(),
    }
}

fn level_display(level: &str) -> String {
    let upper = level.to_ascii_uppercase();
    let display = match upper.as_str() {
        "WARNING" => "WARN",
        other if other.len() > 5 => &other[..5],
        other => other,
    };
    format!("{:<5}", display)
}

/// Return a centered `Rect` carved out of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

/// Run the TUI event loop. This takes ownership of stdout for rendering.
pub(crate) async fn run_tui(arena: Arc<Mutex<Arena>>) -> Result<()> {
    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(arena);
    let mut event_stream = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => app.handle_event(event),
                    Some(Err(_)) => break,
                    None => break,
                }
            }
            _ = tick_interval.tick() => {
                app.update_from_arena();
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Teardown.
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
