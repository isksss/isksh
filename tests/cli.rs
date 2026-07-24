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
