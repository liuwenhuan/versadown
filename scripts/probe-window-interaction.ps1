param(
    [Parameter(Mandatory)][int]$AppProcessId,
    [Parameter(Mandatory)][ValidateSet('enter', 'exit')][string]$Phase
)
$ErrorActionPreference = 'Stop'
# Test-only lifecycle messages to the isolated process launched by the Node
# harness. This does NOT simulate mouse input or measure interactive frame rate.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class WindowInteractionProbe {
    public delegate bool Callback(IntPtr hwnd, IntPtr data);
    [DllImport("user32.dll")] public static extern bool EnumWindows(Callback callback, IntPtr data);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam, uint flags, uint timeout, out IntPtr result);
}
'@
$script:probeWindow = [IntPtr]::Zero
[void][WindowInteractionProbe]::EnumWindows({
    param($windowHandle, $unused)
    $owner = [uint32]0
    [void][WindowInteractionProbe]::GetWindowThreadProcessId($windowHandle, [ref]$owner)
    if ($owner -ne $AppProcessId) { return $true }
    $title = [System.Text.StringBuilder]::new(256)
    [void][WindowInteractionProbe]::GetWindowText($windowHandle, $title, 256)
    if ($title.ToString() -eq 'VersaDown') { $script:probeWindow = $windowHandle; return $false }
    return $true
}, [IntPtr]::Zero)
if ($script:probeWindow -eq [IntPtr]::Zero) { throw 'Isolated main window not found' }
$message = if ($Phase -eq 'enter') { 0x0231 } else { 0x0232 }
$resultValue = [IntPtr]::Zero
if ([WindowInteractionProbe]::SendMessageTimeout($script:probeWindow, $message, [IntPtr]::Zero, [IntPtr]::Zero, 2, 3000, [ref]$resultValue) -eq [IntPtr]::Zero) {
    throw 'Native lifecycle message timed out'
}
Write-Output $Phase
