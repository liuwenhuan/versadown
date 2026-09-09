param(
    [switch]$Installer
)

$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$nodeBin = Join-Path $projectRoot '.tools\node-v24.20.0-win-x64'
$installedNodeBin = Join-Path $env:LOCALAPPDATA 'Programs\nodejs'
$cargoHome = Join-Path $projectRoot '.tools\cargo'
$rustupHome = Join-Path $projectRoot '.tools\rustup'
$releaseTargetDir = Join-Path $projectRoot '.tools\release-target'
$publishedDir = Join-Path $projectRoot 'artifacts'
$publishedBinary = Join-Path $publishedDir 'alldownloader.exe'
$installedCargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (Test-Path -LiteralPath $nodeBin) {
    $env:Path = "$nodeBin;$env:Path"
} elseif (Test-Path -LiteralPath (Join-Path $installedNodeBin 'npm.cmd')) {
    $env:Path = "$installedNodeBin;$env:Path"
}
if (Test-Path -LiteralPath (Join-Path $cargoHome 'bin\cargo.exe')) {
    $env:CARGO_HOME = $cargoHome
    $env:Path = "$cargoHome\bin;$env:Path"
} elseif (Test-Path -LiteralPath (Join-Path $installedCargoBin 'cargo.exe')) {
    $env:Path = "$installedCargoBin;$env:Path"
}
if (Test-Path -LiteralPath $rustupHome) {
    $env:RUSTUP_HOME = $rustupHome
}
$env:CARGO_INCREMENTAL = '1'

$npmCommand = (Get-Command npm.cmd -ErrorAction Stop).Source
$cargoCommand = (Get-Command cargo.exe -ErrorAction Stop).Source
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path -LiteralPath $vcvars)) {
    throw "Visual Studio C++ build environment was not found: $vcvars"
}

Set-Location $projectRoot
if ($Installer) {
    $command = '"{0}" >nul && "{1}" run tauri:build' -f $vcvars, $npmCommand
} else {
    # Keep the distributable binary out of the shared Cargo target. Other
    # development builds may omit `custom-protocol` and would otherwise
    # overwrite the release executable with one that points at the Vite URL.
    $env:CARGO_TARGET_DIR = $releaseTargetDir
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
        & $npmCommand run build
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

if (-not $Installer) {
    $builtBinary = Join-Path $releaseTargetDir 'release\alldownloader.exe'
    if (-not (Test-Path -LiteralPath $builtBinary)) {
        throw "发布二进制未生成: $builtBinary"
    }
    New-Item -ItemType Directory -Force -Path $publishedDir | Out-Null
    Copy-Item -LiteralPath $builtBinary -Destination $publishedBinary -Force
    Write-Host "已发布: $publishedBinary"
}
