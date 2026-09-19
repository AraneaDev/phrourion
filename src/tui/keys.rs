pub(crate) struct Binding {
    pub keys: &'static str,
    pub description: &'static str,
    availability: Availability,
    compact: Option<CompactHint>,
}

pub(crate) struct BindingGroup {
    pub title: &'static str,
    pub bindings: &'static [Binding],
}

#[derive(Clone, Copy)]
enum Availability {
    Always,
    Idle,
    Selection,
    IdleSelection,
}

#[derive(Clone, Copy)]
struct CompactHint {
    order: u8,
    text: &'static str,
}

const fn binding(keys: &'static str, description: &'static str) -> Binding {
    Binding {
        keys,
        description,
        availability: Availability::Always,
        compact: None,
    }
}

const fn available_when(
    keys: &'static str,
    description: &'static str,
    availability: Availability,
) -> Binding {
    Binding {
        keys,
        description,
        availability,
        compact: None,
    }
}

const fn compact(
    keys: &'static str,
    description: &'static str,
    order: u8,
    text: &'static str,
) -> Binding {
    Binding {
        keys,
        description,
        availability: Availability::Always,
        compact: Some(CompactHint { order, text }),
    }
}

const fn compact_when(
    keys: &'static str,
    description: &'static str,
    availability: Availability,
    order: u8,
    text: &'static str,
) -> Binding {
    Binding {
        keys,
        description,
        availability,
        compact: Some(CompactHint { order, text }),
    }
}

impl Binding {
    pub(crate) fn is_available(&self, has_selection: bool, action_busy: bool) -> bool {
        match self.availability {
            Availability::Always => true,
            Availability::Idle => !action_busy,
            Availability::Selection => has_selection,
            Availability::IdleSelection => has_selection && !action_busy,
        }
    }
}

const NAVIGATION: &[Binding] = &[
    binding("j / k / Up / Down", "select repository"),
    compact("q", "quit", 4, "q quit"),
    binding("Ctrl-C", "emergency quit"),
];

const DETAILS: &[Binding] = &[
    binding("h / l / Left / Right", "previous / next detail tab"),
    binding("Tab / Shift-Tab", "next / previous detail tab"),
    compact("Enter", "next detail tab", 1, "Enter details"),
    binding("1-6", "select detail tab"),
    binding("PageUp / PageDown", "scroll details"),
    binding("Esc", "reset details"),
];

const REPOSITORY_ACTIONS: &[Binding] = &[
    available_when("a", "add checkout", Availability::Idle),
    available_when("d", "remove checkout", Availability::IdleSelection),
    available_when("c", "checkout pull request", Availability::IdleSelection),
    available_when("p", "pull", Availability::IdleSelection),
    compact_when(
        "r / R",
        "refresh selected / all",
        Availability::IdleSelection,
        3,
        "r refresh",
    ),
    available_when("o", "open remote page", Availability::Selection),
    available_when("t", "open terminal", Availability::IdleSelection),
    compact("/", "filter repositories", 2, "/ filter"),
];

const WORKSPACES: &[Binding] = &[
    binding("w", "select workspace"),
    available_when("n", "create workspace", Availability::Idle),
    available_when(
        "m / u",
        "add / remove membership",
        Availability::IdleSelection,
    ),
];

const INPUT_FIELDS: &[Binding] = &[
    binding("Printable characters", "type"),
    binding("Backspace", "edit"),
    binding("Enter", "submit"),
    binding("Esc", "cancel"),
    binding("F1", "open help"),
];

const CONFIRMATIONS: &[Binding] = &[
    binding("y / Enter", "confirm"),
    binding("n / Esc", "cancel"),
    binding("F1", "open help"),
];

const HELP: &[Binding] = &[
    compact("? / F1", "open help", 0, "? help"),
    binding("Esc / ? / F1 / q", "close help"),
    binding("j / k / Up / Down", "scroll help"),
    binding("PageUp / PageDown", "scroll help by page"),
];

const CATALOG: &[BindingGroup] = &[
    BindingGroup {
        title: "Navigation",
        bindings: NAVIGATION,
    },
    BindingGroup {
        title: "Details",
        bindings: DETAILS,
    },
    BindingGroup {
        title: "Repository actions",
        bindings: REPOSITORY_ACTIONS,
    },
    BindingGroup {
        title: "Workspaces",
        bindings: WORKSPACES,
    },
    BindingGroup {
        title: "Input fields",
        bindings: INPUT_FIELDS,
    },
    BindingGroup {
        title: "Confirmations",
        bindings: CONFIRMATIONS,
    },
    BindingGroup {
        title: "Help",
        bindings: HELP,
    },
];

pub(crate) fn catalog() -> &'static [BindingGroup] {
    CATALOG
}

pub(crate) fn help_close_hint() -> String {
    let binding = catalog()
        .iter()
        .find(|group| group.title == "Help")
        .and_then(|group| {
            group
                .bindings
                .iter()
                .find(|binding| binding.description == "close help")
        })
        .expect("help catalog must contain a close binding");
    format!("{} {}", binding.keys, binding.description)
}

pub(crate) fn compact_footer_hint() -> String {
    let mut hints = catalog()
        .iter()
        .flat_map(|group| group.bindings.iter())
        .filter_map(|binding| binding.compact)
        .collect::<Vec<_>>();
    hints.sort_unstable_by_key(|hint| hint.order);
    hints
        .into_iter()
        .map(|hint| hint.text)
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_all_dashboard_actions_and_modal_controls() {
        let bindings = catalog()
            .iter()
            .flat_map(|group| group.bindings.iter())
            .collect::<Vec<_>>();
        assert!(
            bindings
                .iter()
                .any(|b| b.keys == "? / F1" && b.description == "open help")
        );
        assert!(
            bindings
                .iter()
                .any(|b| b.keys == "r / R" && b.description == "refresh selected / all")
        );
        assert!(
            bindings
                .iter()
                .any(|b| b.keys == "y / Enter" && b.description == "confirm")
        );
        assert!(catalog().iter().any(|group| group.title == "Workspaces"));
    }
}
