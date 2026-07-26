use std::process::{Command, Stdio};

/// `isksh`に対応する処理を行う。
fn isksh() -> Command {
    Command::new(env!("CARGO_BIN_EXE_isksh"))
}

#[test]
/// `runs_command_string`に対応する処理を行う。
fn runs_command_string() {
    let output = isksh()
        .args(["-c", "value=cli; printf '%s\\n' \"$value\""])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"cli\n");
}

#[test]
/// `runs_script_file_with_arguments`に対応する処理を行う。
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
/// `reads_script_from_stdin`に対応する処理を行う。
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
/// `reads_script_from_stdin_without_option`に対応する処理を行う。
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
/// `rejects_non_utf8_input`に対応する処理を行う。
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
/// `reports_help_version_and_cli_errors`に対応する処理を行う。
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

/// 英語・日本語・中国語でヘルプ、通常メッセージ、診断を出力できることを確認する。
#[test]
fn localizes_cli_and_shell_messages() {
    for (language, help_heading, usage_heading, builtin_text, missing_text) in [
        (
            "en",
            "Usage:",
            "usage:",
            "shell builtin",
            "command not found",
        ),
        (
            "ja",
            "使用方法:",
            "使用方法:",
            "シェル組み込みコマンド",
            "コマンドが見つかりません",
        ),
        ("zh-CN", "用法:", "用法:", "shell 内置命令", "找不到命令"),
    ] {
        let help = isksh()
            .arg("--help")
            .env("ISKSH_LANG", language)
            .output()
            .unwrap();
        assert!(help.status.success());
        assert!(
            String::from_utf8(help.stdout)
                .unwrap()
                .contains(help_heading)
        );

        let builtin = isksh()
            .args(["-c", "help printf; type printf; abbr --help"])
            .env("ISKSH_LANG", language)
            .output()
            .unwrap();
        assert!(builtin.status.success());
        let builtin = String::from_utf8(builtin.stdout).unwrap();
        assert!(builtin.contains(builtin_text), "{language}: {builtin}");
        assert!(builtin.contains(usage_heading), "{language}: {builtin}");

        let missing = isksh()
            .args(["-c", "__isksh_missing_localized_command__"])
            .env("ISKSH_LANG", language)
            .output()
            .unwrap();
        assert_eq!(missing.status.code(), Some(127));
        let missing = String::from_utf8(missing.stderr).unwrap();
        assert!(missing.contains(missing_text), "{language}: {missing}");
    }
}

#[test]
/// `force_interactive_mode_reads_lines_and_exit`に対応する処理を行う。
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
/// `interactive_mode_loads_iskrc`に対応する処理を行う。
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
/// `startup_files_follow_environment_login_and_interactive_rules`に対応する処理を行う。
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
/// `invalid_mode_falls_back_to_bash_and_other_shell_rc_files_are_ignored`に対応する処理を行う。
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
/// `rejects_non_utf8_argument`に対応する処理を行う。
fn rejects_non_utf8_argument() {
    use std::os::unix::ffi::OsStringExt;
    let output = isksh()
        .arg(std::ffi::OsString::from_vec(vec![0xff]))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
