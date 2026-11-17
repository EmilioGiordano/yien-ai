<#
.SYNOPSIS
Captures YIEN's window to a PNG with PrintWindow. Sends no input.

.DESCRIPTION
Usage:
    ./scripts/test-control/capture.ps1 -Root <run folder> -Out shot.png
    ./scripts/test-control/capture.ps1 -ProcessId <pid>   -Out shot.png

Prints a one-line summary: printwindow=<0|1> size=<W>x<H> nonblack=<n>
<out>. A capture that came back all black (nonblack small) means PrintWindow
failed or the session is not on the input desktop; the summary says so.

This sends no input and does not need the window focused; it is the safe
half of capturing UI for validation. It still reads another process's
window, so it runs only at the UI validation of a feature, on the YIEN
started for the test. Use -Root <the run folder> for a run launch.ps1
started (its pid is refused when the process there is not yien.exe), or
-ProcessId for any other window you started for the test.

PrintWindow with PW_RENDERFULLCONTENT (flag 2) captures even a window that
is partly covered. Windows only.
#>
[CmdletBinding()]
param(
    [string]$Root,
    [int]$ProcessId,
    [Parameter(Mandatory)][string]$Out
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $Root -and $ProcessId -eq 0) { throw 'pass -Root <the run folder> or -ProcessId <pid>' }

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class CaptureWindow {
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern bool GetUserObjectInformation(IntPtr desktop, int index, System.Text.StringBuilder name, int length, out int needed);
  public struct RECT { public int L, T, R, B; }
}
"@

# The process the launch started, refusing a reused pid.
function Get-Target {
    if ($ProcessId -ne 0) {
        return Get-Process -Id $ProcessId -ErrorAction Stop
    }
    $run = Get-Content -LiteralPath (Join-Path $Root 'run.json') -Raw | ConvertFrom-Json
    $target = Get-Process -Id $run.pid -ErrorAction Stop
    if ($run.exe -and $target.Path -and ($target.Path -ne $run.exe)) {
        throw "pid $($run.pid) is not the $($run.exe) the run started"
    }
    return $target
}

$target = Get-Target
$target.Refresh()
$handle = $target.MainWindowHandle
if ($handle -eq 0) { throw "process $($target.Id) has no window" }

$rect = New-Object CaptureWindow+RECT
[CaptureWindow]::GetWindowRect($handle, [ref]$rect) | Out-Null
$width = $rect.R - $rect.L
$height = $rect.B - $rect.T
if ($width -le 0 -or $height -le 0) { throw "the window reports ${width}x$height" }

$bitmap = New-Object System.Drawing.Bitmap ($width, $height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$hdc = $graphics.GetHdc()
# 2 = PW_RENDERFULLCONTENT: the window draws into the DC itself.
$ok = [CaptureWindow]::PrintWindow($handle, $hdc, 2)
$graphics.ReleaseHdc($hdc)
$graphics.Dispose()

$out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
$bitmap.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)

# A coarse non-black count: an all-black capture means the window is on
# another desktop or PrintWindow failed.
$nonblack = 0
for ($x = 0; $x -lt $width; $x += 37) {
    for ($y = 0; $y -lt $height; $y += 37) {
        $pixel = $bitmap.GetPixel($x, $y)
        if ($pixel.R + $pixel.G + $pixel.B -gt 30) { $nonblack++ }
    }
}
$bitmap.Dispose()

# Name the input desktop, for the all-black diagnosis.
$desktop = [CaptureWindow]::OpenInputDesktop(0, $false, 0x0001)
$desktopName = 'unknown'
if ($desktop -ne [IntPtr]::Zero) {
    $builder = New-Object System.Text.StringBuilder 256
    $needed = 0
    if ([CaptureWindow]::GetUserObjectInformation($desktop, 2, $builder, 512, [ref]$needed)) {
        $desktopName = $builder.ToString()
    }
}

"printwindow=$ok size=${width}x$height nonblack=$nonblack inputDesktop='$desktopName' $out"
