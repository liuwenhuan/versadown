$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$artifactRoot = Join-Path $projectRoot 'src-tauri\target\release\bundle'
$artifact = Get-ChildItem -LiteralPath $artifactRoot -Recurse -Include *.msi,*.exe -File -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $artifact) { throw 'Windows package artifact was not produced.' }
if ($artifact.Length -lt 1MB) { throw "Windows package artifact is unexpectedly small: $($artifact.FullName)" }
Write-Output "Windows package ready: $($artifact.FullName) ($([math]::Round($artifact.Length / 1MB, 1)) MB)"
