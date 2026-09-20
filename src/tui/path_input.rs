use std::path::{Path, PathBuf};

pub(super) fn resolve_path(input: &str, startup_dir: &Path) -> PathBuf {
    let input = input.trim();
    let expanded = input
        .strip_prefix("~")
        .and_then(|rest| dirs::home_dir().map(|home| home.join(rest.trim_start_matches('/'))))
        .unwrap_or_else(|| PathBuf::from(input));
    if expanded.is_absolute() {
        expanded
    } else {
        startup_dir.join(expanded)
    }
}

pub(super) fn complete_path(input: &str, startup_dir: &Path) -> Option<String> {
    let input = input.trim();
    let path = resolve_path(input, startup_dir);
    let prefix = path.file_name()?.to_string_lossy().to_lowercase();
    let parent = path.parent()?;
    let mut entries: Vec<_> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&prefix)
        })
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    let candidate = entries.first()?.file_name().to_string_lossy().to_string();

    let typed_parent = Path::new(input).parent().unwrap_or_else(|| Path::new(""));
    let mut completed = typed_parent.join(candidate);
    completed.push("");
    Some(completed.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn completion_resolves_relative_directory_names_from_startup_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("Nemesis-MCP")).unwrap();
        fs::create_dir(root.path().join("Other-MCP")).unwrap();

        assert_eq!(
            complete_path("Nem", root.path()),
            Some("Nemesis-MCP/".into())
        );
        assert_eq!(
            resolve_path("Nemesis-MCP", root.path()),
            root.path().join("Nemesis-MCP")
        );
    }
}
