$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$nodeBin = Join-Path $projectRoot '.tools\node-v24.20.0-win-x64'
$cargoHome = Join-Path $projectRoot '.tools\cargo'
$rustupHome = Join-Path $projectRoot '.tools\rustup'
$env:CARGO_HOME = $cargoHome
$env:RUSTUP_HOME = $rustupHome
$env:Path = "$nodeBin;$cargoHome\bin;$env:Path"

Set-Location $projectRoot
& cmd.exe /d /s /c '"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul && npm run tauri:build'
exit $LASTEXITCODE
