use crate::{RunResult, Shell};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupFiles {
    pub env: PathBuf,
    pub profile: PathBuf,
    pub rc: PathBuf,
}

pub fn startup_files() -> Option<StartupFiles> {
    startup_files_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME").or(std::env::var_os("USERPROFILE")),
    )
}

fn startup_files_from(
    config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<StartupFiles> {
    let config = config_home
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .map(|path| path.join(".config"))
        })?
        .join("isksh");
    Some(StartupFiles {
        env: config.join(".iskenv"),
        profile: config.join(".iskprofile"),
        rc: config.join(".iskrc"),
    })
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
    fn resolves_xdg_home_and_missing_paths() {
        let _ = startup_files();
        assert_eq!(
            startup_files_from(Some("config".into()), None),
            Some(StartupFiles {
                env: PathBuf::from("config/isksh/.iskenv"),
                profile: PathBuf::from("config/isksh/.iskprofile"),
                rc: PathBuf::from("config/isksh/.iskrc"),
            })
        );
        assert_eq!(
            startup_files_from(None, Some("home".into())),
            Some(StartupFiles {
                env: PathBuf::from("home/.config/isksh/.iskenv"),
                profile: PathBuf::from("home/.config/isksh/.iskprofile"),
                rc: PathBuf::from("home/.config/isksh/.iskrc"),
            })
        );
        assert_eq!(startup_files_from(Some(OsString::new()), None), None);
        assert_eq!(startup_files_from(None, None), None);
    }

    #[test]
    fn loads_supported_configuration_and_handles_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".iskrc");
        fs::write(
            &path,
            concat!(
                "# shell configuration\n",
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
