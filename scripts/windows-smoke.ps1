$ErrorActionPreference = 'Stop'
$binary = Resolve-Path 'dist/isksh-windows-x86_64.exe'
$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("isksh-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $sandbox | Out-Null
try {
    Copy-Item $binary (Join-Path $sandbox 'isksh.exe')
    Set-Content -LiteralPath (Join-Path $sandbox 'sample.cmd') -Encoding Ascii -Value '@echo cmd-ok'
    foreach ($tool in @('mise', 'nvim', 'lazygit', 'yazi', 'zellij', 'codex')) {
        Set-Content -LiteralPath (Join-Path $sandbox "$tool.cmd") -Encoding Ascii -Value "@echo $tool-ok"
    }
    Push-Location $sandbox
    try {
        $output = & .\isksh.exe -c 'value=windows; printf "%s" "$value"'
        if ($LASTEXITCODE -ne 0 -or $output -ne 'windows') {
            throw "isksh Windows smoke test failed"
        }
        $cmdOutput = & .\isksh.exe -c 'sample'
        if ($LASTEXITCODE -ne 0 -or $cmdOutput -ne 'cmd-ok') {
            throw "isksh Windows PATHEXT smoke test failed"
        }
        foreach ($tool in @('mise', 'nvim', 'lazygit', 'yazi', 'zellij', 'codex')) {
            $toolOutput = & .\isksh.exe -c $tool
            if ($LASTEXITCODE -ne 0 -or $toolOutput -ne "$tool-ok") {
                throw "isksh Windows tool smoke test failed: $tool"
            }
        }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item -LiteralPath $sandbox -Recurse -Force
}
