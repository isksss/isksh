use std::process::{Command, Stdio};

fn isksh() -> Command {
    Command::new(env!("CARGO_BIN_EXE_isksh"))
}

#[test]
fn runs_command_string() {
    let output = isksh()
        .args(["-c", "value=cli; printf '%s\\n' \"$value\""])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"cli\n");
}

#[test]
fn runs_script_file_with_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("script.sh");
    std::fs::write(&script, "printf '%s:%s\\n' \"$0\" \"$1\"\n").unwrap();
    let output = isksh().arg(&script).arg("argument").output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with(":argument\n")
    );
}

#[test]
fn reads_script_from_stdin() {
    let mut child = isksh()
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf stdin")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.stdout, b"stdin");
}

#[test]
fn reads_script_from_stdin_without_option() {
    let output = isksh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"printf implicit")?;
            child.wait_with_output()
        })
        .unwrap();
    assert_eq!(output.stdout, b"implicit");
}

#[test]
fn rejects_non_utf8_input() {
    let mut child = isksh()
        .arg("-s")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&[0xff]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("UTF-8"));
}

#[test]
fn reports_help_version_and_cli_errors() {
    let help = isksh().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout).unwrap().contains("Usage"));
    let version = isksh().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8(version.stdout).unwrap().contains("isksh"));
    assert_eq!(isksh().arg("-c").output().unwrap().status.code(), Some(2));
    assert_eq!(
        isksh().arg("--unknown").output().unwrap().status.code(),
        Some(2)
    );
    assert_eq!(
        isksh()
            .arg("missing-isksh-script")
            .output()
            .unwrap()
            .status
            .code(),
        Some(126)
    );
}

#[test]
fn force_interactive_mode_reads_lines_and_exit() {
    let mut child = isksh()
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf interactive\nexit 4\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("interactive")
    );
}

#[test]
fn interactive_mode_loads_iskrc() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("isksh");
    std::fs::create_dir(&config).unwrap();
    let rc = config.join(".iskrc");
    std::fs::write(
        &rc,
        "export FROM_ISKRC=loaded\nalias configured='printf alias-loaded'\nabbr -a short 'printf abbr-loaded'\nPS1='isk> '\n",
    )
    .unwrap();
    let mut child = isksh()
        .arg("-i")
        .env("XDG_CONFIG_HOME", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf '%s:' \"$FROM_ISKRC\"; configured; short\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("isk> "));
    assert!(stdout.contains("loaded:alias-loaded"));
    assert!(stdout.contains("abbr-loaded"));
}

#[test]
fn startup_files_follow_environment_login_and_interactive_rules() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config/isksh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join(".iskenv"),
        "export ISKSH_MODE=zsh\nexport FROM_ISKENV=env\n",
    )
    .unwrap();
    std::fs::write(config.join(".iskprofile"), "export FROM_PROFILE=profile\n").unwrap();
    std::fs::write(
        config.join(".iskrc"),
        "export FROM_ISKRC=rc\nPROMPT='isk%# '",
    )
    .unwrap();

    let login = isksh()
        .args([
            "-l",
            "-c",
            "print -r -- \"$ISKSH_MODE:$FROM_ISKENV:$FROM_PROFILE:${FROM_ISKRC:-none}\"",
        ])
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .output()
        .unwrap();
    assert!(login.status.success());
    assert_eq!(login.stdout, b"zsh:env:profile:none\n");

    let mut child = isksh()
        .arg("-il")
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf '%s:%s:%s' \"$FROM_ISKENV\" \"$FROM_PROFILE\" \"$FROM_ISKRC\"\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("env:profile:rc")
    );
}

#[test]
fn invalid_mode_falls_back_to_bash_and_other_shell_rc_files_are_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config/isksh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(".iskenv"), "export ISKSH_MODE=invalid\n").unwrap();
    std::fs::write(directory.path().join(".bashrc"), "export LEAKED=bashrc\n").unwrap();
    std::fs::write(directory.path().join(".zshrc"), "export LEAKED=zshrc\n").unwrap();
    let output = isksh()
        .args(["-c", "printf '%s:%s' \"$ISKSH_MODE\" \"${LEAKED:-none}\""])
        .env("HOME", directory.path())
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"bash:none");
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_argument() {
    use std::os::unix::ffi::OsStringExt;
    let output = isksh()
        .arg(std::ffi::OsString::from_vec(vec![0xff]))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
