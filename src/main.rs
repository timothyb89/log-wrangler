use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::eyre::eyre;
use color_eyre::Result;

mod cli;
mod filter;
mod format;
mod log;
mod profile;
mod query;
mod sink;
mod source;
mod util;

use cli::Args;
use sink::tui::{ManagedSource, ManagedSourceKind};
use source::loki::LokiSourceParams;
use source::teleport::TeleportTlsConfig;
use source::{NamedQuery, NamedSource, SourceConfig, SourceMessage};

#[tokio::main]
async fn main() -> Result<()> {
    util::install_tracing();
    color_eyre::install()?;

    let args = Args::parse();

    // Load profile if requested.
    let loaded_profile = if let Some(ref profile_name) = args.profile {
        let path = profile::resolve_profile_path(profile_name)?;
        Some(profile::load_profile(&path)?)
    } else {
        None
    };

    // Determine source URIs. If a profile provides sources and the user didn't
    // explicitly specify --source (i.e., we only have the default stdin://),
    // use the profile's sources instead.
    let source_strings: Vec<String> = {
        let user_specified_sources = args.source.len() > 1
            || (args.source.len() == 1 && args.source[0] != "stdin://");

        let use_profile_sources = !user_specified_sources
            && matches!(args.profile_mode, profile::ProfileLoadMode::All | profile::ProfileLoadMode::Sources)
            && loaded_profile.as_ref().is_some_and(|p| p.sources.is_some());

        if use_profile_sources {
            let ps = loaded_profile.as_ref().unwrap().sources.as_ref().unwrap();
            ps.iter()
                .map(|s| {
                    format!("{}={}", s.name, s.uri)
                })
                .collect()
        } else {
            args.source.clone()
        }
    };

    // Parse all source URIs.
    let sources: Vec<NamedSource> = source_strings
        .iter()
        .enumerate()
        .map(|(i, raw)| source::parse_named_source(raw, i))
        .collect::<Result<_>>()?;

    // Inject queries from profile sources (if using profile sources).
    let mut profile_queries: Vec<String> = Vec::new();
    {
        let user_specified_sources = args.source.len() > 1
            || (args.source.len() == 1 && args.source[0] != "stdin://");
        let use_profile_sources = !user_specified_sources
            && matches!(args.profile_mode, profile::ProfileLoadMode::All | profile::ProfileLoadMode::Sources)
            && loaded_profile.as_ref().is_some_and(|p| p.sources.is_some());

        if use_profile_sources {
            if let Some(ps) = loaded_profile.as_ref().and_then(|p| p.sources.as_ref()) {
                for s in ps {
                    if let Some(q) = &s.query {
                        profile_queries.push(format!("{}={}", s.name, q));
                    }
                }
            }
        }
    }

    // Parse queries and build a name→query map.
    let queries: Vec<NamedQuery> = args
        .query
        .iter()
        .chain(profile_queries.iter())
        .map(|raw| source::parse_named_query(raw))
        .collect();

    // Separate into named and default (unnamed) queries.
    let mut named_queries: HashMap<String, String> = HashMap::new();
    let mut default_query: Option<String> = None;
    for q in queries {
        match q.name {
            Some(name) => {
                if named_queries.contains_key(&name) {
                    return Err(eyre!("Duplicate --query for source '{}'", name));
                }
                named_queries.insert(name, q.query);
            }
            None => {
                if default_query.is_some() {
                    return Err(eyre!(
                        "Multiple unnamed --query flags; use name= prefix to target specific sources"
                    ));
                }
                default_query = Some(q.query);
            }
        }
    }

    // Parse --subcommand flags into a name→command map, same pattern as --query.
    let mut named_subcommands: HashMap<String, String> = HashMap::new();
    let mut default_subcommand: Option<String> = None;
    for raw in &args.subcommand {
        let q = source::parse_named_query(raw);
        match q.name {
            Some(name) => {
                if named_subcommands.contains_key(&name) {
                    return Err(eyre!("Duplicate --subcommand for source '{}'", name));
                }
                named_subcommands.insert(name, q.query);
            }
            None => {
                if default_subcommand.is_some() {
                    return Err(eyre!(
                        "Multiple unnamed --subcommand flags; use name= prefix to target specific sources"
                    ));
                }
                default_subcommand = Some(q.query);
            }
        }
    }

    // Apply profile options as defaults (CLI flags take precedence).
    let mut effective_since = args.since;
    let mut effective_follow = args.follow;
    let mut effective_reorder_buffer = args.reorder_buffer;
    let effective_format_regex = args.format_regex.clone();
    if matches!(args.profile_mode, profile::ProfileLoadMode::All) {
        if let Some(opts) = loaded_profile.as_ref().and_then(|p| p.options.as_ref()) {
            if effective_since.is_none() && args.start.is_none() {
                if let Some(secs) = opts.since_secs {
                    effective_since = Some(std::time::Duration::from_secs(secs));
                }
            }
            if !effective_follow {
                effective_follow = opts.follow.unwrap_or(false);
            }
            if effective_reorder_buffer.is_none() {
                if let Some(secs) = opts.reorder_buffer_secs {
                    effective_reorder_buffer = Some(std::time::Duration::from_secs(secs));
                }
            }
        }
    }

    // Determine if we should load a filter tree from the profile.
    let profile_filter_tree: Option<profile::ProfileViewTree> = if matches!(
        args.profile_mode,
        profile::ProfileLoadMode::All | profile::ProfileLoadMode::Filters
    ) {
        loaded_profile.and_then(|p| p.filters)
    } else {
        None
    };

    let arena = log::Arena::new();

    // Register source names in the arena.
    {
        let mut a = arena.lock().unwrap();
        for src in &sources {
            // Ensure vec is large enough (source IDs may not be contiguous
            // with the internal source at u16::MAX, but user sources are 0..N).
            if src.id as usize >= a.source_names.len() {
                a.source_names.resize(src.id as usize + 1, String::new());
            }
            a.source_names[src.id as usize] = src.name.clone();
        }
    }

    // Create channel for log ingestion. Sources send SourceMessages into `tx`.
    let (tx, rx) = mpsc::channel::<SourceMessage>();

    // Route internal tracing events into the arena as native log entries.
    util::set_internal_log_sender(tx.clone());

    // Build a classifier chain for each source (indexed by source_id).
    let classifiers: Vec<format::ClassifierChain> = sources
        .iter()
        .map(|src| build_classifier_chain(&src.config, effective_format_regex.as_deref()))
        .collect();

    // Spawn the ingest thread (blocking, uses std mpsc).
    let arena_clone = arena.clone();
    let reorder_buffer = effective_reorder_buffer;
    let default_chain = default_auto_chain();
    std::thread::spawn(move || {
        log::ingest(rx, arena_clone, reorder_buffer, classifiers, default_chain);
    });

    // Resolve any Teleport sources before opening the TUI so that `tsh` can
    // freely interact with the terminal (prompt for MFA, etc.).
    let mut teleport_resolved: HashMap<u16, (url::Url, TeleportTlsConfig)> = HashMap::new();
    for src in &sources {
        if let SourceConfig::GrafanaLokiTeleport { app_name, loki_path } = &src.config {
            let tsh_cfg = source::teleport::fetch_tsh_app_config(app_name)?;
            let base_url = tsh_cfg.uri.join(loki_path)
                .map_err(|e| eyre!("Failed to construct Teleport base URL: {}", e))?;
            let tls = source::teleport::build_tls_config(&tsh_cfg)?;
            teleport_resolved.insert(src.id, (base_url, tls));
        }
    }

    // Spawn each source and collect managed-source state for stoppable sources.
    let mut managed_sources: Vec<sink::tui::ManagedSource> = Vec::new();
    let num_cli_sources = sources.len();

    for src in sources {
        match src.config {
            SourceConfig::Stdin { .. } => {
                managed_sources.push(ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: ManagedSourceKind::Stdin,
                });
                // Only spawn the stdin reader when stdin is piped. When stdin
                // is a TTY, reading from it would race with crossterm's event
                // reader on the same fd, causing dropped keypresses.
                if !std::io::stdin().is_terminal() {
                    let tx_stdin = tx.clone();
                    let sid = src.id;
                    std::thread::spawn(move || {
                        let stdin = std::io::stdin();
                        source::stdin::read_stdin(tx_stdin, sid, stdin.lock());
                    });
                }
            }
            SourceConfig::GrafanaLoki { base_url } => {
                let query = named_queries
                    .remove(&src.name)
                    .or_else(|| default_query.clone())
                    .ok_or_else(|| {
                        eyre!(
                            "--query is required for Loki source '{}'. \
                             Use --query '{}=<logql>' or a bare --query '<logql>'",
                            src.name,
                            src.name,
                        )
                    })?;

                let now = jiff::Zoned::now();
                let start = cli::resolve_start_time(&args.start, &effective_since, &now)?;
                let end = cli::resolve_end_time(&args.end, &now)?;

                let params = LokiSourceParams {
                    query: query.clone(),
                    start_ns: start.timestamp().as_nanosecond(),
                    end_ns: Some(end.timestamp().as_nanosecond()),
                    follow: effective_follow,
                };

                let (wtx, wrx) = tokio::sync::watch::channel(None);

                managed_sources.push(ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: ManagedSourceKind::Loki {
                        base_url: base_url.clone(),
                        query,
                        tx: wtx,
                        tls: None,
                    },
                });

                let tx_loki = tx.clone();
                let sid = src.id;
                tokio::spawn(source::loki::run_loki_source(
                    base_url, params, tx_loki, wrx, sid,
                    reqwest::Client::new(), None,
                ));
            }
            SourceConfig::GrafanaLokiTeleport { .. } => {
                let (base_url, tls) = teleport_resolved.remove(&src.id)
                    .expect("Teleport source was resolved above");

                let query = named_queries
                    .remove(&src.name)
                    .or_else(|| default_query.clone())
                    .ok_or_else(|| {
                        eyre!(
                            "--query is required for Teleport source '{}'. \
                             Use --query '{}=<logql>' or a bare --query '<logql>'",
                            src.name,
                            src.name,
                        )
                    })?;

                let now = jiff::Zoned::now();
                let start = cli::resolve_start_time(&args.start, &effective_since, &now)?;
                let end = cli::resolve_end_time(&args.end, &now)?;

                let params = LokiSourceParams {
                    query: query.clone(),
                    start_ns: start.timestamp().as_nanosecond(),
                    end_ns: Some(end.timestamp().as_nanosecond()),
                    follow: effective_follow,
                };

                let (wtx, wrx) = tokio::sync::watch::channel(None);

                let http_client = tls.http_client.clone();
                let ws_tls = Some(tls.rustls_config.clone());

                managed_sources.push(ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: ManagedSourceKind::Loki {
                        base_url: base_url.clone(),
                        query,
                        tx: wtx,
                        tls: Some(tls),
                    },
                });

                let tx_loki = tx.clone();
                let sid = src.id;
                tokio::spawn(source::loki::run_loki_source(
                    base_url, params, tx_loki, wrx, sid,
                    http_client, ws_tls,
                ));
            }
            SourceConfig::Subcommand { command } => {
                let cmd = command
                    .or_else(|| named_subcommands.remove(&src.name))
                    .or_else(|| default_subcommand.clone())
                    .ok_or_else(|| {
                        eyre!(
                            "--subcommand is required for subcommand source '{}'. \
                             Use --subcommand '{}=<command>' or a bare --subcommand '<command>'",
                            src.name,
                            src.name,
                        )
                    })?;

                let kill_tx = source::subcommand::run_subcommand_source(
                    cmd.clone(), tx.clone(), src.id,
                );

                managed_sources.push(ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: ManagedSourceKind::Subcommand { command: cmd, kill_tx },
                });
            }
        }
    }

    let next_source_id = num_cli_sources as u16;

    // Apply startup filter tree from profile (before TUI starts, before entries arrive).
    if let Some(ref tree) = profile_filter_tree {
        let mut a = arena.lock().unwrap();
        let new_root = profile::profile_to_view_tree(tree, &a.rodeo, &a.source_names);
        a.root_view = new_root;
    }

    // Run the TUI on the async runtime.
    sink::tui::run_tui(arena, managed_sources, tx, next_source_id).await?;

    Ok(())
}

