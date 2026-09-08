$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$nodeBin = Join-Path $projectRoot '.tools\node-v24.20.0-win-x64'
$installedNodeBin = Join-Path $env:LOCALAPPDATA 'Programs\nodejs'
$cargoHome = Join-Path $projectRoot '.tools\cargo'
$rustupHome = Join-Path $projectRoot '.tools\rustup'

if (Test-Path -LiteralPath $nodeBin) {
    $env:Path = "$nodeBin;$env:Path"
} elseif (Test-Path -LiteralPath (Join-Path $installedNodeBin 'npm.cmd')) {
    $env:Path = "$installedNodeBin;$env:Path"
}
if (Test-Path -LiteralPath (Join-Path $cargoHome 'bin\cargo.exe')) {
    $env:CARGO_HOME = $cargoHome
    $env:Path = "$cargoHome\bin;$env:Path"
}
if (Test-Path -LiteralPath $rustupHome) {
    $env:RUSTUP_HOME = $rustupHome
}

$npmCommand = (Get-Command npm.cmd -ErrorAction Stop).Source
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path -LiteralPath $vcvars)) {
    throw "Visual Studio C++ build environment was not found: $vcvars"
}

Set-Location $projectRoot
$command = '"{0}" >nul && "{1}" run tauri:build' -f $vcvars, $npmCommand
& cmd.exe /d /s /c $command
exit $LASTEXITCODE
