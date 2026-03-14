use std::path::{Path, PathBuf};

use color_eyre::eyre::eyre;
use color_eyre::Result;
use serde::{Deserialize, Serialize};

use crate::filter::{Filter, FilterMode, FilterTarget};
use crate::log::{Arena, LogView, MetaRodeo};
use crate::sink::tui::{ManagedSource, ManagedSourceKind};

/// What parts of a profile to load.
#[derive(Clone, Debug, Default, clap::ValueEnum)]
pub enum ProfileLoadMode {
    #[default]
    All,
    Sources,
    Filters,
}

// ---------------------------------------------------------------------------
// Serializable profile types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<ProfileSource>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<ProfileViewTree>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<ProfileOptions>,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileSource {
    pub name: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reorder_buffer_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_regex: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileViewTree {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub filters: Vec<ProfileFilter>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<ProfileViewTree>,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileFilter {
    pub mode: ProfileFilterMode,
    pub target: ProfileFilterTarget,
    #[serde(default)]
    pub inverted: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "pattern")]
pub enum ProfileFilterMode {
    Substring(String),
    Regex(String),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProfileFilterTarget {
    Message,
    Label { key: String },
    Any,
    Source { name: String },
    After { timestamp: String },
    Before { timestamp: String },
}

// ---------------------------------------------------------------------------
// Conversion: runtime -> profile
// ---------------------------------------------------------------------------

impl Profile {
    /// Build a profile from the current app state.
    pub fn from_app_state(sources: &[ManagedSource], arena: &Arena) -> Self {
        let profile_sources: Vec<ProfileSource> = sources
            .iter()
            .map(|s| {
                let (uri, query) = match &s.kind {
                    ManagedSourceKind::Stdin => ("stdin://".to_string(), None),
                    ManagedSourceKind::Loki {
                        base_url, query, ..
                    } => {
                        let scheme = base_url.scheme();
                        let uri = format!(
                            "grafana+loki+{}://{}{}",
                            scheme,
                            base_url.host_str().unwrap_or(""),
                            base_url.path()
                        );
                        (uri, Some(query.clone()))
                    }
                    ManagedSourceKind::Subcommand { command, .. } => {
                        ("subcommand://".to_string(), Some(command.clone()))
                    }
                };
                ProfileSource {
                    name: s.name.clone(),
                    uri,
                    query,
                }
            })
            .collect();

        let filters = view_tree_to_profile(&arena.root_view, &arena.rodeo, &arena.source_names);

        let options = ProfileOptions {
            reorder_buffer_secs: None,
            since_secs: None,
            follow: None,
            format_regex: None,
        };

        Profile {
            version: 1,
            sources: Some(profile_sources),
            filters: Some(filters),
            options: Some(options),
        }
    }
}

fn view_tree_to_profile(
    view: &LogView,
    rodeo: &MetaRodeo,
    source_names: &[String],
) -> ProfileViewTree {
    let filters = view
        .filters
        .iter()
        .filter_map(|f| filter_to_profile(f, rodeo, source_names))
        .collect();

    let children = view
        .children
        .iter()
        .map(|child| view_tree_to_profile(child, rodeo, source_names))
        .collect();

    ProfileViewTree { filters, children }
}

fn filter_to_profile(
    filter: &Filter,
    rodeo: &MetaRodeo,
    source_names: &[String],
) -> Option<ProfileFilter> {
    let mode = match &filter.mode {
        FilterMode::Substring(s, _) => ProfileFilterMode::Substring(s.clone()),
        FilterMode::Regex(re) => ProfileFilterMode::Regex(re.as_str().to_string()),
    };

    let target = match &filter.target {
        FilterTarget::Message => ProfileFilterTarget::Message,
        FilterTarget::Label(spur) => {
            let key = rodeo.label_keys.resolve(spur).to_string();
            ProfileFilterTarget::Label { key }
        }
        FilterTarget::Any => ProfileFilterTarget::Any,
        FilterTarget::Source(sid) => {
            let name = source_names
                .get(*sid as usize)
                .cloned()
                .unwrap_or_else(|| format!("source-{}", sid));
            ProfileFilterTarget::Source { name }
        }
        FilterTarget::After(ts) => ProfileFilterTarget::After {
            timestamp: ts.to_string(),
        },
        FilterTarget::Before(ts) => ProfileFilterTarget::Before {
            timestamp: ts.to_string(),
        },
    };

    Some(ProfileFilter {
        mode,
        target,
        inverted: filter.inverted,
    })
}

// ---------------------------------------------------------------------------
// Conversion: profile -> runtime
// ---------------------------------------------------------------------------

/// Convert a profile view tree into a runtime LogView (entries will be empty).
pub fn profile_to_view_tree(
    tree: &ProfileViewTree,
    rodeo: &MetaRodeo,
    source_names: &[String],
) -> LogView {
    let filters: Vec<Filter> = tree
        .filters
        .iter()
        .filter_map(|pf| profile_filter_to_runtime(pf, rodeo, source_names))
        .collect();

    let children: Vec<LogView> = tree
        .children
        .iter()
        .map(|child| profile_to_view_tree(child, rodeo, source_names))
        .collect();

    LogView {
        filters,
        children,
        entries: Vec::new(),
    }
}

fn profile_filter_to_runtime(
    pf: &ProfileFilter,
    rodeo: &MetaRodeo,
    source_names: &[String],
) -> Option<Filter> {
    let mode = match &pf.mode {
        ProfileFilterMode::Substring(s) => FilterMode::substring(s.clone()),
        ProfileFilterMode::Regex(s) => {
            let re = regex::Regex::new(s).ok()?;
            FilterMode::Regex(re)
        }
    };

    let target = match &pf.target {
        ProfileFilterTarget::Message => FilterTarget::Message,
        ProfileFilterTarget::Label { key } => {
            let spur = rodeo.label_keys.get_or_intern(key);
            FilterTarget::Label(spur)
        }
        ProfileFilterTarget::Any => FilterTarget::Any,
        ProfileFilterTarget::Source { name } => {
            let sid = source_names
                .iter()
                .position(|n| n == name)
                .map(|i| i as u16)?;
            FilterTarget::Source(sid)
        }
        ProfileFilterTarget::After { timestamp } => {
            let ts: jiff::Timestamp = timestamp.parse().ok()?;
            FilterTarget::After(ts)
        }
        ProfileFilterTarget::Before { timestamp } => {
            let ts: jiff::Timestamp = timestamp.parse().ok()?;
            FilterTarget::Before(ts)
        }
    };

    Some(Filter {
        mode,
        target,
        inverted: pf.inverted,
    })
}

// ---------------------------------------------------------------------------
// File I/O and directory management
// ---------------------------------------------------------------------------

/// Return the default profile directory for the current platform.
pub fn default_profile_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| eyre!("Could not determine data directory"))?;
    Ok(base.join("log-wrangler").join("profiles"))
}

