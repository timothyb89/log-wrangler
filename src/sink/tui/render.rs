use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Table, TableState,
};
use ratatui::Frame;

use crate::filter::FilterMode;
use crate::log::{Arena, LogView};
use crate::util::INTERNAL_SOURCE_ID;

use super::action::COMMAND_REGISTRY;
use super::filter::entry_matches_filter;
use super::{App, DisplayMode, FilterEntryMode, ManagedSourceKind, OverlayMode, ScrollState, SourceDialogSourceType, TimezoneMode, ToolbarMode};

/// Expand tab characters to spaces so they render visibly in the TUI.
///
/// ratatui treats `\t` as a zero-width control character, so tab-indented
/// lines (e.g. Go stack trace frames logged through journald) would otherwise
/// appear to lose their leading whitespace entirely.  A fixed width of 4 is
/// used since stack trace indentation is always a single leading tab.
fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if c == '\t' {
            out.push_str("    ");
        } else {
            out.push(c);
        }
    }
    out
}

fn format_timestamp(ts: &jiff::Zoned, mode: TimezoneMode) -> String {
    let converted = match mode {
        TimezoneMode::Local => ts.with_time_zone(jiff::tz::TimeZone::system()),
        TimezoneMode::Utc => ts.with_time_zone(jiff::tz::TimeZone::UTC),
    };
    format!("{}", converted.strftime("%H:%M:%S%.3f"))
}

/// Return the calendar date for an arena entry under the given timezone mode.
fn entry_date(arena: &Arena, arena_idx: usize, tz: TimezoneMode) -> jiff::civil::Date {
    let ts = &arena.entries[arena_idx].timestamp;
    match tz {
        TimezoneMode::Local => ts.with_time_zone(jiff::tz::TimeZone::system()).date(),
        TimezoneMode::Utc => ts.with_time_zone(jiff::tz::TimeZone::UTC).date(),
    }
}

/// Compute positions in `view.entries` that are the first occurrence of a new
/// calendar date. Position 0 is always excluded — there is no separator before
/// the very first entry, only before entries that represent a genuine day change.
fn compute_day_transitions(view: &LogView, arena: &Arena, tz: TimezoneMode) -> Vec<usize> {
    let mut seen: std::collections::HashSet<jiff::civil::Date> = Default::default();
    let mut out = Vec::new();
    for (pos, &arena_idx) in view.entries.iter().enumerate() {
        let date = entry_date(arena, arena_idx, tz);
        if seen.insert(date) && pos > 0 {
            out.push(pos);
        }
    }
    out
}

/// Build a separator row for a day transition.
///
/// `fixed_col_widths` are the widths (in chars) of all columns that precede the
/// final `Min(1)` content column.  Each is filled with `─` characters.
/// When `blank_first` is true, the first fixed column is left empty (used for
/// the indicator column in pretty mode which is normally invisible).
/// The content column gets a `── YYYY-MM-DD ──────…` label — the dash fill is
/// deliberately oversized so ratatui clips it at the column boundary.
fn make_day_separator_row(
    date: jiff::civil::Date,
    fixed_col_widths: &[u16],
    blank_first: bool,
    area_width: u16,
) -> Row<'static> {
    let date_str = format!("{}", date.strftime("%Y-%m-%d"));
    let style = Style::default().fg(Color::DarkGray);
    let mut cells: Vec<Cell<'static>> = fixed_col_widths
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            if blank_first && i == 0 {
                Cell::from("")
            } else {
                Cell::from(Span::styled("─".repeat(w as usize), style))
            }
        })
        .collect();
    // Oversized fill — ratatui clips to the actual column width.
    let fill = "─".repeat(area_width as usize);
    let content = format!("── {} {}", date_str, fill);
    cells.push(Cell::from(Span::styled(content, style)));
    Row::new(cells).style(style)
}

impl App {
    pub(super) fn render(&mut self, frame: &mut Frame) {
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

        match &self.overlay {
            OverlayMode::TreeSelect { .. } => {
                self.render_tree_select_overlay(frame, frame.area(), &arena);
            }
            OverlayMode::SourceSelect { .. } => {
                self.render_source_select_overlay(frame, frame.area(), &arena);
            }
            OverlayMode::SourceDialog(_) => {
                self.render_source_dialog(frame, frame.area());
            }
            OverlayMode::CommandPalette(_) => {
                self.render_command_palette(frame, frame.area());
            }
            OverlayMode::ProfileSaveDialog(_) => {
                self.render_profile_save_dialog(frame, frame.area());
            }
            OverlayMode::ProfileLoadDialog(_) => {
                self.render_profile_load_dialog(frame, frame.area());
            }
            OverlayMode::None => {}
        }
    }

    fn render_log_list(
        &mut self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
        view: &LogView,
    ) {
        // Show empty state prompt when no entries and no sources.
        if view.entries.is_empty() && self.sources.is_empty() {
            let msg = Paragraph::new("No sources configured. Press `s` to manage sources.")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center);
            let y = area.y + area.height / 2;
            let prompt_area = ratatui::layout::Rect::new(area.x, y, area.width, 1);
            frame.render_widget(msg, prompt_area);
            return;
        }
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

        self.day_transitions = compute_day_transitions(view, arena, self.timezone_mode);

        let preview = self.preview_filter();
        let show_source = arena.source_names.len() > 1;
        let src_w = if show_source { source_column_width(arena) } else { 0 };

        // Fixed column widths before the Min(1) content column.
        let fixed_col_widths: Vec<u16> = if show_source {
            vec![15, src_w]
        } else {
            vec![15]
        };

