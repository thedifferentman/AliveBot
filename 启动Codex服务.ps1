$ErrorActionPreference = 'Stop'
$taskProject = $PSScriptRoot
$taskNode = (Get-Command node -ErrorAction Stop).Source
& $taskNode (Join-Path $taskProject 'codex-service/server.mjs')
