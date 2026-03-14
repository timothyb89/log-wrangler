mod action;
mod filter;
mod input;
mod render;
mod tree;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::MissedTickBehavior;

use color_eyre::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;

use crate::filter::Filter;
use crate::log::{Arena, ViewPath};
use crate::profile::{self, ProfileLoadMode};
use crate::source::SourceMessage;
use crate::source::loki::LokiSourceParams;
use crate::source::teleport::TeleportTlsConfig;

/// The mode the bottom toolbar is in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolbarMode {
    Normal,
    FilterEntry,
    SearchEntry,
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

/// Timezone display mode for timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimezoneMode {
    /// Display timestamps in the system's local timezone.
    Local,
    /// Display timestamps in UTC.
    Utc,
}

/// Scroll state for the main log list.
#[derive(Debug, Clone)]
enum ScrollState {
    /// No entry highlighted, auto-scroll to bottom showing newest entries.
    Tail,
    /// A specific entry index (within the current LogView's entries vec) is highlighted.
    Selected(usize),
}

/// Which modal overlay is currently open (if any).
enum OverlayMode {
    None,
    TreeSelect { cursor: usize },
    SourceSelect { cursor: usize },
    SourceDialog(SourceDialogState),
    CommandPalette(CommandPaletteState),
    ProfileSaveDialog(ProfileSaveState),
    ProfileLoadDialog(ProfileLoadState),
}

/// State for the command palette overlay.
struct CommandPaletteState {
    /// Text buffer for the search input.
    input: String,
    /// Cursor position within the input.
    cursor: usize,
    /// Index of the selected item in the filtered results list.
    selected: usize,
    /// List area from the last render, used for mouse click hit-testing.
    list_area: ratatui::layout::Rect,
}

impl CommandPaletteState {
    fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            selected: 0,
            list_area: ratatui::layout::Rect::default(),
        }
    }

    /// Return indices into `COMMAND_REGISTRY` that match the current input
    /// (case-insensitive substring match).
    fn filtered_indices(&self) -> Vec<usize> {
        let registry = action::COMMAND_REGISTRY;
        if self.input.is_empty() {
            return (0..registry.len()).collect();
        }
        let query = self.input.to_lowercase();
        registry
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.name.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }
}

/// State for the profile save dialog.
struct ProfileSaveState {
    /// Text buffer for the profile name (or path).
    input: String,
    cursor: usize,
    error: Option<String>,
}

/// State for the profile load dialog.
struct ProfileLoadState {
    /// Cached list of discovered profiles.
    profiles: Vec<(String, std::path::PathBuf)>,
    /// Selected index in the profile list.
    cursor: usize,
    /// Which parts of the profile to load.
    load_mode: ProfileLoadMode,
    /// Text input for filtering the list or entering a custom path.
    input: String,
    input_cursor: usize,
}

impl ProfileLoadState {
    fn new(load_mode: ProfileLoadMode) -> Self {
        let profiles = profile::list_profiles().unwrap_or_default();
        Self {
            profiles,
            cursor: 0,
            load_mode,
            input: String::new(),
            input_cursor: 0,
        }
    }

