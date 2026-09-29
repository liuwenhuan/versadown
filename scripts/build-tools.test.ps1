$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-tools.ps1')

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("versadown-build-tools-test-" + [guid]::NewGuid())
$originalPath = $env:Path
$originalUserProfile = $env:USERPROFILE
$originalVcvarsPath = $env:VCVARS_PATH
$originalRustupHome = $env:RUSTUP_HOME
try {
    $binDir = Join-Path $testRoot 'bin'
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    $pnpmPath = Join-Path $binDir 'pnpm.cmd'
    Set-Content -LiteralPath $pnpmPath -Value "@echo off`r`necho 10.0.0`r`nexit /b 0" -NoNewline
    $env:USERPROFILE = $testRoot
    $env:RUSTUP_HOME = Join-Path $originalUserProfile '.rustup'
    $cargoBinDir = Join-Path $originalUserProfile '.cargo\bin'
    $env:Path = "$binDir;$cargoBinDir;$originalPath"

    $tools = Get-VersaDownBuildTools -ProjectRoot $testRoot
    if ($tools.Pnpm -ne $pnpmPath) {
        throw "Expected pnpm from PATH, got $($tools.Pnpm)"
    }

    $vcvarsPath = Join-Path $testRoot 'vcvars64.bat'
    Set-Content -LiteralPath $vcvarsPath -Value '@echo off' -NoNewline
    $env:VCVARS_PATH = $vcvarsPath
    if ((Get-VersaDownMsvcEnvironment) -ne $vcvarsPath) {
        throw 'Expected the explicit MSVC environment path to be selected.'
    }

    Write-Host 'PASS: CI-provided pnpm and MSVC paths are discovered.'
} finally {
    $env:Path = $originalPath
    $env:USERPROFILE = $originalUserProfile
    $env:VCVARS_PATH = $originalVcvarsPath
    $env:RUSTUP_HOME = $originalRustupHome
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
