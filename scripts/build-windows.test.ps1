$ErrorActionPreference = 'Stop'

function Assert-True {
    param(
        [Parameter(Mandatory)]
        [bool]$Condition,

        [Parameter(Mandatory)]
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Write-TestFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Content
    )

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
    Set-Content -LiteralPath $Path -Value $Content -NoNewline
}

$scriptSource = Get-Content (Join-Path $PSScriptRoot 'build-windows.ps1') -Raw
$functionsStart = $scriptSource.IndexOf('function Get-FileSha256')
$functionsEnd = $scriptSource.IndexOf('function Publish-BuildOutput')
if ($functionsStart -lt 0 -or $functionsEnd -lt 0) {
    throw 'Could not load the quick-build cache helpers.'
}
Invoke-Expression $scriptSource.Substring($functionsStart, $functionsEnd - $functionsStart)

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("alldownloader-build-test-" + [guid]::NewGuid())
try {
    foreach ($relativePath in @(
        'index.html',
        'package.json',
        'pnpm-lock.yaml',
        'scripts/build-windows.ps1',
        'src/App.tsx',
        'src-tauri/Cargo.toml',
        'src-tauri/Cargo.lock',
        'src-tauri/build.rs',
        'src-tauri/tauri.conf.json'
    )) {
        Write-TestFile -Path (Join-Path $testRoot $relativePath) -Content "before-$relativePath"
    }

    $targetRoot = Join-Path $testRoot 'target'
    $baselineFrontendSignature = Get-FrontendSignature -ProjectRoot $testRoot
    $baselineSignature = Get-QuickRustSignature -ProjectRoot $testRoot
    Save-QuickRustSignature -ProjectRoot $testRoot -Signature $baselineSignature

    Write-TestFile -Path (Join-Path $testRoot 'src/App.tsx') -Content 'after-frontend-change'
    $changedFrontendSignature = Get-FrontendSignature -ProjectRoot $testRoot
    $changedSignature = Get-QuickRustSignature -ProjectRoot $testRoot
    Assert-True -Condition ($baselineFrontendSignature -ne $changedFrontendSignature) -Message 'The frontend signature must change when frontend source changes.'
    Assert-True -Condition ($baselineSignature -ne $changedSignature) -Message 'The quick-build signature must change when frontend source changes.'
    $fakeCargo = Join-Path $testRoot 'fake-cargo.cmd'
    $cleanMarker = Join-Path $testRoot 'cargo-clean-called.txt'
    Write-TestFile -Path $fakeCargo -Content "@echo off`r`necho %* > `"$cleanMarker`"`r`nexit /b 0"

    Reset-QuickRustBuildIfInputsChanged -CargoCommand $fakeCargo -ProjectRoot $testRoot -TargetRoot $targetRoot | Out-Null

    Assert-True -Condition (-not (Test-Path -LiteralPath $cleanMarker)) -Message 'A frontend-only change must not delete the incremental Rust target.'

    $blocked = $false
    try {
        Assert-PublishedBinaryAvailable -PublishedBinary (Join-Path $testRoot 'artifacts/quick/alldownloader.exe') -Processes @([pscustomobject]@{
            Id = 77
            Path = (Join-Path $testRoot 'artifacts/quick/alldownloader.exe')
        })
    } catch {
        $blocked = $_.Exception.Message -match 'PID 77'
    }
    Assert-True -Condition $blocked -Message 'The build must stop before compiling when its published executable is running.'

    $projectRoot = $testRoot
    $vcvars = Join-Path $testRoot 'vcvars64.bat'
    $compileMarker = Join-Path $testRoot 'frontend-signature.txt'
    $compileCargo = Join-Path $testRoot 'compile-cargo.cmd'
    Write-TestFile -Path $vcvars -Content '@echo off'
    Write-TestFile -Path $compileCargo -Content "@echo off`r`necho %ALDOWNLOADER_FRONTEND_SIGNATURE% > `"$compileMarker`"`r`nexit /b 0"

    Invoke-MsvcBuild -VcvVars $vcvars -CargoCommand $compileCargo -PnpmCommand 'pnpm' -TargetRoot $targetRoot -Profile quick -FrontendSignature $changedFrontendSignature

    Assert-True -Condition ((Get-Content -LiteralPath $compileMarker -Raw).Trim() -eq $changedFrontendSignature) -Message 'The quick Cargo build must receive the current frontend signature.'
    Write-Host 'PASS: frontend-only changes preserve the incremental Rust target.'
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
