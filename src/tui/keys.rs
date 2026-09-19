pub(crate) struct Binding {
    pub keys: &'static str,
    pub description: &'static str,
}

pub(crate) struct BindingGroup {
    pub title: &'static str,
    pub bindings: &'static [Binding],
}

const NAVIGATION: &[Binding] = &[
    Binding {
        keys: "j / k / Up / Down",
        description: "select repository",
    },
    Binding {
        keys: "q",
        description: "quit",
    },
    Binding {
        keys: "Ctrl-C",
        description: "emergency quit",
    },
];

const DETAILS: &[Binding] = &[
    Binding {
        keys: "h / l / Left / Right",
        description: "previous / next detail tab",
    },
    Binding {
        keys: "Tab / Shift-Tab",
        description: "next / previous detail tab",
    },
    Binding {
        keys: "Enter",
        description: "next detail tab",
    },
    Binding {
        keys: "1-6",
        description: "select detail tab",
    },
    Binding {
        keys: "PageUp / PageDown",
        description: "scroll details",
    },
    Binding {
        keys: "Esc",
        description: "reset details",
    },
];

const REPOSITORY_ACTIONS: &[Binding] = &[
    Binding {
        keys: "a",
        description: "add checkout",
    },
    Binding {
        keys: "d",
        description: "remove checkout",
    },
    Binding {
        keys: "c",
        description: "checkout pull request",
    },
    Binding {
        keys: "p",
        description: "pull",
    },
    Binding {
        keys: "r / R",
        description: "refresh selected / all",
    },
    Binding {
        keys: "o",
        description: "open remote page",
    },
    Binding {
        keys: "t",
        description: "open terminal",
    },
    Binding {
        keys: "/",
        description: "filter repositories",
    },
];

const WORKSPACES: &[Binding] = &[
    Binding {
        keys: "w",
        description: "select workspace",
    },
    Binding {
        keys: "n",
        description: "create workspace",
    },
    Binding {
        keys: "m / u",
        description: "add / remove membership",
    },
];

const INPUT_FIELDS: &[Binding] = &[
    Binding {
        keys: "Printable characters",
        description: "type",
    },
    Binding {
        keys: "Backspace",
        description: "edit",
    },
    Binding {
        keys: "Enter",
        description: "submit",
    },
    Binding {
        keys: "Esc",
        description: "cancel",
    },
    Binding {
        keys: "F1",
        description: "open help",
    },
];

const CONFIRMATIONS: &[Binding] = &[
    Binding {
        keys: "y / Enter",
        description: "confirm",
    },
    Binding {
        keys: "n / Esc",
        description: "cancel",
    },
    Binding {
        keys: "F1",
        description: "open help",
    },
];

const HELP: &[Binding] = &[
    Binding {
        keys: "? / F1",
        description: "open help",
    },
    Binding {
        keys: "Esc / ? / F1 / q",
        description: "close help",
    },
    Binding {
        keys: "j / k / Up / Down",
        description: "scroll help",
    },
    Binding {
        keys: "PageUp / PageDown",
        description: "scroll help by page",
    },
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
