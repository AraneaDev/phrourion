use anyhow::{Context, Result, bail};
use std::{path::Path, process::Stdio};
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

    let directory = path.to_string_lossy().into_owned();
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

fn executable_exists(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return is_executable(path);
    }

    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|directory| is_executable(&directory.join(program)))
        })
        .unwrap_or(false)
}

fn selected_terminal() -> Result<String> {
    if let Ok(terminal) = std::env::var("TERMINAL") {
        if SUPPORTED_TERMINALS.contains(&terminal_name(&terminal)) && executable_exists(&terminal) {
            return Ok(terminal);
        }
    }

    SUPPORTED_TERMINALS
        .iter()
        .find(|terminal| executable_exists(terminal))
        .map(|terminal| (*terminal).to_string())
        .context(
            "No supported terminal found (tried foot, kitty, alacritty, wezterm, gnome-terminal)",
        )
}

pub async fn open(path: &Path) -> Result<String> {
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }

    let terminal = selected_terminal()?;
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
        command.as_std_mut().process_group(0);
    }
    command
        .spawn()
        .with_context(|| format!("Cannot start {terminal_label}"))?;

    Ok(format!("Opened {} in {terminal_label}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{command_for, open};
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn write_fake_terminal(dir: &Path, name: &str) -> PathBuf {
        let executable = dir.join(name);
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$PHROURION_TERMINAL_CAPTURE\"\n",
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

    fn restore_env(name: &str, previous: Option<OsString>) {
        unsafe {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
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
        let _guard = ENV_LOCK.lock().await;
        let bin = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let capture = bin.path().join("args");
        let kitty = write_fake_terminal(bin.path(), "kitty");
        let previous_terminal = std::env::var_os("TERMINAL");
        let previous_capture = std::env::var_os("PHROURION_TERMINAL_CAPTURE");
        unsafe {
            std::env::set_var("TERMINAL", &kitty);
            std::env::set_var("PHROURION_TERMINAL_CAPTURE", &capture);
        }

        let message = open(repo.path()).await.unwrap();
        let args = wait_for_capture(&capture).await;

        restore_env("TERMINAL", previous_terminal);
        restore_env("PHROURION_TERMINAL_CAPTURE", previous_capture);
        assert_eq!(
            message,
            format!("Opened {} in kitty", repo.path().display())
        );
        assert_eq!(args, format!("--directory\n{}\n", repo.path().display()));
    }

    #[tokio::test]
    async fn open_falls_back_to_the_first_supported_terminal_on_path() {
        let _guard = ENV_LOCK.lock().await;
        let bin = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let capture = bin.path().join("args");
        write_fake_terminal(bin.path(), "foot");
        let previous_terminal = std::env::var_os("TERMINAL");
        let previous_path = std::env::var_os("PATH");
        let previous_capture = std::env::var_os("PHROURION_TERMINAL_CAPTURE");
        unsafe {
            std::env::remove_var("TERMINAL");
            std::env::set_var("PATH", bin.path());
            std::env::set_var("PHROURION_TERMINAL_CAPTURE", &capture);
        }

        let message = open(repo.path()).await.unwrap();
        let args = wait_for_capture(&capture).await;

        restore_env("TERMINAL", previous_terminal);
        restore_env("PATH", previous_path);
        restore_env("PHROURION_TERMINAL_CAPTURE", previous_capture);
        assert_eq!(message, format!("Opened {} in foot", repo.path().display()));
        assert_eq!(
            args,
            format!("--working-directory\n{}\n", repo.path().display())
        );
    }
}
