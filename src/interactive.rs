use crate::{InputState, Shell};
use std::io::{self, BufRead, Write};

pub fn run_interactive(
    shell: &mut Shell,
    reader: &mut dyn BufRead,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    show_prompts: bool,
) -> io::Result<i32> {
    let mut source = String::new();
    let mut status = 0;
    loop {
        if show_prompts {
            let prompt = shell.prompt(!source.is_empty());
            stdout.write_all(prompt.as_bytes())?;
            stdout.flush()?;
        }

        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            if !source.is_empty() {
                let result = shell.run(&source, &[]);
                stdout.write_all(&result.stdout)?;
                stderr.write_all(&result.stderr)?;
                status = result.status;
            }
            return Ok(shell.take_exit_status().unwrap_or(status));
        }
        source.push_str(&line);

        match Shell::check_input(&source) {
            InputState::Incomplete => continue,
            InputState::Complete | InputState::Invalid(_) => {
                let result = shell.run(&source, &[]);
                stdout.write_all(&result.stdout)?;
                stderr.write_all(&result.stderr)?;
                status = result.status;
                source.clear();
                if let Some(exit_status) = shell.take_exit_status() {
                    return Ok(exit_status);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn preserves_state_and_collects_continuation_lines() {
        let mut shell = Shell::default();
        let mut input =
            Cursor::new(b"value=ok\nif true\nthen printf '%s' \"$value\"\nfi\nexit 7\n");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status =
            run_interactive(&mut shell, &mut input, &mut stdout, &mut stderr, false).unwrap();
        assert_eq!(status, 7);
        assert_eq!(stdout, b"ok");
        assert!(stderr.is_empty());
    }

    #[test]
    fn prints_primary_and_continuation_prompts() {
        let mut shell = Shell::default();
        let mut input = Cursor::new(b"printf '%s' 'continued\nline'\n");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status =
            run_interactive(&mut shell, &mut input, &mut stdout, &mut stderr, true).unwrap();
        assert_eq!(status, 0);
        assert!(stdout.starts_with(b"$ > "));
        assert!(stdout.ends_with(b"continued\nline$ "));
    }

    #[test]
    fn executes_pending_input_at_eof_and_reports_invalid_input() {
        let mut shell = Shell::default();
        let mut input = Cursor::new(b"printf pending");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_interactive(&mut shell, &mut input, &mut stdout, &mut stderr, false).unwrap(),
            0
        );
        assert_eq!(stdout, b"pending");

        let mut input = Cursor::new(b"if true");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_interactive(&mut shell, &mut input, &mut stdout, &mut stderr, false).unwrap(),
            2
        );
        assert!(!stderr.is_empty());

        let mut input = Cursor::new(b")\n");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_interactive(&mut shell, &mut input, &mut stdout, &mut stderr, false).unwrap(),
            2
        );
        assert!(!stderr.is_empty());
    }
}
