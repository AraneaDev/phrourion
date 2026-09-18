use anyhow::{Context, Result, bail};
use std::{ffi::OsStr, path::Path, process::Stdio};
use tokio::process::Command;

const SUPPORTED_TERMINALS: [&str; 5] = ["foot", "kitty", "alacritty", "wezterm", "gnome-terminal"];

fn terminal_name(terminal: &str) -> &str {
    Path::new(terminal)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(terminal)
}

pub fn command_for(path: &Path, terminal: &str) -> Result<(String, Vec<String>)> {
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }

    let directory = path
        .to_str()
        .context("Repository path is not valid UTF-8")?
        .to_owned();
    let args = match terminal_name(terminal) {
        "foot" | "alacritty" => vec!["--working-directory".into(), directory],
        "kitty" => vec!["--directory".into(), directory],
        "wezterm" => vec!["start".into(), "--cwd".into(), directory],
        "gnome-terminal" => vec![format!("--working-directory={directory}")],
        _ => bail!("Unsupported terminal: {terminal}"),
    };

    Ok((terminal.into(), args))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn executable_path(program: &str, path_value: Option<&OsStr>) -> Option<std::path::PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return is_executable(path)
            .then(|| std::fs::canonicalize(path).ok())
            .flatten();
    }

    path_value.and_then(|path| {
        std::env::split_paths(path)
            .map(|directory| directory.join(program))
            .find(|candidate| is_executable(candidate))
            .and_then(|candidate| std::fs::canonicalize(candidate).ok())
    })
}

fn selected_terminal(terminal_value: Option<&OsStr>, path_value: Option<&OsStr>) -> Result<String> {
    if let Some(terminal) = terminal_value.and_then(|value| value.to_str()) {
        if SUPPORTED_TERMINALS.contains(&terminal_name(terminal))
            && let Some(path) = executable_path(terminal, path_value)
        {
            return path
                .to_str()
                .context("Terminal executable path is not valid UTF-8")
                .map(str::to_owned);
        }
    }

    SUPPORTED_TERMINALS
        .iter()
        .find_map(|terminal| executable_path(terminal, path_value))
        .map(|terminal| {
            terminal
                .to_str()
                .context("Terminal executable path is not valid UTF-8")
                .map(str::to_owned)
        })
        .transpose()?
        .context(
            "No supported terminal found (tried foot, kitty, alacritty, wezterm, gnome-terminal)",
        )
}

pub async fn open(path: &Path) -> Result<String> {
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }

    open_with(
        &std::fs::canonicalize(path)
            .with_context(|| format!("Cannot resolve repository path {}", path.display()))?,
        std::env::var_os("TERMINAL").as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
    .await
}

async fn open_with(
    path: &Path,
    terminal_value: Option<&OsStr>,
    path_value: Option<&OsStr>,
) -> Result<String> {
    let terminal = selected_terminal(terminal_value, path_value)?;
    let terminal_label = terminal_name(&terminal).to_string();
    let (program, args) = command_for(path, &terminal)?;
    let mut command = Command::new(&program);
    command
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.as_std_mut().pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .spawn()
        .with_context(|| format!("Cannot start {terminal_label}"))?;

    Ok(format!("Opened {} in {terminal_label}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{command_for, open_with};
    use std::{
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    fn write_fake_terminal(dir: &Path, name: &str, capture: &Path) -> PathBuf {
        let executable = dir.join(name);
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                capture.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&executable, permissions).unwrap();
        }
        executable
    }

    async fn wait_for_capture(path: &Path) -> String {
        for _ in 0..100 {
            if let Ok(contents) = fs::read_to_string(path) {
                return contents;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("terminal did not write {}", path.display());
    }

    #[test]
    fn foot_uses_working_directory_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        assert_eq!(
            command_for(path, "foot").unwrap(),
            (
                "foot".into(),
                vec![
                    "--working-directory".into(),
                    path.to_string_lossy().into_owned(),
                ],
            )
        );
    }

    #[test]
    fn kitty_uses_directory_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        assert_eq!(
            command_for(path, "kitty").unwrap(),
            (
                "kitty".into(),
                vec!["--directory".into(), path.to_string_lossy().into_owned()],
            )
        );
    }

    #[test]
    fn alacritty_uses_working_directory_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        assert_eq!(
            command_for(path, "alacritty").unwrap(),
            (
                "alacritty".into(),
                vec![
                    "--working-directory".into(),
                    path.to_string_lossy().into_owned(),
                ],
            )
        );
    }

    #[test]
    fn wezterm_starts_in_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        assert_eq!(
            command_for(path, "wezterm").unwrap(),
            (
                "wezterm".into(),
                vec![
                    "start".into(),
                    "--cwd".into(),
                    path.to_string_lossy().into_owned(),
                ],
            )
        );
    }

    #[test]
    fn gnome_terminal_uses_joined_working_directory_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        assert_eq!(
            command_for(path, "gnome-terminal").unwrap(),
            (
                "gnome-terminal".into(),
                vec![format!("--working-directory={}", path.display())],
            )
        );
    }

    #[test]
    fn unsupported_terminal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let error = command_for(dir.path(), "xterm").unwrap_err();

        assert!(error.to_string().contains("Unsupported terminal: xterm"));
    }

    #[test]
    fn nonexistent_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");

        let error = command_for(&missing, "foot").unwrap_err();

        assert!(error.to_string().contains("not a directory"));
    }

    #[tokio::test]
    async fn open_prefers_terminal_and_spawns_it_without_a_shell() {
        let bin = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let capture = bin.path().join("args");
        write_fake_terminal(bin.path(), "kitty", &capture);

        let message = open_with(
            repo.path(),
            Some(OsStr::new("kitty")),
            Some(bin.path().as_os_str()),
        )
        .await
        .unwrap();
        let args = wait_for_capture(&capture).await;

        assert_eq!(
            message,
            format!("Opened {} in kitty", repo.path().display())
        );
        assert_eq!(args, format!("--directory\n{}\n", repo.path().display()));
    }

    #[tokio::test]
    async fn open_falls_back_to_the_first_supported_terminal_on_path() {
        let bin = tempfile::tempdir_in(".").unwrap();
        let repo = tempfile::tempdir().unwrap();
        let capture = bin.path().join("args");
        write_fake_terminal(bin.path(), "foot", &capture);
        let relative_bin = PathBuf::from(bin.path().file_name().unwrap());

        let message = open_with(repo.path(), None, Some(relative_bin.as_os_str()))
            .await
            .unwrap();
        let args = wait_for_capture(&capture).await;

        assert_eq!(message, format!("Opened {} in foot", repo.path().display()));
        assert_eq!(
            args,
            format!("--working-directory\n{}\n", repo.path().display())
        );
    }
}
