$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$destinationRoot = Join-Path $projectRoot 'src-tauri\binaries'
$destination = Join-Path $destinationRoot 'aria2-next-x86_64-pc-windows-msvc.exe'
$expectedVersion = '3.0.9'
$expectedSha256 = 'E74899CE4A39815A509BC663D29ACA5028F9BDD6B390FE33141EB11D478D1C13'

if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
    throw "Bundled aria2-next media sidecar is missing: $destination"
}

$actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash
if ($actualSha256 -ne $expectedSha256) { throw "aria2 hash mismatch: $actualSha256" }
$versionOutput = (& $destination --version 2>&1) -join "`n"
if ($versionOutput -notmatch "aria2[- ]next version $([regex]::Escape($expectedVersion))") {
    throw "Bundled aria2-next is not version $expectedVersion"
}
if ($versionOutput -notmatch 'HLS/DASH') {
    throw 'Bundled aria2-next sidecar does not include native HLS/DASH support'
}
Write-Output "aria2-next sidecar ready: $destination"
