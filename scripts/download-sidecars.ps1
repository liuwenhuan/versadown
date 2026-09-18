$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$destinationRoot = Join-Path $projectRoot 'src-tauri\binaries'
$destination = Join-Path $destinationRoot 'aria2-next-x86_64-pc-windows-msvc.exe'
$expectedVersion = '2.8.0'
$expectedSha256 = '74183154FA993A96FFD8C21B971B4783E05F0B2EC189E134A3B6F92292865758'

if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
    throw "Bundled aria2-next media sidecar is missing: $destination"
}

$actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash
if ($actualSha256 -ne $expectedSha256) { throw "aria2 hash mismatch: $actualSha256" }
$versionOutput = (& $destination --version 2>&1) -join "`n"
if ($versionOutput -notmatch "Aria2 Next version $([regex]::Escape($expectedVersion))") {
    throw "Bundled aria2-next is not version $expectedVersion"
}
if ($versionOutput -notmatch 'HLS/DASH') {
    throw 'Bundled aria2-next sidecar does not include native HLS/DASH support'
}
Write-Output "aria2-next sidecar ready: $destination"
