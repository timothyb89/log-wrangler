use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::{mpsc, OnceLock};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::source::{RawLog, SourceMessage};

/// Source ID reserved for internal tracing events.
pub const INTERNAL_SOURCE_ID: u16 = u16::MAX;

static INTERNAL_TX: OnceLock<mpsc::Sender<SourceMessage>> = OnceLock::new();

/// Register the channel sender so internal tracing events appear as log entries.
pub fn set_internal_log_sender(tx: mpsc::Sender<SourceMessage>) {
    let _ = INTERNAL_TX.set(tx);
}

pub fn install_tracing() {
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    // Only write to stderr when it's redirected (not a terminal), so it
    // doesn't corrupt the TUI. Useful for `cargo run 2>/tmp/debug.log`.
    let stderr_layer = if !std::io::stderr().is_terminal() {
        Some(fmt::layer().with_target(true).with_writer(std::io::stderr))
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(stderr_layer)
        .with(InternalLogLayer)
        .with(ErrorLayer::default())
        .init();
}

/// A tracing layer that routes events into the log arena as native entries.
/// Events are formatted as JSON so the arena's nested JSON parser extracts
/// level and message, giving proper coloring in pretty mode.
struct InternalLogLayer;

impl<S: tracing::Subscriber> Layer<S> for InternalLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let Some(tx) = INTERNAL_TX.get() else { return };

        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        let level = event.metadata().level().to_string().to_lowercase();
        let target = event.metadata().target();

        let mut message = visitor.message;
        if !visitor.fields.is_empty() {
            message.push(' ');
            message.push_str(&visitor.fields.join(" "));
        }

        let json = serde_json::json!({
            "level": level,
            "message": message,
            "target": target,
        });

        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "internal".to_string());

        let raw_log = RawLog {
            timestamp: jiff::Zoned::now(),
            message: json.to_string(),
            labels,
            source_id: INTERNAL_SOURCE_ID,
        };

        let _ = tx.send(SourceMessage::Log(raw_log));
    }
}

#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
}