        // Build rows, inserting a separator before each day-transition entry.
        // Populate row map for mouse click support (all rows height 1 in raw mode).
        self.visible_row_map.clear();
        self.log_list_body_y = area.y + 1; // +1 for header row
        let mut rows: Vec<Row> = Vec::new();
        let mut line_count = 0usize;
        let mut table_select_idx: Option<usize> = None;
        let mut view_idx = start_idx;

        while view_idx < total && line_count < visible_height {
            if self.day_transitions.binary_search(&view_idx).is_ok() {
                let date = entry_date(arena, entries[view_idx], self.timezone_mode);
                rows.push(make_day_separator_row(date, &fixed_col_widths, false, area.width));
                // Clicking a separator selects the entry that follows it.
                self.visible_row_map.push(view_idx);
                line_count += 1;
                if line_count >= visible_height {
                    break;
                }
            }

            let arena_idx = entries[view_idx];
            let resolved = arena.resolve_entry(arena_idx);
            let entry = &arena.entries[arena_idx];

            let timestamp_str = format_timestamp(resolved.timestamp, self.timezone_mode);
            let message = if Some(view_idx) == selected_view_idx {
                resolved.message.chars().skip(self.h_scroll).collect()
            } else {
                resolved.message.to_string()
            };

            let mut cells = vec![Cell::from(timestamp_str)];
            if show_source {
                let name = resolve_source_name(arena, entry.source_id);
                cells.push(Cell::from(Span::styled(
                    name.to_string(),
                    source_style(entry.source_id),
                )));
            }
            cells.push(Cell::from(message));

            let mut style = Style::default();
            if let Some(ref filter) = self.search {
                if entry_matches_filter(arena, arena_idx, filter) {
                    style = style.fg(Color::Yellow).bold();
                }
            }
            if let Some(ref pf) = preview {
                if entry_matches_filter(arena, arena_idx, pf) {
                    style = style.bold();
                }
            }

            if Some(view_idx) == selected_view_idx {
                table_select_idx = Some(rows.len());
            }
            rows.push(Row::new(cells).style(style));
            self.visible_row_map.push(view_idx);
            line_count += 1;
            view_idx += 1;
        }

        let widths: Vec<Constraint> = if show_source {
            vec![Constraint::Length(15), Constraint::Length(src_w), Constraint::Min(1)]
        } else {
            vec![Constraint::Length(15), Constraint::Min(1)]
        };

        let header_cells: Vec<&str> = if show_source {
            vec!["Time", "Source", "Message"]
        } else {
            vec!["Time", "Message"]
        };