fn build_classifier_chain(
    config: &SourceConfig,
    format_regex: Option<&str>,
) -> format::ClassifierChain {
    let hint = match config {
        SourceConfig::Stdin { format_hint } => format_hint.as_deref(),
        SourceConfig::Subcommand { .. }
        | SourceConfig::GrafanaLoki { .. }
        | SourceConfig::GrafanaLokiTeleport { .. } => None,
    };

    match hint {
        Some("slog") => {
            format::ClassifierChain::new(vec![Box::new(format::json::slog())])
        }
        Some("rust-tracing") => {
            format::ClassifierChain::new(vec![Box::new(format::json::rust_tracing())])
        }
        Some("journald-json") => {
            format::ClassifierChain::new(vec![Box::new(format::Encapsulating {
                outer: Box::new(format::json::journald_json()),
                inner: Box::new(format::json::default()),
            })])
        }
        Some("journald") => {
            format::ClassifierChain::new(vec![Box::new(format::Encapsulating {
                outer: Box::new(format::plaintext::SystemdClassifier),
                inner: Box::new(format::json::default()),
            })])
        }
        Some("generic") => {
            format::ClassifierChain::new(vec![Box::new(format::plaintext::GenericClassifier)])
        }
        Some("regex") => {
            if let Some(pattern) = format_regex {
                match regex::Regex::new(pattern) {
                    Ok(re) => format::ClassifierChain::new(vec![
                        Box::new(format::plaintext::RegexClassifier { pattern: re }),
                    ]),
                    Err(e) => {
                        eprintln!("Warning: invalid --format-regex pattern: {e}");
                        default_auto_chain()
                    }
                }
            } else {
                eprintln!("Warning: format=regex requires --format-regex");
                default_auto_chain()
            }
        }
        _ => default_auto_chain(),
    }
}

/// Default classifier chain used when no explicit format hint is given.
/// Tries journald JSON first (strict: requires the `MESSAGE` key), then
/// journald plaintext (strict regex), then falls back to generic JSON.
/// The `last_hit` cache means a stable-format stream pays at most one extra
/// classification attempt on the very first message.
fn default_auto_chain() -> format::ClassifierChain {
    format::ClassifierChain::new(vec![
        Box::new(format::Encapsulating {
            outer: Box::new(format::json::journald_json()),
            inner: Box::new(format::json::default()),
        }),
        Box::new(format::Encapsulating {
            outer: Box::new(format::plaintext::SystemdClassifier),
            inner: Box::new(format::json::default()),
        }),
        Box::new(format::json::default()),
    ])
}
