use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};
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

    /// Whether the app should exit.
    should_quit: bool,

    /// Cached count of entries in the current view (updated each tick).
    current_entry_count: usize,

    /// Viewport height from the last render, used for page scroll calculations.
    viewport_height: usize,
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
            should_quit: false,
            current_entry_count: 0,
            viewport_height: 0,
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
                match self.toolbar_mode {
                    ToolbarMode::Normal => self.handle_normal_key(key.code, key.modifiers),
                    ToolbarMode::FilterEntry => self.handle_filter_key(key.code, key.modifiers),
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => self.scroll_down(),
                MouseEventKind::ScrollUp => self.scroll_up(),
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
            }
            (KeyCode::Backspace, _) => self.pop_filter(),
            (KeyCode::Char('p'), _) => self.pop_and_remove_filter(),
            (KeyCode::Char('['), _) => self.navigate_sibling(-1),
            (KeyCode::Char(']'), _) => self.navigate_sibling(1),
            (KeyCode::Tab, _) => self.navigate_child(),
            _ => {}
        }
    }

    fn handle_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Esc, _) => {
                self.toolbar_mode = ToolbarMode::Normal;
                self.filter_input.clear();
                self.filter_cursor = 0;
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
            inverted: false,
        };

        let Ok(mut arena) = self.arena.lock() else {
            return;
        };

        // Build child LogView by testing parent entries against the filter.
        let parent = arena.view_at(&self.view_path);
        let mut child_entries = Vec::new();
        for &arena_idx in &parent.entries {
            let resolved = arena.resolve_entry(arena_idx);
            let matches = filter.matches(resolved.message)
                || resolved
                    .labels
                    .iter()
                    .any(|(_, v)| filter.matches(v));
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

    fn navigate_child(&mut self) {
        let Ok(arena) = self.arena.lock() else {
            return;
        };

        let view = arena.view_at(&self.view_path);
        if !view.children.is_empty() {
            drop(arena);
            self.view_path.push(0);
            self.scroll = ScrollState::Tail;
            self.h_scroll = 0;
            self.update_from_arena();
        }
    }

    // --- Rendering ---

    fn render(&mut self, frame: &mut Frame) {
        let Ok(arena) = self.arena.lock() else {
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
    }

    fn render_log_list(
        &self,
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

        // Determine visible window and selected offset within it.
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

        let rows: Vec<Row> = (start_idx..end_idx)
            .map(|view_idx| {
                let arena_idx = entries[view_idx];
                let resolved = arena.resolve_entry(arena_idx);

                let timestamp_str = format!("{}", resolved.timestamp.strftime("%H:%M:%S%.3f"));

                // Apply horizontal scroll to the selected row's message.
                let message = if Some(view_idx) == selected_view_idx {
                    let chars: String = resolved.message.chars().skip(self.h_scroll).collect();
                    chars
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

                let status = format!(
                    " {} | Filters: {} | View: {}/{} entries | q:quit /:filter",
                    mode_indicator, filter_depth, entry_count, total_count,
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

                let input_display =
                    format!(" [{}] Filter: {}", mode_label, self.filter_input);

                let paragraph = Paragraph::new(Line::from(input_display))
                    .style(Style::default().bg(Color::Yellow).fg(Color::Black));
                frame.render_widget(paragraph, area);

                // Position cursor within the filter input.
                let prefix_len = 4 + mode_label.len() + 10; // " [XXX] Filter: "
                let cursor_x =
                    area.x + prefix_len as u16 + self.filter_cursor as u16;
                frame.set_cursor_position((cursor_x, area.y));
            }
        }
    }
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
