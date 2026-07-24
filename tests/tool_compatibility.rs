#[cfg(unix)]
#[test]
fn resolves_and_runs_every_dotfiles_cli_tool() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tools = [
        "eza",
        "atuin",
        "bat",
        "delta",
        "fzf",
        "ghq",
        "jq",
        "lazydocker",
        "lazygit",
        "nvim",
        "rg",
        "starship",
        "tree-sitter",
        "usage",
        "yazi",
        "zellij",
        "zoxide",
        "sheldon",
        "shfmt",
        "glab",
        "deno",
        "go",
        "java",
        "node",
        "python",
        "uv",
        "gwq",
        "sqio",
        "ni",
        "copilot",
        "codex",
        "playwright",
        "opencode",
        "nu",
        "rustc",
        "gh",
        "marp",
        "terraform",
        "herdr",
        "pnpm",
        "cloudflared",
        "devcontainer",
        "lazysql",
        "officecli",
        "leaf",
    ];
    let directory = tempfile::tempdir().unwrap();
    for tool in tools {
        let path = directory.path().join(tool);
        std::fs::write(&path, "#!/bin/sh\nprintf '%s\\n' \"$1\"\nexit 23\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_isksh"))
            .args(["-c", &format!("{tool} --probe")])
            .env("PATH", directory.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23), "{tool}");
        assert_eq!(output.stdout, b"--probe\n", "{tool}");
    }
}
