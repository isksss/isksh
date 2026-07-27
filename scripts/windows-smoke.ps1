$ErrorActionPreference = 'Stop'
$binary = Resolve-Path 'dist/isksh-windows-x86_64.exe'
$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("isksh-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $sandbox | Out-Null
try {
    Copy-Item $binary (Join-Path $sandbox 'isksh.exe')
    Set-Content -LiteralPath (Join-Path $sandbox 'sample.cmd') -Encoding Ascii -Value '@echo cmd-ok'
    Set-Content -LiteralPath (Join-Path $sandbox 'read-pipe.cmd') -Encoding Ascii -Value '@more < %1'
    Set-Content -LiteralPath (Join-Path $sandbox 'stream-producer.cmd') -Encoding Ascii -Value @(
        '@echo stream-ok'
        ':wait'
        '@if not exist stream-observed (ping -n 2 127.0.0.1 >nul & goto wait)'
    )
    Set-Content -LiteralPath (Join-Path $sandbox 'stream-consumer.cmd') -Encoding Ascii -Value @(
        '@set /p line='
        '@echo %line%'
        '@type nul > stream-observed'
    )
    Set-Content -LiteralPath (Join-Path $sandbox 'loaded') -Encoding Ascii -Value 'print autoload-ok'
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
        $processInput = & .\isksh.exe -c 'read-pipe <(printf process-input)'
        if ($LASTEXITCODE -ne 0 -or $processInput -ne 'process-input') {
            throw "isksh Windows process input substitution smoke test failed"
        }
        $processOutput = & .\isksh.exe -c 'stream-producer > >(stream-consumer)'
        if ($LASTEXITCODE -ne 0 -or $processOutput -ne 'stream-ok' -or -not (Test-Path 'stream-observed')) {
            throw "isksh Windows process output substitution smoke test failed"
        }
        & .\isksh.exe -c ': <(printf unused)' | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "isksh Windows unused process substitution cleanup smoke test failed"
        }
        $previousErrorAction = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            & .\isksh.exe -c 'printf <(printf unused) $((1/0))' 2>$null | Out-Null
            $failedExpansionStatus = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousErrorAction
        }
        if ($failedExpansionStatus -eq 0) {
            throw "isksh Windows failed expansion cleanup smoke test failed"
        }
        foreach ($tool in @('mise', 'nvim', 'lazygit', 'yazi', 'zellij', 'codex')) {
            $toolOutput = & .\isksh.exe -c $tool
            if ($LASTEXITCODE -ne 0 -or $toolOutput -ne "$tool-ok") {
                throw "isksh Windows tool smoke test failed: $tool"
            }
        }
        $previousMode = $env:ISKSH_MODE
        $previousFpath = $env:FPATH
        try {
            $env:ISKSH_MODE = 'zsh'
            $env:FPATH = $sandbox
            $zshOutput = @(& .\isksh.exe -c 'values=(one two); print -r -- ${values[1]}:$((2 ** 3)); [[ abc123 =~ ''([a-z]+)([0-9]+)'' ]] && print -r -- $MATCH')
            if ($LASTEXITCODE -ne 0 -or $zshOutput.Count -ne 2 -or $zshOutput[0] -ne 'one:8' -or $zshOutput[1] -ne 'abc123') {
                throw "isksh Windows zsh compatibility smoke test failed"
            }
            $autoloadOutput = & .\isksh.exe -c 'autoload loaded; loaded'
            if ($LASTEXITCODE -ne 0 -or $autoloadOutput -ne 'autoload-ok') {
                throw "isksh Windows zsh autoload smoke test failed"
            }
            $builtinOutput = & .\isksh.exe -c 'compinit; compadd alpha; bindkey -M main ''^X'' complete-word; print -r -- ${+builtins[compadd]}'
            if ($LASTEXITCODE -ne 0 -or $builtinOutput -ne '1') {
                throw "isksh Windows zsh builtin smoke test failed"
            }
        } finally {
            $env:ISKSH_MODE = $previousMode
            $env:FPATH = $previousFpath
        }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item -LiteralPath $sandbox -Recurse -Force
}
