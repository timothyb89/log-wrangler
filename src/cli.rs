use clap::Parser;
use color_eyre::eyre::eyre;
use color_eyre::Result;

#[derive(Parser)]
#[command(version, about)]
pub struct Args {
    /// Log source URI (repeatable). Format: [name=]uri
    ///
    /// stdin:// (default) reads JSONL from stdin.
    /// grafana+loki+http://host:port/api/datasources/proxy/uid/UID queries Loki via Grafana.
    ///
    /// Optional name prefix: --source "prod=grafana+loki+http://..."
    #[arg(long, default_value = "stdin://")]
    pub source: Vec<String>,

    /// LogQL query (repeatable). Format: [name=]<logql>
    ///
    /// A bare query (no name prefix) applies to all Loki sources that don't
    /// have a named query. Named queries match the source name from --source.
    ///
    /// Examples:
    ///   --query '{app="myapp"}'
    ///   --query 'prod={app="myapp"}' --query 'staging={app="other"}'
    #[arg(long)]
    pub query: Vec<String>,

    /// Absolute start time (RFC 3339). Mutually exclusive with --since.
    #[arg(long, conflicts_with = "since")]
    pub start: Option<String>,

    /// Absolute end time (RFC 3339). Defaults to now.
    #[arg(long)]
    pub end: Option<String>,

    /// Relative lookback duration, e.g. "1h", "30m", "2d".
    /// Defaults to 1h when neither --start nor --since is provided.
    #[arg(long, conflicts_with = "start", value_parser = parse_std_duration)]
    pub since: Option<std::time::Duration>,

    /// Reorder buffer duration. When set, incoming messages are held for this
    /// duration to allow out-of-order messages to be sorted by timestamp.
    /// Example: "5s", "30s".
    #[arg(long, value_parser = parse_std_duration)]
    pub reorder_buffer: Option<std::time::Duration>,

    /// Follow mode: stream new logs via WebSocket after initial fetch.
    #[arg(long)]
    pub follow: bool,

    /// Regex pattern for the `regex` format. Used when any source specifies
    /// `?format=regex`. Must contain named capture groups `timestamp` and
    /// `level`; an optional `message` group overrides the full line as the
    /// displayed message. Any additional named groups become structured fields.
    ///
    /// Example: `(?P<timestamp>\S+) (?P<level>\w+) (?P<message>.*)`
    #[arg(long)]
    pub format_regex: Option<String>,

    /// Shell command for a subcommand source (repeatable). Format: [name=]<command>
    ///
    /// Fills in the command for `subcommand://` sources. A named entry
    /// (e.g., `--subcommand 'myapp=./server --port 8080'`) matches the source
    /// with that name. An unnamed entry fills in all subcommand sources that
    /// have no explicit command.
    ///
    /// Examples:
    ///   --source subcommand://app1 --subcommand 'app1=./server --port 8080'
    ///   --source subcommand:// --subcommand 'tail -f /var/log/syslog'
    #[arg(long)]
    pub subcommand: Vec<String>,

    /// Load a saved profile on startup. Can be a profile name (looked up in the
    /// default profile directory) or a path to a JSON file.
    #[arg(long)]
    pub profile: Option<String>,

    /// Control what parts of a profile to load.
    #[arg(long, value_enum, default_value = "all")]
    pub profile_mode: crate::profile::ProfileLoadMode,
}

/// Resolve the start time from CLI args.
pub fn resolve_start_time(
    start: &Option<String>,
    since: &Option<std::time::Duration>,
    now: &jiff::Zoned,
) -> Result<jiff::Zoned> {
    if let Some(s) = start {
        Ok(s.parse::<jiff::Zoned>()
            .or_else(|_| {
                s.parse::<jiff::Timestamp>()
                    .map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC))
            })
            .map_err(|e| eyre!("Invalid --start time '{}': {}", s, e))?)
    } else {
        let duration = match since {
            Some(d) => jiff::SignedDuration::from_secs(d.as_secs() as i64),
            None => parse_duration("1h")?,
        };
        Ok(now.checked_sub(duration)
            .map_err(|e| eyre!("Failed to subtract duration from now: {}", e))?)
    }
}

/// Resolve the end time from CLI args.
pub fn resolve_end_time(end: &Option<String>, now: &jiff::Zoned) -> Result<jiff::Zoned> {
    match end {
        Some(s) => Ok(s
            .parse::<jiff::Zoned>()
            .or_else(|_| {
                s.parse::<jiff::Timestamp>()
                    .map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC))
            })
            .map_err(|e| eyre!("Invalid --end time '{}': {}", s, e))?),
        None => Ok(now.clone()),
    }
}

/// Parse a human-readable duration string into `std::time::Duration` for use as
/// a clap `value_parser`.
fn parse_std_duration(s: &str) -> Result<std::time::Duration> {
    parse_duration(s).map(|d| std::time::Duration::from_secs(d.as_secs().unsigned_abs()))
}

/// Parse a human-readable duration string like "1h", "30m", "2d", "1h30m".
pub fn parse_duration(s: &str) -> Result<jiff::SignedDuration> {
    let mut total_secs: i64 = 0;
    let mut num_buf = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            num_buf.push(c);
        } else {
            if num_buf.is_empty() {
                return Err(eyre!("Invalid duration '{}': expected number before '{}'", s, c));
            }
            let n: i64 = num_buf.parse().map_err(|e| eyre!("Invalid number in duration: {}", e))?;
            num_buf.clear();
            match c {
                'd' => total_secs += n * 86400,
                'h' => total_secs += n * 3600,
                'm' => total_secs += n * 60,
                's' => total_secs += n,
                _ => return Err(eyre!("Unknown duration unit '{}' in '{}'", c, s)),
            }
        }
    }

    if !num_buf.is_empty() {
        return Err(eyre!(
            "Invalid duration '{}': trailing number without unit (use d/h/m/s)",
            s
        ));
    }

    if total_secs == 0 {
        return Err(eyre!("Invalid duration '{}': resolves to zero", s));
    }

    Ok(jiff::SignedDuration::from_secs(total_secs))
}
