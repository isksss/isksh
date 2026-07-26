use std::fs;
use std::path::Path;

/// リポジトリ内のUTF-8テキストファイルを読み込む。
fn read_repository_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

/// Release CIが全対応OSのバイナリとチェックサムを公開することを確認する。
#[test]
fn release_workflow_distributes_every_supported_platform() {
    let workflow = read_repository_file(".github/workflows/release.yml");
    for artifact in [
        "isksh-linux-x86_64",
        "isksh-linux-aarch64",
        "isksh-windows-x86_64.exe",
        "isksh-macos-x86_64",
        "isksh-macos-aarch64",
    ] {
        assert!(
            workflow.contains(artifact),
            "成果物がありません: {artifact}"
        );
    }
    assert!(workflow.contains("macos-15-intel"));
    assert!(workflow.contains("macos-15"));
    assert!(workflow.contains("merge-multiple: true"));
    assert!(workflow.contains("sha256sum --check dist/*.sha256"));
}

/// aquaがLinux・Windows・macOSの成果物を選択できることを確認する。
#[test]
fn aqua_registry_supports_every_operating_system() {
    let registry = read_repository_file("aqua/registry.yaml");
    for environment in ["linux", "windows/amd64", "darwin"] {
        assert!(
            registry.contains(&format!("- {environment}")),
            "aqua対応環境がありません: {environment}"
        );
    }
    assert!(registry.contains("darwin: macos"));
    assert!(registry.contains("arm64: aarch64"));

    let workflow = read_repository_file(".github/workflows/release.yml");
    assert!(workflow.contains("aqua-smoke-linux:"));
    assert!(workflow.contains("aqua-smoke-windows:"));
    assert!(workflow.contains("aqua-smoke-macos:"));
}