    /// Return indices into `self.profiles` matching the current input filter.
    fn filtered_indices(&self) -> Vec<usize> {
        if self.input.is_empty() {
            return (0..self.profiles.len()).collect();
        }
        let query = self.input.to_lowercase();
        self.profiles
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| name.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Which kind of source the add-source dialog is creating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceDialogSourceType {
    Loki,
    Subcommand,
}

/// State for the add/edit source dialog overlay.
struct SourceDialogState {
    mode: SourceDialogMode,
    /// Source type being created (only relevant in Add mode).
    source_type: SourceDialogSourceType,
    /// Field storage:
    ///   Loki:       [name, url, query]
    ///   Subcommand: [name, command, ""]
    fields: [String; 3],
    /// Cursor position per field.
    cursors: [usize; 3],
    /// Index of the currently active (focused) field.
    active_field: usize,
    /// Validation error to display, if any.
    error: Option<String>,
    /// Teleport TLS config carried over when cloning a Teleport source.
    /// `None` for manually-entered (plain HTTP/HTTPS) sources.
    tls: Option<TeleportTlsConfig>,
}

enum SourceDialogMode {
    /// All fields editable.
    Add,
    /// Only query editable. `source_idx` indexes into `App::sources`.
    Edit { source_idx: usize },
}

/// Source-type-specific state needed to manage a running source.
pub(crate) enum ManagedSourceKind {
    Stdin,
    Loki {
        base_url: url::Url,
        query: String,
        tx: tokio::sync::watch::Sender<Option<LokiSourceParams>>,
        /// Set when this source was created from a `grafana+loki+teleport://`
        /// URI. Carries the mTLS credentials so they can be reused when the
        /// source is cloned or its query is restarted.
        tls: Option<TeleportTlsConfig>,
    },
    /// A running child process. Dropping `kill_tx` (or sending on it) kills
    /// the child.
    Subcommand {
        command: String,
        #[allow(dead_code)]
        kill_tx: tokio::sync::oneshot::Sender<()>,
    },
}

/// State for a managed (stoppable/editable) source.
pub(crate) struct ManagedSource {
    pub source_id: u16,
    pub name: String,
    pub kind: ManagedSourceKind,
}

pub(crate) struct App {
    arena: Arc<Mutex<Arena>>,

    /// Path to the currently viewed LogView node in the filter tree.
    view_path: ViewPath,

    /// Vertical scroll state.
    scroll: ScrollState,

    /// Horizontal scroll offset for the currently selected entry's message.
    h_scroll: usize,

    /// Vertical line offset within the currently selected entry in pretty mode.
    /// When a selected entry is taller than the viewport, this tracks how far
    /// down within that entry the user has scrolled. Reset to 0 when moving
    /// to a different entry.
    v_scroll: usize,

    /// Cached total height (in lines) of the currently selected entry in
    /// pretty mode. Set during render, read during input handling.
    selected_entry_height: Option<usize>,

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

    /// Currently open modal overlay.
    overlay: OverlayMode,

    /// Current display mode (raw vs pretty).
    display_mode: DisplayMode,

    /// Current timezone display mode (local vs UTC).
    timezone_mode: TimezoneMode,

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

    /// Managed sources (stoppable/editable). Empty when no managed sources exist.
    sources: Vec<ManagedSource>,

    /// Sender for the log ingest channel. Cloned for each dynamically-added source.
    ingest_tx: mpsc::Sender<SourceMessage>,

    /// Next source ID to assign when adding a source at runtime.
    next_source_id: u16,
}

impl App {
    fn new(
        arena: Arc<Mutex<Arena>>,
        sources: Vec<ManagedSource>,
        ingest_tx: mpsc::Sender<SourceMessage>,
        next_source_id: u16,
    ) -> Self {
        Self {
            arena,
            view_path: Vec::new(),
            scroll: ScrollState::Tail,
            h_scroll: 0,
            v_scroll: 0,
            selected_entry_height: None,
            toolbar_mode: ToolbarMode::Normal,
            filter_entry_mode: FilterEntryMode::Substring,
            filter_input: String::new(),
            filter_cursor: 0,
            filter_inverted: false,
            should_quit: false,
            current_entry_count: 0,
            viewport_height: 0,
            overlay: OverlayMode::None,
            display_mode: DisplayMode::Pretty,
            timezone_mode: TimezoneMode::Local,
            search: None,
            visible_row_map: Vec::new(),
            log_list_body_y: 0,
            pretty_viewport_start: None,
            sources,
            ingest_tx,
            next_source_id,
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
    sources: Vec<ManagedSource>,
    ingest_tx: mpsc::Sender<SourceMessage>,
    next_source_id: u16,
) -> Result<()> {
    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(arena, sources, ingest_tx, next_source_id);
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
    // Avoid burst re-renders when ticks are missed during a slow frame.
    tick_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Use a dedicated thread for reading crossterm events instead of EventStream.
    // EventStream has a known mutex contention issue: its background thread holds
    // the global INTERNAL_EVENT_READER lock during blocking poll(), causing
    // poll_next() to fail its 0ms try_lock.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match crossterm::event::read() {
                Ok(event) => {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Initial render.
    terminal.draw(|frame| app.render(frame))?;

    loop {
        tokio::select! {
            biased;

            Some(event) = event_rx.recv() => {
                app.handle_event(event);

                // Drain any additional queued events.
                while let Ok(event) = event_rx.try_recv() {
                    app.handle_event(event);
                }

                terminal.draw(|frame| app.render(frame))?;
            }
            _ = tick_interval.tick() => {
                // Only re-render when new log entries have arrived.
                let prev_count = app.current_entry_count;
                app.update_from_arena();
                if app.current_entry_count != prev_count {
                    terminal.draw(|frame| app.render(frame))?;
                }
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
