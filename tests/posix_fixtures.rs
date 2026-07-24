use isksh::Shell;

fn assert_script(source: &str, expected: &str, status: i32) {
    let result = Shell::default().run(source, &[]);
    assert_eq!(
        result.status,
        status,
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8(result.stdout).unwrap(), expected);
}

#[test]
fn and_or_lists_follow_exit_status() {
    assert_script(
        "false && printf no; true || printf no; false || printf yes",
        "yes",
        0,
    );
}

#[test]
fn quotes_control_field_splitting() {
    assert_script(
        "value='a b'; for x in $value; do printf '<%s>' \"$x\"; done",
        "<a><b>",
        0,
    );
    assert_script(
        "value='a b'; for x in \"$value\"; do printf '<%s>' \"$x\"; done",
        "<a b>",
        0,
    );
}

#[test]
fn parameter_default_operators_work() {
    assert_script(
        "unset value; printf '%s' \"${value:-fallback}\"",
        "fallback",
        0,
    );
    assert_script(
        "printf '%s' \"${value:=assigned}\"; printf ':%s' \"$value\"",
        "assigned:assigned",
        0,
    );
}

#[test]
fn redirection_writes_and_reads_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("output.txt")
        .to_string_lossy()
        .replace('\\', "/");
    let source = format!("printf first > '{path}'; printf second >> '{path}'; cat < '{path}'");
    assert_script(&source, "firstsecond", 0);
}

#[test]
fn here_documents_expand_unless_delimiter_is_quoted() {
    assert_script("value=ok\ncat <<EOF\n$value:$((1 + 2))\nEOF\n", "ok:3\n", 0);
    assert_script("value=ignored\ncat <<'EOF'\n$value\nEOF\n", "$value\n", 0);
}

#[test]
fn pattern_removal_and_non_whitespace_ifs_match_posix() {
    assert_script(
        "value=abcabc; printf '%s|%s|%s|%s' \"${value%c*}\" \"${value%%c*}\" \"${value#a*}\" \"${value##a*}\"",
        "abcab|ab|bcabc|",
        0,
    );
    assert_script(
        "IFS=:; value='a::b:'; for field in $value; do printf '<%s>' \"$field\"; done",
        "<a><><b>",
        0,
    );
}

#[test]
fn external_pipeline_is_concurrent() {
    assert_script("yes | head -n 1", "y\n", 0);
}
