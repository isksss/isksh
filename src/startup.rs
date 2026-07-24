use crate::{RunResult, Shell};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn startup_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or(std::env::var_os("USERPROFILE"));
    startup_file_from(
        std::env::var_os("ISKSH_RC"),
        std::env::var_os("XDG_CONFIG_HOME"),
        home,
    )
}

pub fn bash_startup_file() -> Option<PathBuf> {
    bash_startup_file_from(
        std::env::var_os("ISKSH_RC").is_some(),
        std::env::var_os("HOME").or(std::env::var_os("USERPROFILE")),
    )
}

fn bash_startup_file_from(override_present: bool, home: Option<OsString>) -> Option<PathBuf> {
    if override_present {
        return None;
    }
    home.filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".bashrc"))
}

fn startup_file_from(
    override_path: Option<OsString>,
    config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return (!path.is_empty()).then(|| PathBuf::from(path));
    }
    if let Some(path) = config_home.filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("isksh").join(".iskrc"));
    }
    home.filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".config").join("isksh").join(".iskrc"))
}

pub fn load_startup_file(shell: &mut Shell, path: &Path) -> io::Result<Option<RunResult>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let source = String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{}: input must be valid UTF-8 (invalid byte at offset {})",
                path.display(),
                error.utf8_error().valid_up_to()
            ),
        )
    })?;
    Ok(Some(shell.run(&source, &[])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_override_xdg_home_and_disabled_paths() {
        let _ = startup_file();
        let _ = bash_startup_file();
        assert_eq!(
            startup_file_from(Some("custom.rc".into()), None, None),
            Some(PathBuf::from("custom.rc"))
        );
        assert_eq!(
            startup_file_from(None, Some("config".into()), None),
            Some(PathBuf::from("config/isksh/.iskrc"))
        );
        assert_eq!(
            startup_file_from(None, None, Some("home".into())),
            Some(PathBuf::from("home/.config/isksh/.iskrc"))
        );
        assert_eq!(startup_file_from(Some(OsString::new()), None, None), None);
        assert_eq!(startup_file_from(None, None, None), None);
        assert_eq!(bash_startup_file_from(true, Some("home".into())), None);
        assert_eq!(
            bash_startup_file_from(false, Some("home".into())),
            Some(PathBuf::from("home/.bashrc"))
        );
        assert_eq!(bash_startup_file_from(false, Some(OsString::new())), None);
        assert_eq!(bash_startup_file_from(false, None), None);
    }

    #[test]
    fn loads_supported_bashrc_style_syntax_and_handles_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".iskrc");
        fs::write(
            &path,
            concat!(
                "# bashrc-style configuration\n",
                "export ISKSH_TEST=configured\n",
                "alias greet='printf configured'\n",
                "prompt_name() { printf '%s' \"$ISKSH_TEST\"; }\n",
            ),
        )
        .unwrap();
        let mut shell = Shell::default();
        assert_eq!(
            load_startup_file(&mut shell, &path)
                .unwrap()
                .unwrap()
                .status,
            0
        );
        assert_eq!(
            shell.run("greet; prompt_name", &[]).stdout,
            b"configuredconfigured"
        );
        assert!(
            load_startup_file(&mut shell, &directory.path().join("missing"))
                .unwrap()
                .is_none()
        );

        fs::write(&path, [0xff]).unwrap();
        assert_eq!(
            load_startup_file(&mut shell, &path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(load_startup_file(&mut shell, directory.path()).is_err());
    }
}
