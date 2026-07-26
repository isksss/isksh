use crate::i18n::localize;
use crate::{RunResult, Shell};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// iskshが認識する起動ファイルのパス。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupFiles {
    /// 起動のたびに読み込む環境ファイル。
    pub env: PathBuf,
    /// ログインシェルが読み込むプロファイルファイル。
    pub profile: PathBuf,
    /// 対話シェルが読み込む実行時設定ファイル。
    pub rc: PathBuf,
}

/// 現在の環境から起動ファイルのパスを解決する。
///
/// `XDG_CONFIG_HOME`を優先し、未設定なら`$HOME/.config`を使用する。
/// Windowsでは`USERPROFILE`をホームディレクトリの代替として扱う。
/// 利用可能な設定またはホームディレクトリがなければ`None`を返す。
pub fn startup_files() -> Option<StartupFiles> {
    startup_files_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME").or(std::env::var_os("USERPROFILE")),
    )
}

/// `startup_files_from`に対応する処理を行う。
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

/// 起動ファイルを一つ読み込んで実行する。
///
/// `path`が存在しなければ`Ok(None)`、既存ファイルを実行した場合は`Ok(Some(_))`を返す。
///
/// # エラー
///
/// ファイルを読み込めない場合はI/Oエラー、内容が有効なUTF-8でない場合は
/// [`io::ErrorKind::InvalidData`]を返す。
pub fn load_startup_file(shell: &mut Shell, path: &Path) -> io::Result<Option<RunResult>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let source = String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            localize(format!(
                "{}: input must be valid UTF-8 (invalid byte at offset {})",
                path.display(),
                error.utf8_error().valid_up_to()
            )),
        )
    })?;
    Ok(Some(shell.run(&source, &[])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `resolves_xdg_home_and_missing_paths`に対応する処理を行う。
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
    /// `loads_supported_configuration_and_handles_errors`に対応する処理を行う。
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
