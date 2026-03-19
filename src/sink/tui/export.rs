use std::fmt;
use std::io::Write;

use serde::Serialize;

use crate::log::{Arena, LogView};
use super::{App, OverlayMode, TimezoneMode};

/// Export format mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportMode {
    /// The original input message exactly as received.
    Raw,
    /// The inner message of the encapsulated format, without further extraction.
    InnerRaw,
    /// Extracted innermost message with labels and formatting.
    Pretty,
    /// Normalized JSONL with derived labels, timestamp, etc.
    Json,
}

impl ExportMode {
    pub const ALL: [ExportMode; 4] = [
        ExportMode::Raw,
        ExportMode::InnerRaw,
        ExportMode::Pretty,
        ExportMode::Json,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExportMode::Raw => "raw",
            ExportMode::InnerRaw => "inner_raw",
            ExportMode::Pretty => "pretty",
            ExportMode::Json => "json",
        }
    }

    pub fn from_label(s: &str) -> Option<ExportMode> {
        match s {
            "raw" => Some(ExportMode::Raw),
            "inner_raw" => Some(ExportMode::InnerRaw),
            "pretty" => Some(ExportMode::Pretty),
            "json" => Some(ExportMode::Json),
            _ => None,
        }
    }
}

impl fmt::Display for ExportMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportMode::Raw => write!(f, "Raw"),
            ExportMode::InnerRaw => write!(f, "Inner Raw"),
            ExportMode::Pretty => write!(f, "Pretty"),
            ExportMode::Json => write!(f, "JSON"),
        }
    }
}

/// Generate export content from the current view.
pub(super) fn generate_export(
    arena: &Arena,
    view: &LogView,
    mode: ExportMode,
    tz: TimezoneMode,
) -> String {
    let mut out = String::new();

    for &idx in &view.entries {
        let resolved = arena.resolve_entry(idx);
        let entry = &arena.entries[idx];

        match mode {
            ExportMode::Raw => {
                out.push_str(resolved.message);
                out.push('\n');
            }
            ExportMode::InnerRaw => {
                out.push_str(resolved.inner_message.unwrap_or(resolved.message));
                out.push('\n');
            }
            ExportMode::Pretty => {
                let ts = format_timestamp_export(resolved.timestamp, tz);
                let msg = resolved.inner_message.unwrap_or(resolved.message);

                out.push_str(&ts);

                if let Some(level) = resolved.level {
                    out.push_str(" [");
                    out.push_str(level);
                    out.push(']');
                }

                out.push(' ');
                out.push_str(msg);

                // Append labels and structured fields as key=value pairs.
                let has_labels = !resolved.labels.is_empty()
                    || !resolved.structured_fields.is_empty();
                if has_labels {
                    let mut first = true;
                    for (k, v) in &resolved.labels {
                        if first {
                            out.push_str("  ");
                            first = false;
                        } else {
                            out.push(' ');
                        }
                        out.push_str(k);
                        out.push('=');
                        out.push_str(v);
                    }
                    for (k, v) in &resolved.structured_fields {
                        if first {
                            out.push_str("  ");
                            first = false;
                        } else {
                            out.push(' ');
                        }
                        out.push_str(k);
                        out.push('=');
                        out.push_str(v);
                    }
                }

                out.push('\n');
            }
            ExportMode::Json => {
                let ts_utc = resolved
                    .timestamp
                    .with_time_zone(jiff::tz::TimeZone::UTC);
                let timestamp = format!("{}", ts_utc.strftime("%Y-%m-%dT%H:%M:%S%.3fZ"));

                let msg = resolved.inner_message.unwrap_or(resolved.message);

                let source_name = arena
                    .source_names
                    .get(entry.source_id as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("");

                let labels: serde_json::Map<String, serde_json::Value> = resolved
                    .labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                    .collect();

                let fields: serde_json::Map<String, serde_json::Value> = resolved
                    .structured_fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                    .collect();

                let obj = JsonExportEntry {
                    timestamp: &timestamp,
                    level: resolved.level,
                    message: msg,
                    raw_message: resolved.message,
                    source: source_name,
                    labels,
                    fields,
                };

                if let Ok(line) = serde_json::to_string(&obj) {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
    }

    out
}

#[derive(Serialize)]
struct JsonExportEntry<'a> {
    timestamp: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<&'a str>,
    message: &'a str,
    raw_message: &'a str,
    source: &'a str,
    labels: serde_json::Map<String, serde_json::Value>,
    fields: serde_json::Map<String, serde_json::Value>,
}

fn format_timestamp_export(ts: &jiff::Zoned, tz: TimezoneMode) -> String {
    let converted = match tz {
        TimezoneMode::Local => ts.with_time_zone(jiff::tz::TimeZone::system()),
        TimezoneMode::Utc => ts.with_time_zone(jiff::tz::TimeZone::UTC),
    };
    format!("{}", converted.strftime("%Y-%m-%d %H:%M:%S%.3f"))
}

/// Detect the platform clipboard command and arguments.
fn detect_clipboard_cmd() -> Option<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "macos") {
        return Some(("pbcopy", &[]));
    }
    if cfg!(target_os = "windows") {
        return Some(("clip", &[]));
    }
    // Linux: prefer Wayland if WAYLAND_DISPLAY is set.
    if std::env::var("WAYLAND_DISPLAY").is_ok() && command_exists("wl-copy") {
        return Some(("wl-copy", &[]));
    }
    if command_exists("xclip") {
        return Some(("xclip", &["-selection", "clipboard"]));
    }
    if command_exists("xsel") {
        return Some(("xsel", &["--clipboard", "--input"]));
    }
    // Wayland fallback even without WAYLAND_DISPLAY.
    if command_exists("wl-copy") {
        return Some(("wl-copy", &[]));
    }
    None
}

fn command_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn copy_to_clipboard(content: &str) -> Result<(), String> {
    let (cmd, args) = detect_clipboard_cmd().ok_or_else(|| {
        "No clipboard utility found. Install xclip, xsel, or wl-copy.".to_string()
    })?;

    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run {}: {}", cmd, e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write to {}: {}", cmd, e))?;
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for {}: {}", cmd, e))?;

    if !status.success() {
        return Err(format!("{} exited with status {}", cmd, status));
    }

    Ok(())
}

fn write_to_file(content: &str, path: &str) -> Result<(), String> {
    let path = if path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            home.join(&path[1..].trim_start_matches('/'))
        } else {
            std::path::PathBuf::from(path)
        }
    } else {
        std::path::PathBuf::from(path)
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(&path, content).map_err(|e| format!("Failed to write file: {}", e))
}

impl App {
    pub(super) fn export_to_clipboard(&mut self) {
        let content = {
            let arena = match self.arena.lock() {
                Ok(a) => a,
                Err(_) => return,
            };
            let view = arena.view_at(&self.view_path);
            generate_export(&arena, view, self.export_mode, self.timezone_mode)
        };

        if let Err(e) = copy_to_clipboard(&content) {
            self.export_error = Some(e);
        }
    }

    pub(super) fn export_to_file(&mut self, path: &str) {
        let content = {
            let arena = match self.arena.lock() {
                Ok(a) => a,
                Err(_) => return,
            };
            let view = arena.view_at(&self.view_path);
            generate_export(&arena, view, self.export_mode, self.timezone_mode)
        };

        if let Err(e) = write_to_file(&content, path) {
            if let OverlayMode::ExportFileDialog(ref mut state) = self.overlay {
                state.error = Some(e);
            }
            return;
        }

        self.overlay = OverlayMode::None;
    }
}
