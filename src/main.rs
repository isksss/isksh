use isksh::Shell;
use std::fs;
use std::io::{self, Read, Write};

fn main() {
    let status = match run_cli() {
        Ok(status) => status,
        Err((status, message)) => {
            eprintln!("isksh: {message}");
            status
        }
    };
    std::process::exit(status);
}

fn run_cli() -> Result<i32, (i32, String)> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| (2, "arguments must be valid UTF-8".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if arguments.first().map(String::as_str) == Some("--help") {
        println!(
            "isksh 0.1.0\n\nUsage:\n  isksh SCRIPT [ARG...]\n  isksh -c COMMAND [NAME [ARG...]]\n  isksh -s [ARG...]\n"
        );
        return Ok(0);
    }
    if arguments.first().map(String::as_str) == Some("--version") {
        println!("isksh {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    let (source, name, positional) = match arguments.first().map(String::as_str) {
        Some("-c") => {
            let source = arguments
                .get(1)
                .cloned()
                .ok_or_else(|| (2, "-c requires a command string".to_string()))?;
            let name = arguments.get(2).cloned().unwrap_or_else(|| "isksh".into());
            let positional = arguments.get(3..).unwrap_or_default().to_vec();
            (source, name, positional)
        }
        Some("-s") => (
            read_stdin_utf8()?,
            "isksh".into(),
            arguments.get(1..).unwrap_or_default().to_vec(),
        ),
        Some(path) if path.starts_with('-') => {
            return Err((2, format!("unknown option: {path}")));
        }
        Some(path) => {
            let source =
                fs::read_to_string(path).map_err(|error| (126, format!("{path}: {error}")))?;
            (
                source,
                path.to_string(),
                arguments.get(1..).unwrap_or_default().to_vec(),
            )
        }
        None => (read_stdin_utf8()?, "isksh".into(), Vec::new()),
    };

    let mut shell = Shell::new(name);
    shell.set_positional(positional);
    let result = shell.run(&source, &[]);
    io::stdout()
        .write_all(&result.stdout)
        .map_err(|error| (1, error.to_string()))?;
    io::stderr()
        .write_all(&result.stderr)
        .map_err(|error| (1, error.to_string()))?;
    Ok(result.status)
}

fn read_stdin_utf8() -> Result<String, (i32, String)> {
    let mut bytes = Vec::new();
    io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| (1, error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| {
        (
            2,
            format!(
                "input must be valid UTF-8 (invalid byte at offset {})",
                error.utf8_error().valid_up_to()
            ),
        )
    })
}
