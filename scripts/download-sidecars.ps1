$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$destinationRoot = Join-Path $projectRoot 'src-tauri\binaries'
$destination = Join-Path $destinationRoot 'aria2-next-x86_64-pc-windows-msvc.exe'
$expectedSha256 = 'AD5159A68B3132D83444D4BB8285D750F5C3579FE581FC14211426B30BD39D35'

if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
    throw "Bundled aria2-next media sidecar is missing: $destination"
}

$actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash
if ($actualSha256 -ne $expectedSha256) { throw "aria2 hash mismatch: $actualSha256" }
$versionOutput = (& $destination --version 2>&1) -join "`n"
if ($versionOutput -notmatch 'HLS/DASH') {
    throw 'Bundled aria2-next sidecar does not include native HLS/DASH support'
}
Write-Output "aria2-next sidecar ready: $destination"
