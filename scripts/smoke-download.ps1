param(
    # Allows the real download test to run before packaging; default checks all formats.
    [switch]$SkipPackageCheck
)

$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$cargoCommand = Get-Command cargo.exe -ErrorAction SilentlyContinue
if ($cargoCommand) {
    $cargoPath = $cargoCommand.Source
} else {
    $cargoPath = @(
        (Join-Path $projectRoot '.tools\cargo\bin\cargo.exe'),
        (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe')
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
}
if (-not $cargoPath) { throw 'Cargo is required to verify the real bundled aria2 download lifecycle.' }

$targetRoot = if ($env:CARGO_TARGET_DIR) {
    if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { $env:CARGO_TARGET_DIR }
    else { Join-Path $projectRoot $env:CARGO_TARGET_DIR }
} else { Join-Path $projectRoot 'src-tauri\target' }

Push-Location $projectRoot
try {
    # The Rust fixture binds only 127.0.0.1, disables peer discovery, owns its child,
    # uses temporary data, and bounds every condition poll. No public downloads.
    & $cargoPath test --manifest-path src-tauri/Cargo.toml --test aria2_live -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw "Real aria2 download smoke failed (exit $LASTEXITCODE)." }

    if ($SkipPackageCheck) {
        Write-Output 'Real loopback download smoke passed; package checks explicitly skipped.'
    } else {
        $releaseRoot = Join-Path $targetRoot 'release'
        $application = Get-Item -LiteralPath (Join-Path $releaseRoot 'versadown.exe') -ErrorAction Stop
        $msi = Get-ChildItem -LiteralPath (Join-Path $releaseRoot 'bundle\msi') -Filter *.msi -File -ErrorAction Stop
        $nsis = Get-ChildItem -LiteralPath (Join-Path $releaseRoot 'bundle\nsis') -Filter *.exe -File -ErrorAction Stop
        if (-not $msi -or -not $nsis) { throw 'Both MSI and NSIS Windows package artifacts are required.' }
        foreach ($artifact in @($application) + @($msi) + @($nsis)) {
            if ($artifact.Length -lt 1MB) { throw "Windows artifact is unexpectedly small: $($artifact.FullName)" }
            Write-Output "Windows artifact ready: $($artifact.FullName) ($([math]::Round($artifact.Length / 1MB, 1)) MB)"
        }
        Write-Output 'Real loopback download smoke and EXE/MSI/NSIS artifact checks passed.'
    }
} finally {
    Pop-Location
}
