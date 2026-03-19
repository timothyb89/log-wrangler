/// A discrete command that can be invoked from keybindings or the command palette.
///
/// This intentionally excludes per-frame scrolling (j/k, h/l, PageUp/Down) which
/// are rapid-repeat navigation actions that don't belong in a palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Quit,
    EnterFilterMode,
    EnterFilterSubstring,
    EnterFilterRegex,
    EnterFilterQuery,
    EnterSearchMode,
    PopFilter,
    PopAndRemoveFilter,
    NavigateSiblingPrev,
    NavigateSiblingNext,
    OpenTreeSelect,
    OpenSourceSelect,
    ToggleDisplayMode,
    ToggleTimezone,
    TimeFilterAfter,
    TimeFilterBefore,
    SearchNext,
    SearchPrev,
    ClearSearch,
    ScrollToTop,
    ScrollToBottom,
    AddSourceLoki,
    AddSourceSubcommand,
    OpenCommandPalette,
    SaveProfile,
    LoadProfile,
    LoadProfileSourcesOnly,
    LoadProfileFiltersOnly,
}

/// A command entry in the palette registry.
pub(super) struct CommandEntry {
    pub action: Action,
    pub name: &'static str,
    pub hint: &'static str,
}

/// All commands available in the command palette.
///
/// `OpenCommandPalette` is deliberately excluded (can't open palette from palette).
pub(super) const COMMAND_REGISTRY: &[CommandEntry] = &[
    CommandEntry { action: Action::EnterFilterMode,      name: "Filter logs",                hint: "/" },
    CommandEntry { action: Action::EnterFilterSubstring, name: "Filter (substring)",         hint: "" },
    CommandEntry { action: Action::EnterFilterRegex,     name: "Filter (regex)",             hint: "" },
    CommandEntry { action: Action::EnterFilterQuery,     name: "Filter (query)",             hint: "" },
    CommandEntry { action: Action::EnterSearchMode,      name: "Search logs",                hint: "?" },
    CommandEntry { action: Action::PopFilter,            name: "Pop filter (go up)",         hint: "Backspace" },
    CommandEntry { action: Action::PopAndRemoveFilter,   name: "Pop and remove filter",      hint: "p" },
    CommandEntry { action: Action::NavigateSiblingPrev,  name: "Previous sibling view",      hint: "[" },
    CommandEntry { action: Action::NavigateSiblingNext,  name: "Next sibling view",          hint: "]" },
    CommandEntry { action: Action::OpenTreeSelect,       name: "Open view tree",             hint: "Tab" },
    CommandEntry { action: Action::OpenSourceSelect,     name: "Open sources",               hint: "s" },
    CommandEntry { action: Action::ToggleDisplayMode,    name: "Toggle raw/pretty view",     hint: "v" },
    CommandEntry { action: Action::ToggleTimezone,       name: "Toggle timezone UTC/Local",  hint: "t" },
    CommandEntry { action: Action::TimeFilterAfter,      name: "Filter after selected",      hint: ">" },
    CommandEntry { action: Action::TimeFilterBefore,     name: "Filter before selected",     hint: "<" },
    CommandEntry { action: Action::SearchNext,           name: "Next search match",          hint: "n" },
    CommandEntry { action: Action::SearchPrev,           name: "Previous search match",      hint: "N" },
    CommandEntry { action: Action::ClearSearch,          name: "Clear search",               hint: "Esc" },
    CommandEntry { action: Action::ScrollToTop,          name: "Scroll to top",              hint: "g" },
    CommandEntry { action: Action::ScrollToBottom,       name: "Scroll to bottom (tail)",    hint: "G" },
    CommandEntry { action: Action::AddSourceLoki,         name: "Add source: Loki",            hint: "" },
    CommandEntry { action: Action::AddSourceSubcommand,  name: "Add source: Subcommand",      hint: "" },
    CommandEntry { action: Action::SaveProfile,          name: "Save profile",                hint: "" },
    CommandEntry { action: Action::LoadProfile,          name: "Load profile",                hint: "" },
    CommandEntry { action: Action::LoadProfileSourcesOnly, name: "Load profile (sources only)", hint: "" },
    CommandEntry { action: Action::LoadProfileFiltersOnly, name: "Load profile (filters only)", hint: "" },
    CommandEntry { action: Action::Quit,                 name: "Quit",                        hint: "q" },
];
