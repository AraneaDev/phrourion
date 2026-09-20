use std::process::Command;

#[test]
fn auth_help_lists_non_interactive_credential_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_phro"))
        .args(["auth", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in ["list", "set", "test", "remove"] {
        assert!(help.contains(command), "missing auth subcommand: {command}");
    }
    let set_help = String::from_utf8(
        Command::new(env!("CARGO_BIN_EXE_phro"))
            .args(["auth", "set", "--help"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(set_help.contains("token-stdin"));
}
