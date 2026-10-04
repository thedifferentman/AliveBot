$ErrorActionPreference = 'Stop'
$taskPreviousHome = $env:CODEX_HOME
try {
    $env:CODEX_HOME = Join-Path $PSScriptRoot 'codex-home'
    $taskConfiguration = Get-Content (Join-Path $PSScriptRoot 'codex-service/config.json') -Raw | ConvertFrom-Json
    & $taskConfiguration.executable login
} finally {
    if ($null -eq $taskPreviousHome) { Remove-Item Env:CODEX_HOME -ErrorAction SilentlyContinue }
    else { $env:CODEX_HOME = $taskPreviousHome }
}