        let table = Table::new(rows, widths)
            .header(
                Row::new(header_cells)
                    .style(Style::default().bold().fg(Color::Cyan)),
            )
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White));

        let mut table_state = TableState::default();
        table_state.select(table_select_idx);

        frame.render_stateful_widget(table, area, &mut table_state);

        // Scrollbar overlay.
        let mut scrollbar_state = ScrollbarState::new(total).position(start_idx);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(Style::default().fg(Color::Gray).bg(Color::Reset))
            .thumb_style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);

        // Paint day-transition marks on the scrollbar track.
        if total > 0 {
            let buf = frame.buffer_mut();
            let track_x = area.x + area.width - 1;
            for &pos in &self.day_transitions {
                // Match ratatui's scrollbar thumb formula exactly.
                // When viewport_content_length is unset, ratatui uses:
                //   viewport = area.height
                //   max_vp   = (total - 1) + viewport
                //   thumb_y  = round(position * track_len / max_vp)
                let track_len = area.height as f64;
                let max_vp = (total - 1 + area.height as usize) as f64;
                let y = area.y + (pos as f64 * track_len / max_vp).round() as u16;
                if y < area.y + area.height {
                    if let Some(cell) = buf.cell_mut((track_x, y)) {
                        // When the mark overlaps the scrollbar thumb, preserve
                        // the filled background so both remain visible.
                        let on_thumb = cell.symbol() == "█";
                        cell.set_symbol("◆");
                        if on_thumb {
                            cell.set_fg(Color::Black);
                            cell.set_bg(Color::White);
                        } else {
                            cell.set_fg(Color::Yellow);
                        }
                    }
                }
            }
        }
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

        let show_source = arena.source_names.len() > 1;
        let src_w = if show_source { source_column_width(arena) } else { 0 };

        // Content column width for label layout calculations.
        // Columns: indicator(1) + gap(1) + time(15) + gap(1) + [source(src_w) + gap(1)] + level(5) + gap(1) + content
        let content_width = area.width.saturating_sub(
            1 + 1 + 15 + 1 + if show_source { src_w + 1 } else { 0 } + 5 + 1,
        );

        // Account for the table header row when computing available data rows.
        let data_height = visible_height.saturating_sub(1);

        // Cache selected entry height and clamp v_scroll.
        if let Some(sel) = selected_view_idx {
            let sel_h = pretty_row_height(arena, entries[sel], true, content_width);
            let max_v = sel_h.saturating_sub(data_height);
            self.v_scroll = self.v_scroll.min(max_v);
            self.selected_entry_height = Some(sel_h);
        } else {
            self.v_scroll = 0;
            self.selected_entry_height = None;
        }

        // Compute start_idx.
        //
        // Tail mode: fill from the bottom (no persistent anchor).
        // Selected mode: reuse the persisted viewport start when the selection
        // is still visible; only scroll when the selection leaves the viewport.
        // A small padding (SCROLL_PAD screen-lines) is added when the viewport
        // *does* shift so the user sees context in the scroll direction.
        const SCROLL_PAD: usize = 2;

        let start_idx = match &self.scroll {
            ScrollState::Tail => {
                self.pretty_viewport_start = None;
                let mut acc = 0usize;
                let mut start = total;
                while start > 0 {
                    let h = pretty_row_height(arena, entries[start - 1], false, content_width);
                    if acc + h > data_height {
                        break;
                    }
                    acc += h;
                    start -= 1;
                }
                start
            }
            ScrollState::Selected(sel) => {
                let sel = (*sel).min(total.saturating_sub(1));
                let sel_h = pretty_row_height(arena, entries[sel], true, content_width);

                // If the entry fills or exceeds the data area (viewport minus
                // header row), it gets the whole screen and we scroll within
                // it via v_scroll.
                if sel_h >= data_height {
                    self.pretty_viewport_start = Some(sel);
                    sel
                } else {

                // Place sel near the top of the viewport with SCROLL_PAD
                // lines of context above it.
                let pad_to_top = |sel: usize| -> usize {
                    let mut s = sel;
                    let mut pad = 0;
                    while s > 0 && pad < SCROLL_PAD {
                        pad += pretty_row_height(arena, entries[s - 1], false, content_width);
                        s -= 1;
                    }
                    s
                };

                // Place sel near the bottom of the viewport with SCROLL_PAD
                // lines of context below it.
                let pad_to_bottom = |sel: usize| -> usize {
                    let sel_h = pretty_row_height(arena, entries[sel], true, content_width);
                    let mut pad_below = 0;
                    let mut pi = sel + 1;
                    while pi < total && pad_below < SCROLL_PAD {
                        pad_below +=
                            pretty_row_height(arena, entries[pi], false, content_width);
                        pi += 1;
                    }
                    let space_above = data_height
                        .saturating_sub(sel_h)
                        .saturating_sub(pad_below);
                    let mut s = sel;
                    let mut acc = 0;
                    while s > 0 {
                        let h = pretty_row_height(
                            arena,
                            entries[s - 1],
                            false,
                            content_width,
                        );
                        if acc + h > space_above {
                            break;
                        }
                        acc += h;
                        s -= 1;
                    }
                    s
                };

                let start = if let Some(prev) = self.pretty_viewport_start {
                    let prev = prev.min(total.saturating_sub(1));

                    if sel < prev {
                        // Selection is above the viewport.
                        pad_to_top(sel)
                    } else {
                        // Find sel's screen position relative to prev.
                        let mut acc = 0usize;
                        let mut sel_screen_top: Option<usize> = None;
                        for i in prev..total {
                            let h =
                                pretty_row_height(arena, entries[i], i == sel, content_width);
                            if i == sel {
                                sel_screen_top = Some(acc);
                                break;
                            }
                            acc += h;
                            if acc >= data_height {
                                break;
                            }
                        }

                        match sel_screen_top {
                            None => {
                                // sel not reached – below viewport.
                                pad_to_bottom(sel)
                            }
                            Some(top) => {
                                let bottom = top + sel_h;
                                if bottom > data_height {
                                    // Entry extends past viewport – shift.
                                    pad_to_bottom(sel)
                                } else {
                                    // Entry is fully visible – keep stable.
                                    prev
                                }
                            }
                        }
                    }
                } else {
                    // No previous viewport – first entry into Selected mode.
                    // Anchor from the bottom using correct expanded heights.
                    let mut acc = 0usize;
                    let mut bottom_start = total;
                    while bottom_start > 0 {
                        let idx = bottom_start - 1;
                        let h =
                            pretty_row_height(arena, entries[idx], idx == sel, content_width);
                        if acc + h > data_height {
                            break;
                        }
                        acc += h;
                        bottom_start -= 1;
                    }

                    if sel >= bottom_start {
                        bottom_start
                    } else {
                        pad_to_top(sel)
                    }
                };

                self.pretty_viewport_start = Some(start);
                start

                } // end else (entry fits in viewport)
            }
        };

        self.day_transitions = compute_day_transitions(view, arena, self.timezone_mode);

        // Fixed column widths before the Min(1) content column (pretty mode).
        let pretty_fixed_widths: Vec<u16> = if show_source {
            vec![1, 15, src_w, 5]
        } else {
            vec![1, 15, 5]
        };

        // Build rows from start_idx, accumulating heights until viewport full.
        let preview = self.preview_filter();
        self.visible_row_map.clear();
        self.log_list_body_y = area.y + 1; // +1 for header row
        let mut rows: Vec<Row> = Vec::new();
        let mut accumulated = 0usize;
        let mut table_select_idx: Option<usize> = None;

        for view_idx in start_idx..total {
            if accumulated >= data_height {
                break;
            }

            // Insert a day-separator row before the first entry of a new date.
            if self.day_transitions.binary_search(&view_idx).is_ok() {
                let date = entry_date(arena, entries[view_idx], self.timezone_mode);
                rows.push(make_day_separator_row(date, &pretty_fixed_widths, true, area.width));
                // Clicking a separator selects the entry that follows it.
                self.visible_row_map.push(view_idx);
                accumulated += 1;
                if accumulated >= data_height {
                    break;
                }
            }

            let arena_idx = entries[view_idx];
            let resolved = arena.resolve_entry(arena_idx);
            let is_selected = Some(view_idx) == selected_view_idx;

            // Suppress raw JSON blobs: if a classifier extracted structured
            // fields but found no inner message, let the fields speak rather
            // than falling back to the raw (unreadable) JSON string.
            let display_msg = resolved.inner_message.unwrap_or_else(|| {
                if resolved.structured_fields.is_empty() {
                    resolved.message
                } else {
                    ""
                }
            });
            let msg_lines: Vec<&str> = display_msg.lines().collect();
            let msg_line_count = if display_msg.is_empty() { 0 } else { msg_lines.len().max(1) };

            // Merge logcli labels + structured fields for display, sorted by key.
            let mut all_labels: Vec<(&str, &str)> = resolved
                .labels
                .iter()
                .chain(resolved.structured_fields.iter())
                .copied()
                .collect();
            // `_`-prefixed keys are internal metadata; sort them after user fields.
            all_labels.sort_by(|(a, _), (b, _)| {
                match (a.starts_with('_'), b.starts_with('_')) {
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    _ => a.cmp(b),
                }
            });

            let layout = label_layout(&all_labels, content_width);
            let source_line = if is_selected && show_source { 1 } else { 0 };
            let label_rows = if is_selected { layout.num_rows } else { 0 };
            let full_row_height = (msg_line_count + source_line + label_rows).max(1);

            // Content column.
            let (content, rendered_height) = if is_selected {
                // Expanded: message lines + source + label rows.
                let mut lines: Vec<Line> = msg_lines
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        if i == 0 {
                            let chars: String = expand_tabs(l).chars().skip(self.h_scroll).collect();
                            Line::from(chars)
                        } else {
                            Line::from(expand_tabs(l))
                        }
                    })
                    .collect();
                if show_source {
                    let entry = &arena.entries[arena_idx];
                    let src_name = resolve_source_name(arena, entry.source_id);
                    lines.push(Line::from(Span::styled(
                        format!("  source: {}", src_name),
                        source_style(entry.source_id),
                    )));
                }

                // Render labels with alignment and optional two-column layout.
                let key_style = Style::default().fg(Color::DarkGray);
                let sep_style = Style::default().fg(Color::DarkGray);
                let val_style = Style::default().fg(Color::Gray);
                let key_col = 2 + layout.max_key_len + 3; // "  key_padded : "

                if layout.two_columns {
                    let half = (all_labels.len() + 1) / 2;
                    let cw = content_width as usize;
                    let gap = 3usize;

                    // Size left value column to fit actual content, but cap so
                    // the right column has at least key_col + 8 chars.
                    let max_left_val = all_labels[..half].iter()
                        .map(|(_, v)| v.len()).max().unwrap_or(0);
                    let right_min = key_col + 8;
                    let left_val_cap = max_left_val
                        .min(cw.saturating_sub(key_col + gap + right_min));
                    let right_col_offset = key_col + left_val_cap + gap;

                    for row_idx in 0..layout.num_rows {
                        let mut spans: Vec<Span> = Vec::new();

                        let (k, v) = all_labels[row_idx];
                        spans.push(Span::styled(
                            format!("  {:<width$}", k, width = layout.max_key_len),
                            key_style,
                        ));
                        spans.push(Span::styled(" : ", sep_style));
                        let truncated: String = v.chars().take(left_val_cap).collect();
                        let display_len = truncated.len();
                        spans.push(Span::styled(truncated, val_style));

                        let right_idx = half + row_idx;
                        if right_idx < all_labels.len() {
                            let left_used = key_col + display_len;
                            let pad = right_col_offset.saturating_sub(left_used);
                            spans.push(Span::raw(" ".repeat(pad)));

                            let (rk, rv) = all_labels[right_idx];
                            spans.push(Span::styled(
                                format!("{:<width$}", rk, width = layout.max_key_len),
                                key_style,
                            ));
                            spans.push(Span::styled(" : ", sep_style));
                            spans.push(Span::styled(rv.to_string(), val_style));
                        }

                        lines.push(Line::from(spans));
                    }
                } else {
                    for (k, v) in &all_labels {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {:<width$}", k, width = layout.max_key_len),
                                key_style,
                            ),
                            Span::styled(" : ", sep_style),
                            Span::styled(v.to_string(), val_style),
                        ]));
                    }
                }

                // Apply intra-entry scrolling for tall entries.
                // Window tall entries to data_height (visible_height minus
                // header row) so the row fits in the table's data area.
                if lines.len() > data_height {
                    let v = self.v_scroll.min(lines.len().saturating_sub(data_height));
                    let window_end = (v + data_height).min(lines.len());
                    let windowed: Vec<Line> = lines[v..window_end].to_vec();
                    let h = windowed.len();
                    (Cell::from(Text::from(windowed)), h)
                } else {
                    (Cell::from(Text::from(lines)), full_row_height)
                }
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
                            Span::raw(expand_tabs(l)),
                            Span::styled(label_suffix, Style::default().fg(Color::DarkGray)),
                        ]));
                    } else {
                        lines.push(Line::from(expand_tabs(l)));
                    }
                }
                (Cell::from(Text::from(lines)), msg_line_count)
            } else {
                // Single-line: message + inline abridged labels.
                let cell = if all_labels.is_empty() {
                    Cell::from(expand_tabs(display_msg))
                } else {
                    let mut label_suffix = String::new();
                    for (k, v) in &all_labels {
                        label_suffix.push_str(&format!("  {}={}", k, v));
                    }
                    Cell::from(Line::from(vec![
                        Span::raw(expand_tabs(display_msg)),
                        Span::styled(label_suffix, Style::default().fg(Color::DarkGray)),
                    ]))
                };
                (cell, 1)
            };

            // Indicator column: mark multi-line entries with scroll hints.
            let is_windowed = is_selected && rendered_height < full_row_height;
            let indicator = if is_windowed {
                // Tall entry with intra-scroll: show ▲/▼ hints.
                let arrow_style = Style::default().fg(Color::Yellow);
                let mut ind_lines: Vec<Line> = Vec::with_capacity(rendered_height);
                if self.v_scroll > 0 {
                    ind_lines.push(Line::from(Span::styled("▲", arrow_style)));
                } else {
                    ind_lines.push(Line::from("│"));
                }
                for _ in 1..rendered_height.saturating_sub(1) {
                    ind_lines.push(Line::from("│"));
                }
                if rendered_height > 1 {
                    if self.v_scroll + data_height < full_row_height {
                        ind_lines.push(Line::from(Span::styled("▼", arrow_style)));
                    } else {
                        ind_lines.push(Line::from("│"));
                    }
                }
                Cell::from(Text::from(ind_lines))
            } else if rendered_height > 1 {
                let lines: Vec<Line> = (0..rendered_height).map(|_| Line::from("│")).collect();
                Cell::from(Text::from(lines))
            } else {
                Cell::from(" ")
            };

            // Timestamp.
            let timestamp_str = format_timestamp(resolved.timestamp, self.timezone_mode);

            // Level with semantic color.
            let level_cell = if let Some(lvl) = resolved.level {
                Cell::from(Span::styled(level_display(lvl), level_style(lvl)))
            } else {
                Cell::from("")
            };

            if is_selected {
                table_select_idx = Some(rows.len());
            }

            // Extend row map: each screen row within this entry maps to view_idx.
            for _ in 0..rendered_height {
                self.visible_row_map.push(view_idx);
            }

            let mut cells = vec![indicator, Cell::from(timestamp_str)];
            if show_source {
                let entry = &arena.entries[arena_idx];
                let name = resolve_source_name(arena, entry.source_id);
                cells.push(
                    Cell::from(Span::styled(name.to_string(), source_style(entry.source_id))),
                );
            }
            cells.push(level_cell);
            cells.push(content);

            let mut row = Row::new(cells).height(rendered_height as u16);

            let mut style = Style::default();
            if let Some(ref filter) = self.search {
                if entry_matches_filter(arena, arena_idx, filter) {
                    style = style.fg(Color::Yellow).bold();
                }
            }
            if let Some(ref pf) = preview {
                if entry_matches_filter(arena, arena_idx, pf) {
                    style = style.bold();
                }
            }
            row = row.style(style);

            rows.push(row);
            accumulated += rendered_height;
        }

        let widths: Vec<Constraint> = if show_source {
            vec![
                Constraint::Length(1),     // indicator
                Constraint::Length(15),    // time
                Constraint::Length(src_w), // source
                Constraint::Length(5),     // level
                Constraint::Min(1),       // content
            ]
        } else {
            vec![
                Constraint::Length(1),  // indicator
                Constraint::Length(15), // time
                Constraint::Length(5),  // level
                Constraint::Min(1),    // content
            ]
        };

        let header_cells: Vec<&str> = if show_source {
            vec![" ", "Time", "Source", "Level", "Message"]
        } else {
            vec![" ", "Time", "Level", "Message"]
        };

        let table = Table::new(rows, widths)
            .header(
                Row::new(header_cells)
                    .style(Style::default().bold().fg(Color::Cyan)),
            )
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White));

        let mut table_state = TableState::default();
        table_state.select(table_select_idx);

        frame.render_stateful_widget(table, area, &mut table_state);

        // Scrollbar overlay.
        let mut scrollbar_state = ScrollbarState::new(total).position(start_idx);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(Style::default().fg(Color::Gray).bg(Color::Reset))
            .thumb_style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);

        // Paint day-transition marks on the scrollbar track.
        if total > 0 {
            let buf = frame.buffer_mut();
            let track_x = area.x + area.width - 1;
            for &pos in &self.day_transitions {
                // Match ratatui's scrollbar thumb formula exactly.
                // When viewport_content_length is unset, ratatui uses:
                //   viewport = area.height
                //   max_vp   = (total - 1) + viewport
                //   thumb_y  = round(position * track_len / max_vp)
                let track_len = area.height as f64;
                let max_vp = (total - 1 + area.height as usize) as f64;
                let y = area.y + (pos as f64 * track_len / max_vp).round() as u16;
                if y < area.y + area.height {
                    if let Some(cell) = buf.cell_mut((track_x, y)) {
                        // When the mark overlaps the scrollbar thumb, preserve
                        // the filled background so both remain visible.
                        let on_thumb = cell.symbol() == "█";
                        cell.set_symbol("◆");
                        if on_thumb {
                            cell.set_fg(Color::Black);
                            cell.set_bg(Color::White);
                        } else {
                            cell.set_fg(Color::Yellow);
                        }
                    }
                }
            }
        }
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
                    ScrollState::Tail => "TAIL".to_string(),
                    ScrollState::Selected(_) => {
                        if let Some(h) = self.selected_entry_height {
                            if h > self.viewport_height {
                                format!("SCROLL [{}/{}]", self.v_scroll + 1, h)
                            } else {
                                "SCROLL".to_string()
                            }
                        } else {
                            "SCROLL".to_string()
                        }
                    }
                };

                let display_label = match self.display_mode {
                    DisplayMode::Raw => "RAW",
                    DisplayMode::Pretty => "PRETTY",
                };

                let tz_label = match self.timezone_mode {
                    TimezoneMode::Local => "LOCAL",
                    TimezoneMode::Utc => "UTC",
                };

                let search_indicator = match &self.search {
                    Some(filter) => {
                        let prefix = if filter.inverted { "!" } else { "" };
                        let pattern = match &filter.mode {
                            FilterMode::Substring(s, _) => format!("\"{}\"", s),
                            FilterMode::Regex(r) => format!("/{}/", r.as_str()),
                        };
                        format!(" | ?:{}{}", prefix, pattern)
                    }
                    None => String::new(),
                };

                let mut hints = String::from("q:quit /:filter ?:search");
                if self.search.is_some() {
                    hints.push_str(" n/N:match Esc:clear");
                }
                hints.push_str(" s:sources >:after <:before v:view t:tz");

                let status = format!(
                    " {} | {} | {} | Filters: {} | View: {}/{} entries{} | {}",
                    mode_indicator, display_label, tz_label, filter_depth, entry_count, total_count,
                    search_indicator, hints,
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
            ToolbarMode::SearchEntry => {
                let mode_label = match self.filter_entry_mode {
                    FilterEntryMode::Substring => "SUB",
                    FilterEntryMode::Regex => "RGX",
                };

                let base_style = Style::default().bg(Color::Magenta).fg(Color::White);

                let mut spans = vec![
                    Span::styled(format!(" [{}]", mode_label), base_style),
                ];

                if self.filter_inverted {
                    spans.push(Span::styled(
                        " NOT",
                        Style::default().bg(Color::Red).fg(Color::White).bold(),
                    ));
                }

                let search_text = format!(" Search: {}", self.filter_input);
                spans.push(Span::styled(search_text, base_style));

                let used: usize = spans.iter().map(|s| s.content.len()).sum();
                let remaining = (area.width as usize).saturating_sub(used);
                if remaining > 0 {
                    spans.push(Span::styled(" ".repeat(remaining), base_style));
                }

                let paragraph = Paragraph::new(Line::from(spans));
                frame.render_widget(paragraph, area);

                // prefix spans: " [MODE]" + optional " NOT" + " Search: "
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
        let cursor = match &self.overlay {
            OverlayMode::TreeSelect { cursor } => *cursor,
            _ => return,
        };

        let mut flat: Vec<(super::ViewPath, String)> = Vec::new();
        let mut path: super::ViewPath = Vec::new();
        Self::flatten_view_tree(&arena.root_view, &mut path, 0, &[], &mut flat, &arena.source_names, self.timezone_mode);

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

    fn render_source_select_overlay(
        &self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        arena: &Arena,
    ) {
        let cursor = match &self.overlay {
            OverlayMode::SourceSelect { cursor } => *cursor,
            _ => return,
        };

        let view = arena.view_at(&self.view_path);

        let popup_area = centered_rect(50, 50, area);
        frame.render_widget(Clear, popup_area);

        let items: Vec<ListItem> = self
            .sources
            .iter()
            .map(|source| {
                let count = view.entries.iter().filter(|&&idx| arena.entries[idx].source_id == source.source_id).count();
                let badge = match &source.kind {
                    ManagedSourceKind::Loki { tls: Some(t), .. } => {
                        format!("  \u{2022}teleport:{}", t.app_name)
                    }
                    ManagedSourceKind::Subcommand { command, .. } => {
                        format!("  \u{2022}cmd:{}", command)
                    }
                    _ => String::new(),
                };
                let label = format!("{}{}  ({} entries)", source.name, badge, count);
                ListItem::new(label).style(source_style(source.source_id))
            })
            .collect();

        let cursor = cursor.min(items.len().saturating_sub(1));
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Sources ")
                    .title_bottom(" Enter : filter   e : edit   a : add   c : clone   d : delete   Esc : cancel "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
            .highlight_symbol("> ");

        let mut list_state = ListState::default();
        list_state.select(Some(cursor));
        frame.render_stateful_widget(list, popup_area, &mut list_state);
    }

    fn render_source_dialog(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let state = match &self.overlay {
            OverlayMode::SourceDialog(s) => s,
            _ => return,
        };

        let is_add = matches!(state.mode, super::SourceDialogMode::Add);
        let is_subcommand = matches!(state.source_type, SourceDialogSourceType::Subcommand);

        let title = match (&state.mode, state.source_type) {
            (super::SourceDialogMode::Add, SourceDialogSourceType::Loki) => " Add Source: Loki ".to_string(),
            (super::SourceDialogMode::Add, SourceDialogSourceType::Subcommand) => " Add Source: Subcommand ".to_string(),
            (super::SourceDialogMode::Edit { .. }, _) => format!(" Edit: {} ", state.fields[0]),
        };

        // Height: add mode gets name + type-toggle hint; subcommand has one fewer field.
        let dialog_height = match (&state.mode, state.source_type) {
            (super::SourceDialogMode::Add, SourceDialogSourceType::Loki) => 10,
            (super::SourceDialogMode::Add, SourceDialogSourceType::Subcommand) => 9,
            (super::SourceDialogMode::Edit { .. }, _) => 8,
        };
        let popup_area = centered_rect_fixed(60, dialog_height, area);
        frame.render_widget(Clear, popup_area);

        let hint = if is_add {
            " Ctrl+T: toggle type   Tab: next field   Enter: connect   Esc: cancel "
        } else {
            " Enter: apply   Esc: cancel "
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title.as_str())
            .title_bottom(hint);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let field_width = inner.width.saturating_sub(10) as usize;

        let mut y = inner.y;
        let label_x = inner.x + 2;
        let field_x = inner.x + 10;
        let dim_style = Style::default().fg(Color::DarkGray);
        let active_style = Style::default().fg(Color::White);
        let inactive_style = Style::default().fg(Color::Gray);
        let error_style = Style::default().fg(Color::Red);

        let mut cursor_pos: Option<(u16, u16)> = None;

        let render_field = |frame: &mut Frame,
                            label: &str,
                            value: &str,
                            cursor: usize,
                            is_active: bool,
                            is_editable: bool,
                            row_y: u16,
                            field_width: usize,
                            cursor_pos: &mut Option<(u16, u16)>| {
            let label_span = Span::styled(
                format!("{:>7}: ", label),
                if is_editable { inactive_style } else { dim_style },
            );
            frame.render_widget(
                Paragraph::new(Line::from(label_span)),
                ratatui::layout::Rect::new(label_x, row_y, 10, 1),
            );

            let scroll_offset = if is_active && cursor > field_width.saturating_sub(2) {
                cursor - field_width.saturating_sub(2)
            } else {
                0
            };
            let visible: String = value.chars().skip(scroll_offset).take(field_width).collect();

            let style = if is_active {
                active_style
            } else if is_editable {
                inactive_style
            } else {
                dim_style
            };

            if is_editable {
                let field_text = format!("{:<width$}", visible, width = field_width);
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(field_text, style))),
                    ratatui::layout::Rect::new(field_x, row_y, field_width as u16, 1),
                );
            } else {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(&visible, style))),
                    ratatui::layout::Rect::new(field_x, row_y, field_width as u16, 1),
                );
            }

            if is_active {
                let cursor_x = field_x + (cursor - scroll_offset) as u16;
                *cursor_pos = Some((cursor_x, row_y));
            }
        };

        if is_add {
            y += 1;
            render_field(frame, "Name", &state.fields[0], state.cursors[0],
                state.active_field == 0, true, y, field_width, &mut cursor_pos);
        }

        // Second field: URL for Loki, Command for Subcommand.
        y += 1;
        let second_label = if is_subcommand { "Command" } else { "URL" };
        render_field(frame, second_label, &state.fields[1], state.cursors[1],
            state.active_field == 1, is_add, y, field_width, &mut cursor_pos);

        // Query field: Loki only.
        if !is_subcommand {
            y += 1;
            render_field(frame, "Query", &state.fields[2], state.cursors[2],
                state.active_field == 2, true, y, field_width, &mut cursor_pos);
        }

        if let Some(err) = &state.error {
            y += 2;
            let err_area = ratatui::layout::Rect::new(label_x, y, inner.width.saturating_sub(4), 1);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(err.as_str(), error_style))),
                err_area,
            );
        }

        if let Some((cx, cy)) = cursor_pos {
            frame.set_cursor_position((cx, cy));
        }
    }

    fn render_command_palette(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let state = match &mut self.overlay {
            OverlayMode::CommandPalette(s) => s,
            _ => return,
        };

        let filtered = state.filtered_indices();

        // Height: 3 for input box (border + input + border) + items + 2 for list borders.
        let list_height = (filtered.len() as u16).min(area.height.saturating_sub(7));
        let dialog_height = 3 + list_height + 2;
        // Top-anchored with a small offset so filtering doesn't shift the dialog.
        let w = (area.width as u32 * 60 / 100) as u16;
        let left = area.x + (area.width.saturating_sub(w)) / 2;
        let top = area.y + 2;
        let h = dialog_height.min(area.height.saturating_sub(2));
        let popup_area = ratatui::layout::Rect::new(left, top, w, h);
        frame.render_widget(Clear, popup_area);

        let chunks = Layout::vertical([
            Constraint::Length(3), // input
            Constraint::Min(1),   // command list
        ])
        .split(popup_area);

        // Store list area for mouse hit-testing.
        state.list_area = chunks[1];

        // Input field.
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(" Command Palette ");
        let input_paragraph = Paragraph::new(state.input.as_str()).block(input_block);
        frame.render_widget(input_paragraph, chunks[0]);

        // Cursor in input field.
        let cursor_x = chunks[0].x + 1 + state.cursor as u16;
        let cursor_y = chunks[0].y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));

        // Command list.
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&idx| {
                let entry = &COMMAND_REGISTRY[idx];
                let line = Line::from(vec![
                    Span::raw(entry.name),
                    Span::styled(
                        format!("  {}", entry.hint),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();

        let selected = state.selected.min(filtered.len().saturating_sub(1));
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title_bottom(" ↑↓ : navigate   Enter : run   Esc : cancel "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
            .highlight_symbol("> ");

        let mut list_state = ListState::default();
        list_state.select(Some(selected));
        frame.render_stateful_widget(list, chunks[1], &mut list_state);
    }

    fn render_profile_save_dialog(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let state = match &self.overlay {
            OverlayMode::ProfileSaveDialog(s) => s,
            _ => return,
        };

        let height = if state.error.is_some() { 7 } else { 5 };
        let popup_area = centered_rect_fixed(60, height, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Save Profile ")
            .title_bottom(" Enter : save   Esc : cancel ");
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let label_style = Style::default().fg(Color::Gray);
        let field_style = Style::default().fg(Color::White);
        let error_style = Style::default().fg(Color::Red);

        // Name label.
        let label_y = inner.y + 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("    Name: ", label_style))),
            ratatui::layout::Rect::new(inner.x, label_y, 10, 1),
        );

        // Name input field.
        let field_x = inner.x + 10;
        let field_width = inner.width.saturating_sub(12) as usize;
        let scroll_offset = if state.cursor > field_width.saturating_sub(2) {
            state.cursor - field_width.saturating_sub(2)
        } else {
            0
        };
        let visible: String = state.input.chars().skip(scroll_offset).take(field_width).collect();
        let field_text = format!("{:<width$}", visible, width = field_width);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(field_text, field_style))),
            ratatui::layout::Rect::new(field_x, label_y, field_width as u16, 1),
        );

        let cursor_x = field_x + (state.cursor - scroll_offset) as u16;
        frame.set_cursor_position((cursor_x, label_y));

        if let Some(err) = &state.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(err.as_str(), error_style))),
                ratatui::layout::Rect::new(inner.x + 2, label_y + 2, inner.width.saturating_sub(4), 1),
            );
        }
    }

    fn render_profile_load_dialog(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let state = match &self.overlay {
            OverlayMode::ProfileLoadDialog(s) => s,
            _ => return,
        };

        let filtered = state.filtered_indices();

        let mode_label = match &state.load_mode {
            crate::profile::ProfileLoadMode::All => "Load Profile",
            crate::profile::ProfileLoadMode::Sources => "Load Profile (sources only)",
            crate::profile::ProfileLoadMode::Filters => "Load Profile (filters only)",
        };

        // Height: 3 for input box + items + 2 for list borders.
        let list_height = (filtered.len() as u16).min(area.height.saturating_sub(7));
        let dialog_height = 3 + list_height + 2;
        let w = (area.width as u32 * 60 / 100) as u16;
        let left = area.x + (area.width.saturating_sub(w)) / 2;
        let top = area.y + 2;
        let h = dialog_height.min(area.height.saturating_sub(2)).max(7);
        let popup_area = ratatui::layout::Rect::new(left, top, w, h);
        frame.render_widget(Clear, popup_area);

        let chunks = Layout::vertical([
            Constraint::Length(3), // input
            Constraint::Min(1),   // profile list
        ])
        .split(popup_area);

        // Input field.
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", mode_label));
        let input_paragraph = Paragraph::new(state.input.as_str()).block(input_block);
        frame.render_widget(input_paragraph, chunks[0]);

        let cursor_x = chunks[0].x + 1 + state.input_cursor as u16;
        let cursor_y = chunks[0].y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));

        // Profile list.
        let items: Vec<ListItem> = if filtered.is_empty() && !state.input.is_empty() {
            // When no matches and user has typed something, show the input as a
            // custom path option.
            vec![ListItem::new(Line::from(vec![
                Span::styled("Open: ", Style::default().fg(Color::DarkGray)),
                Span::raw(state.input.as_str()),
            ]))]
        } else {
            filtered
                .iter()
                .map(|&idx| {
                    let (name, path) = &state.profiles[idx];
                    let line = Line::from(vec![
                        Span::raw(name.as_str()),
                        Span::styled(
                            format!("  {}", path.display()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    ListItem::new(line)
                })
                .collect()
        };

        let selected = state.cursor.min(items.len().saturating_sub(1));
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title_bottom(" ↑↓ : navigate   Enter : load   Esc : cancel "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
            .highlight_symbol("> ");

        let mut list_state = ListState::default();
        list_state.select(Some(selected));
        frame.render_stateful_widget(list, chunks[1], &mut list_state);
    }
}

fn resolve_source_name(arena: &Arena, source_id: u16) -> &str {
    if source_id == INTERNAL_SOURCE_ID {
        return "internal";
    }
    arena
        .source_names
        .get(source_id as usize)
        .map(|s| s.as_str())
        .unwrap_or("?")
}

const SOURCE_COLORS: &[Color] = &[
    Color::Blue,
    Color::Green,
    Color::Magenta,
    Color::Cyan,
    Color::Yellow,
    Color::Red,
];

fn source_style(source_id: u16) -> Style {
    Style::default().fg(SOURCE_COLORS[source_id as usize % SOURCE_COLORS.len()])
}

/// Max display width for the source column (capped at 12).
fn source_column_width(arena: &Arena) -> u16 {
    arena
        .source_names
        .iter()
        .map(|n| n.len())
        .max()
        .unwrap_or(0)
        .min(12) as u16
}

struct LabelLayout {
    num_rows: usize,
    max_key_len: usize,
    two_columns: bool,
}

/// Determine the number of display rows and column layout for a set of labels.
fn label_layout(labels: &[(&str, &str)], content_width: u16) -> LabelLayout {
    if labels.is_empty() {
        return LabelLayout { num_rows: 0, max_key_len: 0, two_columns: false };
    }

    let max_key_len = labels.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let num_labels = labels.len();

    // Each column needs: indent(2) + key + separator(" : ", 3) + min_value(8).
    let min_col_width = 2 + max_key_len + 3 + 8;
    let two_columns = num_labels >= 4
        && (content_width as usize) >= 2 * min_col_width + 2;

    let num_rows = if two_columns {
        (num_labels + 1) / 2
    } else {
        num_labels
    };

    LabelLayout { num_rows, max_key_len, two_columns }
}

/// Compute the row height for an entry in pretty mode.
pub(super) fn pretty_row_height(
    arena: &Arena,
    arena_idx: usize,
    is_selected: bool,
    content_width: u16,
) -> usize {
    let resolved = arena.resolve_entry(arena_idx);
    let msg = resolved.inner_message.unwrap_or_else(|| {
        if resolved.structured_fields.is_empty() {
            resolved.message
        } else {
            ""
        }
    });
    let msg_lines = if msg.is_empty() { 0 } else { msg.lines().count().max(1) };
    if !is_selected {
        return msg_lines.max(1);
    }
    let all_labels: Vec<(&str, &str)> = resolved
        .labels
        .iter()
        .chain(resolved.structured_fields.iter())
        .copied()
        .collect();
    let source_line = if arena.source_names.len() > 1 { 1 } else { 0 };
    let layout = label_layout(&all_labels, content_width);
    (msg_lines + source_line + layout.num_rows).max(1)
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

/// Return a centered `Rect` with a fixed height and percentage width.
fn centered_rect_fixed(percent_x: u16, height: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let h = height.min(area.height);
    let top = area.y + (area.height.saturating_sub(h)) / 2;
    let w = (area.width as u32 * percent_x as u32 / 100) as u16;
    let left = area.x + (area.width.saturating_sub(w)) / 2;
    ratatui::layout::Rect::new(left, top, w, h)
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
