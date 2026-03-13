use std::collections::HashMap;
use std::sync::mpsc;

use tokio::io::AsyncBufReadExt;

use crate::source::{RawLog, SourceMessage};

/// Spawn `sh -c <command>`, ingest its stdout and stderr as `RawLog` messages,
/// and return a kill sender.  When the sender is dropped (or `.send(())` is
/// called), the child process is killed.
pub fn run_subcommand_source(
    command: String,
    tx: mpsc::Sender<SourceMessage>,
    source_id: u16,
) -> tokio::sync::oneshot::Sender<()> {
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let child = tokio::process::Command::new("sh")
            .args(["-c", &command])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to spawn subcommand '{}': {}", command, e);
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let tx_out = tx.clone();
        let stdout_task = tokio::spawn(read_stream(stdout, "stdout", tx_out, source_id));
        let stderr_task = tokio::spawn(read_stream(stderr, "stderr", tx, source_id));

        tokio::select! {
            // Kill-signal received: sender was dropped or explicitly sent.
            _ = kill_rx => {
                let _ = child.kill().await;
            }
            // Process exited on its own.
            _ = child.wait() => {}
        }

        stdout_task.abort();
        stderr_task.abort();
    });

    kill_tx
}

async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    stream: &'static str,
    tx: mpsc::Sender<SourceMessage>,
    source_id: u16,
) {
    let mut lines = tokio::io::BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let mut labels = HashMap::new();
                labels.insert("_stream".to_string(), stream.to_string());
                let log = RawLog {
                    timestamp: jiff::Zoned::now(),
                    message: line,
                    labels,
                    source_id,
                };
                if tx.send(SourceMessage::Log(log)).is_err() {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
}
