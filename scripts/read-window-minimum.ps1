param([Parameter(Mandatory)][int]$AppProcessId)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class ResizeMinimumProbe {
    public delegate bool Callback(IntPtr hwnd, IntPtr data);
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int x, y; }
    [StructLayout(LayoutKind.Sequential)] public struct MinMaxInfo { public Point reserved, maxSize, maxPosition, minTrackSize, maxTrackSize; }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int left, top, right, bottom; }
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] public static extern bool EnumWindows(Callback callback, IntPtr data);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int size);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, IntPtr wp, ref MinMaxInfo info, uint flags, uint timeout, out IntPtr result);
}
'@
$previousDpiContext = [ResizeMinimumProbe]::SetThreadDpiAwarenessContext([IntPtr](-4))
$script:minimumResult = $null
try {
    [void][ResizeMinimumProbe]::EnumWindows({
        param($windowHandle, $unused)
        $owner = [uint32]0
        [void][ResizeMinimumProbe]::GetWindowThreadProcessId($windowHandle, [ref]$owner)
        if ($owner -ne $AppProcessId) { return $true }
        $title = [System.Text.StringBuilder]::new(256)
        [void][ResizeMinimumProbe]::GetWindowText($windowHandle, $title, 256)
        if ($title.ToString() -ne 'AllDownloader') { return $true }
        $info = New-Object ResizeMinimumProbe+MinMaxInfo
        $resultValue = [IntPtr]::Zero
        if ([ResizeMinimumProbe]::SendMessageTimeout($windowHandle, 0x24, [IntPtr]::Zero, [ref]$info, 2, 2000, [ref]$resultValue) -eq [IntPtr]::Zero) { return $true }
        $client = New-Object ResizeMinimumProbe+Rect
        $outer = New-Object ResizeMinimumProbe+Rect
        [void][ResizeMinimumProbe]::GetClientRect($windowHandle, [ref]$client)
        [void][ResizeMinimumProbe]::GetWindowRect($windowHandle, [ref]$outer)
        $scale = [ResizeMinimumProbe]::GetDpiForWindow($windowHandle) / 96.0
        $frameWidth = $outer.right - $outer.left - $client.right
        $frameHeight = $outer.bottom - $outer.top - $client.bottom
        $script:minimumResult = [pscustomobject]@{
            dpiScale = $scale
            minTrackWidth = $info.minTrackSize.x
            minTrackHeight = $info.minTrackSize.y
            width = ($info.minTrackSize.x - $frameWidth) / $scale
            height = ($info.minTrackSize.y - $frameHeight) / $scale
        }
        return $false
    }, [IntPtr]::Zero)
} finally {
    [void][ResizeMinimumProbe]::SetThreadDpiAwarenessContext($previousDpiContext)
}
if (-not $script:minimumResult) { throw 'Could not inspect the test main window minimum track size.' }
$script:minimumResult | ConvertTo-Json -Compress
