param(
    [switch]$Installer
)

$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'build-tools.ps1')

$releaseTargetDir = Join-Path $projectRoot '.tools\release-target'
$publishedDir = Join-Path $projectRoot 'artifacts'
$publishedBinary = Join-Path $publishedDir 'alldownloader.exe'
$publishedSidecarDir = Join-Path $publishedDir 'binaries'
$sidecarName = 'aria2-next-x86_64-pc-windows-msvc.exe'
$sidecarSource = Join-Path $projectRoot "src-tauri\binaries\$sidecarName"
$publishedSidecar = Join-Path $publishedSidecarDir $sidecarName
$xunleiHelperName = 'xunlei-native-helper.cjs'
$xunleiHelperSource = Join-Path $projectRoot "src-tauri\binaries\$xunleiHelperName"
$publishedXunleiHelper = Join-Path $publishedSidecarDir $xunleiHelperName
$legacyPublishedSidecar = Join-Path $publishedSidecarDir 'aria2c-x86_64-pc-windows-msvc.exe'
$buildTools = Get-AllDownloaderBuildTools -ProjectRoot $projectRoot
$pnpmCommand = $buildTools.Pnpm
$cargoCommand = $buildTools.Cargo
$env:Path = "$(Split-Path -Parent $pnpmCommand);$(Split-Path -Parent $cargoCommand);$env:Path"
$env:CARGO_TARGET_DIR = $releaseTargetDir
$env:CARGO_INCREMENTAL = '1'

$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path -LiteralPath $vcvars)) {
    throw "Visual Studio C++ build environment was not found: $vcvars"
}

Set-Location $projectRoot
if ($Installer) {
    $command = '"{0}" >nul && "{1}" exec tauri build --runner "{2}"' -f $vcvars, $pnpmCommand, $cargoCommand
} else {
    # Keep the distributable binary out of the shared Cargo target. Other
    # development builds may omit `custom-protocol` and would otherwise
    # overwrite the release executable with one that points at the Vite URL.
    $frontendInputs = @(
        (Join-Path $projectRoot 'index.html'),
        (Join-Path $projectRoot 'package.json'),
        (Join-Path $projectRoot 'pnpm-lock.yaml')
    )
    $frontendInputs += Get-ChildItem -LiteralPath (Join-Path $projectRoot 'src') -File -Recurse
    $frontendInputs += Get-ChildItem -LiteralPath $projectRoot -File -Filter 'tsconfig*.json'
    $frontendInputs += Get-ChildItem -LiteralPath $projectRoot -File -Filter 'vite.config.*'
    $publicDir = Join-Path $projectRoot 'public'
    if (Test-Path -LiteralPath $publicDir) {
        $frontendInputs += Get-ChildItem -LiteralPath $publicDir -File -Recurse
    }

    $signatureLines = foreach ($input in $frontendInputs | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Sort-Object { $_.FullName }) {
        $path = if ($input -is [System.IO.FileInfo]) { $input.FullName } else { [string]$input }
        $relativePath = $path.Substring($projectRoot.Length).TrimStart([char[]]@([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar))
        '{0}:{1}' -f $relativePath, (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    }
    $signatureBytes = [System.Text.Encoding]::UTF8.GetBytes(($signatureLines -join "`n"))
    $signatureHasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $frontendSignature = -join ($signatureHasher.ComputeHash($signatureBytes) | ForEach-Object { $_.ToString('x2') })
    } finally {
        $signatureHasher.Dispose()
    }
    $cacheDir = Join-Path $projectRoot '.tools\build-cache'
    $signatureFile = Join-Path $cacheDir 'frontend.sha256'
    $distDir = Join-Path $projectRoot 'dist'
    $previousSignature = if (Test-Path -LiteralPath $signatureFile) { (Get-Content -LiteralPath $signatureFile -Raw).Trim() } else { '' }

    if ($frontendSignature -ne $previousSignature -or -not (Test-Path -LiteralPath $distDir)) {
        & $pnpmCommand run build
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null
        Set-Content -LiteralPath $signatureFile -Value $frontendSignature -NoNewline
    } else {
        Write-Host 'Frontend unchanged; reusing dist.'
    }

    $command = '"{0}" >nul && "{1}" build --manifest-path "{2}" --release --features custom-protocol' -f $vcvars, $cargoCommand, (Join-Path $projectRoot 'src-tauri\Cargo.toml')
}
& cmd.exe /d /s /c $command
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$builtBinary = Join-Path $releaseTargetDir 'release\alldownloader.exe'
if (-not (Test-Path -LiteralPath $builtBinary)) {
    throw "Release binary was not produced: $builtBinary"
}
New-Item -ItemType Directory -Force -Path $publishedDir | Out-Null
Copy-Item -LiteralPath $builtBinary -Destination $publishedBinary -Force
if (-not (Test-Path -LiteralPath $sidecarSource -PathType Leaf)) {
    throw "Aria2 Next sidecar was not found: $sidecarSource"
}
New-Item -ItemType Directory -Force -Path $publishedSidecarDir | Out-Null
Copy-Item -LiteralPath $sidecarSource -Destination $publishedSidecar -Force
if (-not (Test-Path -LiteralPath $xunleiHelperSource -PathType Leaf)) {
    throw "Xunlei native helper was not found: $xunleiHelperSource"
}
Copy-Item -LiteralPath $xunleiHelperSource -Destination $publishedXunleiHelper -Force
if (Test-Path -LiteralPath $legacyPublishedSidecar -PathType Leaf) {
    Remove-Item -LiteralPath $legacyPublishedSidecar -Force
}
Write-Host "Published: $publishedBinary"
Write-Host "Published: $publishedSidecar"
Write-Host "Published: $publishedXunleiHelper"