/// Resolve a profile name or path to an actual filesystem path.
///
/// If the input contains a path separator or ends with `.json`, it is treated as
/// a literal path. Otherwise it is looked up as `{name}.json` in the default
/// profile directory.
pub fn resolve_profile_path(name_or_path: &str) -> Result<PathBuf> {
    if name_or_path.contains(std::path::MAIN_SEPARATOR) || name_or_path.ends_with(".json") {
        Ok(PathBuf::from(name_or_path))
    } else {
        let dir = default_profile_dir()?;
        Ok(dir.join(format!("{}.json", name_or_path)))
    }
}

pub fn save_profile(profile: &Profile, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(profile)?;
    std::fs::write(path, json)?;
    Ok(())
}

pub fn load_profile(path: &Path) -> Result<Profile> {
    let contents = std::fs::read_to_string(path)?;
    let profile: Profile =
        serde_json::from_str(&contents).map_err(|e| eyre!("Invalid profile JSON: {}", e))?;
    Ok(profile)
}

/// List profiles in the default profile directory.
/// Returns `(display_name, full_path)` pairs sorted by name.
pub fn list_profiles() -> Result<Vec<(String, PathBuf)>> {
    let dir = default_profile_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            profiles.push((name, path));
        }
    }
    profiles.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(profiles)
}
