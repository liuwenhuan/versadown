$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$toolsRoot = Join-Path $projectRoot '.tools'
$version = '2.7.5'
$assetName = "aria2-next-$version-windows-x86_64.exe"
$source = Join-Path $toolsRoot $assetName
$checksums = Join-Path $toolsRoot "aria2-next-$version-checksums.sha256"
$destinationRoot = Join-Path $projectRoot 'src-tauri\binaries'
$destination = Join-Path $destinationRoot 'aria2-next-x86_64-pc-windows-msvc.exe'
$expectedSha256 = '7C1F49BF9F22F15F684CE1DCC76768F04B5DC3795281AEF750D1E5AD78FF1459'
$releaseRoot = "https://github.com/AnInsomniacy/aria2-next/releases/download/v$version"

New-Item -ItemType Directory -Force -Path $toolsRoot, $destinationRoot | Out-Null
Invoke-WebRequest -Uri "$releaseRoot/$assetName" -OutFile $source
Invoke-WebRequest -Uri "$releaseRoot/aria2-next-$version-checksums.sha256" -OutFile $checksums
$publishedLine = Get-Content -LiteralPath $checksums | Where-Object { $_ -match "\s+$([regex]::Escape($assetName))$" } | Select-Object -First 1
if (-not $publishedLine) { throw "Official checksum does not list $assetName" }
$publishedSha256 = ($publishedLine -split '\s+')[0].ToUpperInvariant()
if ($publishedSha256 -ne $expectedSha256) { throw "Pinned aria2-next checksum differs from the official manifest: $publishedSha256" }
$actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash
if ($actualSha256 -ne $expectedSha256) { throw "aria2 hash mismatch: $actualSha256" }
Copy-Item -LiteralPath $source -Destination $destination -Force
Write-Output "aria2-next sidecar ready: $destination"
