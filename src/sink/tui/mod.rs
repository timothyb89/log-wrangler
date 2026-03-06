mod filter;
mod input;
mod render;
mod tree;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;

use crate::filter::Filter;
use crate::log::{Arena, ViewPath};
use crate::source::loki::LokiSourceParams;

/// The mode the bottom toolbar is in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolbarMode {
    Normal,
    FilterEntry,
    SearchEntry,
    QueryEntry,
}

/// Direction for search navigation.
enum Direction {
    Forward,
    Backward,
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

    /// Active search filter. When `Some`, matching entries are highlighted
    /// and n/N navigation is available.
    search: Option<Filter>,

    /// Map from screen row offset (relative to log list body top) to view
    /// entry index. Populated each render frame; used by mouse click handler.
    visible_row_map: Vec<usize>,

    /// The absolute Y coordinate where the log list body starts (after header).
    log_list_body_y: u16,

    /// Persisted viewport start index for pretty mode. Kept stable across frames
    /// so that arriving messages and selection changes within the visible range
    /// do not shift the viewport.
    pretty_viewport_start: Option<usize>,

    /// Current LogQL query string (for Loki sources).
    loki_query: String,

    /// Text buffer for query editing input.
    query_input: String,

    /// Cursor position within query_input.
    query_cursor: usize,

    /// Watch channel sender to signal Loki source restart. None for stdin.
    source_restart_tx: Option<tokio::sync::watch::Sender<Option<LokiSourceParams>>>,
}

impl App {
    fn new(
        arena: Arc<Mutex<Arena>>,
        source_restart_tx: Option<tokio::sync::watch::Sender<Option<LokiSourceParams>>>,
        initial_query: Option<String>,
    ) -> Self {
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
            search: None,
            visible_row_map: Vec::new(),
            log_list_body_y: 0,
            pretty_viewport_start: None,
            loki_query: initial_query.unwrap_or_default(),
            query_input: String::new(),
            query_cursor: 0,
            source_restart_tx,
        }
    }

    fn update_from_arena(&mut self) {
        if let Ok(arena) = self.arena.lock() {
            self.current_entry_count = arena.view_at(&self.view_path).entries.len();
        }
    }
}

/// Run the TUI event loop. This takes ownership of stdout for rendering.
pub(crate) async fn run_tui(
    arena: Arc<Mutex<Arena>>,
    restart_tx: Option<tokio::sync::watch::Sender<Option<LokiSourceParams>>>,
    initial_query: Option<String>,
) -> Result<()> {
    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(arena, restart_tx, initial_query);
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
