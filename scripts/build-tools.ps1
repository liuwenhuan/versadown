function Resolve-VersaDownBuildTool {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$ToolName,

        [Parameter(Mandatory)]
        [string[]]$Candidates
    )

    foreach ($candidate in $Candidates) {
        if ([string]::IsNullOrWhiteSpace($candidate) -or -not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            continue
        }

        $file = Get-Item -LiteralPath $candidate
        $isLink = [bool]($file.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
        if ($file.Length -le 0 -and -not $isLink) {
            continue
        }

        try {
            $probe = New-Object System.Diagnostics.Process
            if ($file.Extension -in @('.cmd', '.bat')) {
                $probe.StartInfo.FileName = $env:ComSpec
                $probe.StartInfo.Arguments = '/d /s /c ""{0}" --version"' -f $file.FullName
            } else {
                $probe.StartInfo.FileName = $file.FullName
                $probe.StartInfo.Arguments = '--version'
            }
            $probe.StartInfo.UseShellExecute = $false
            $probe.StartInfo.CreateNoWindow = $true
            if ($probe.Start()) {
                $probe.WaitForExit()
            }
            if ($probe.HasExited -and $probe.ExitCode -eq 0) {
                return $file.FullName
            }
        } catch {
            continue
        } finally {
            if ($probe) {
                $probe.Dispose()
            }
        }
    }

    throw "No usable $ToolName tool was found."
}

function Get-VersaDownBuildTools {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$ProjectRoot
    )

    # Sandboxed Windows shells may virtualize GetFolderPath while keeping the
    # real tool installation under USERPROFILE. Prefer the explicit process
    # environment so discovery and the build use the same account paths.
    $userProfile = $env:USERPROFILE
    if ([string]::IsNullOrWhiteSpace($userProfile)) {
        $userProfile = [Environment]::GetFolderPath('UserProfile')
    }
    $codexRuntime = Join-Path $userProfile '.cache\codex-runtimes\codex-primary-runtime\dependencies'
    $projectCargo = Join-Path $ProjectRoot '.tools\cargo\bin\cargo.exe'
    $projectPnpm = Join-Path $ProjectRoot '.tools\node-v24.20.0-win-x64\node_modules\corepack\shims\pnpm.cmd'

    [pscustomobject]@{
        Cargo = Resolve-VersaDownBuildTool -ToolName 'Cargo' -Candidates @(
            (Join-Path $userProfile '.cargo\bin\cargo.exe'),
            $projectCargo
        )
        Pnpm = Resolve-VersaDownBuildTool -ToolName 'pnpm' -Candidates @(
            (Join-Path $codexRuntime 'bin\fallback\pnpm.cmd'),
            $projectPnpm
        )
    }
}
