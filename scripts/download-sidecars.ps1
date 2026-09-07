$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$toolsRoot = Join-Path $projectRoot '.tools'
$archive = Join-Path $toolsRoot 'aria2-1.37.0-win-64bit-build1.zip'
$extractRoot = Join-Path $toolsRoot 'aria2-1.37.0'
$source = Join-Path $extractRoot 'aria2-1.37.0-win-64bit-build1\aria2c.exe'
$destinationRoot = Join-Path $projectRoot 'src-tauri\binaries'
$destination = Join-Path $destinationRoot 'aria2c-x86_64-pc-windows-msvc.exe'
$expectedSha256 = 'BE2099C214F63A3CB4954B09A0BECD6E2E34660B886D4C898D260FEBFE9D70C2'

New-Item -ItemType Directory -Force -Path $toolsRoot, $destinationRoot | Out-Null
Invoke-WebRequest -Uri 'https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip' -OutFile $archive
Expand-Archive -LiteralPath $archive -DestinationPath $extractRoot -Force
if (-not (Test-Path -LiteralPath $source)) { throw 'aria2c.exe was not found in the official archive.' }
Copy-Item -LiteralPath $source -Destination $destination -Force
$actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash
if ($actualSha256 -ne $expectedSha256) { throw "aria2 hash mismatch: $actualSha256" }
Write-Output "aria2 sidecar ready: $destination"
