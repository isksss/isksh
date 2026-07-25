param(
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = "Stop"
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "invalid release version: $Version"
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$workDir = Join-Path ([System.IO.Path]::GetTempPath()) ("isksh-aqua-" + [guid]::NewGuid())
$aquaRoot = Join-Path $workDir "root"

try {
    New-Item -ItemType Directory -Path $workDir | Out-Null
    Copy-Item (Join-Path $repoRoot "aqua\aqua.yaml") $workDir
    Copy-Item (Join-Path $repoRoot "aqua\registry.yaml") $workDir

    $env:AQUA_ROOT_DIR = $aquaRoot
    $env:AQUA_DISABLE_POLICY = "true"
    $config = Join-Path $workDir "aqua.yaml"
    $env:AQUA_CONFIG = $config

    & aqua g -i "local,isksss/isksh@$Version"
    if ($LASTEXITCODE -ne 0) { throw "aqua generate failed" }

    & aqua update-checksum
    if ($LASTEXITCODE -ne 0) { throw "aqua checksum update failed" }

    & aqua install
    if ($LASTEXITCODE -ne 0) { throw "aqua install failed" }

    $env:Path = "$aquaRoot\bin;$aquaRoot\bat;$env:Path"
    $output = & isksh -c 'printf aqua-ok'
    if ($LASTEXITCODE -ne 0 -or $output -ne "aqua-ok") {
        throw "installed isksh smoke test failed"
    }
}
finally {
    Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
}
