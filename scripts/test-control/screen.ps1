<#
.SYNOPSIS
Finds YIEN's window, moves it, focuses it and sends real keyboard or mouse
input. The real-input fallback for UI validation.

.DESCRIPTION
Usage:
    ./scripts/test-control/screen.ps1 -Action find              -Root <run folder>
    ./scripts/test-control/screen.ps1 -Action find              -ProcessId <pid>
    ./scripts/test-control/screen.ps1 -Action move              -Root <run folder> [-X 40] [-Y 40] [-Width 1400] [-Height 860]
    ./scripts/test-control/screen.ps1 -Action focus             -Root <run folder>
    ./scripts/test-control/screen.ps1 -Action keys              -Root <run folder> -Text '^c'
    ./scripts/test-control/screen.ps1 -Action click             -Root <run folder> -ClickX 100 -ClickY 40
    ./scripts/test-control/screen.ps1 -Action stats             -Root <run folder>

Windows only.

    find    print the window's pid, handle, rectangle and title
    move    move and resize the window (default 40,40 at 1400x860)
    focus   bring the window to the front
    keys    send keys to the focused window (System.Windows.Forms.SendKeys
            notation: ^c is Ctrl+C, {ENTER} is Enter, {ESC} Escape, {TAB}
            Tab, {UP}/{DOWN}/{LEFT}/{RIGHT} arrows)
    click   click at a point relative to the window's top-left corner
    stats   print responding, CPU seconds and working set of the process

THIS SENDS REAL INPUT TO THE DESKTOP: keys go to whatever window is in
front, clicks hit real screen coordinates. It may only run at the UI
validation of a feature (AGENTS.md "Validating in the UI"), and only after the developer has said yes for that run. For everything the test control pipe can
drive, use YienTestControl.psm1 or the scenario scripts instead; they never
touch the desktop.

The window is found from the pid in <Root>/run.json (written by launch.ps1),
or from -ProcessId directly. With -Root, the pid is refused when the process
at it is not the yien.exe the run started. Never point this at a YIEN you
did not start for the test.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Action,
    [string]$Root,
    [int]$ProcessId,
    [int]$X = 40,
    [int]$Y = 40,
    [int]$Width = 1400,
    [int]$Height = 860,
    [string]$Text = '',
    [int]$ClickX = 0,
    [int]$ClickY = 0
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $Root -and $ProcessId -eq 0) { throw 'pass -Root <the run folder> or -ProcessId <pid>' }

Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class ScreenFallback {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hgt, bool repaint);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
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

function Get-Handle($Target) {
    $Target.Refresh()
    $handle = $Target.MainWindowHandle
    if ($handle -eq 0) { throw "process $($Target.Id) has no window" }
    return $handle
}

function Focus($Target) {
    $handle = Get-Handle $Target
    [ScreenFallback]::ShowWindow($handle, 9) | Out-Null
    [ScreenFallback]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 300
    return $handle
}

$target = Get-Target
switch ($Action) {
    'find' {
        $handle = Get-Handle $target
        $rect = New-Object ScreenFallback+RECT
        [ScreenFallback]::GetWindowRect($handle, [ref]$rect) | Out-Null
        $title = $target.MainWindowTitle
        "pid=$($target.Id) handle=$handle rect=$($rect.L),$($rect.T)-$($rect.R),$($rect.B) title='$title'"
    }
    'move' {
        $handle = Get-Handle $target
        [ScreenFallback]::MoveWindow($handle, $X, $Y, $Width, $Height, $true) | Out-Null
        "moved $($target.Id) to $X,$Y ${Width}x$Height"
    }
    'focus' {
        $handle = Focus $target
        "focused $($target.Id) (handle $handle)"
    }
    'keys' {
        if ([string]::IsNullOrEmpty($Text)) { throw 'pass -Text with the keys to send' }
        $null = Focus $target
        [System.Windows.Forms.SendKeys]::SendWait($Text)
        "sent '$Text'"
    }
    'click' {
        $handle = Focus $target
        $rect = New-Object ScreenFallback+RECT
        [ScreenFallback]::GetWindowRect($handle, [ref]$rect) | Out-Null
        # mouse_event flags: 2 is left down, 4 is left up.
        [ScreenFallback]::SetCursorPos($rect.L + $ClickX, $rect.T + $ClickY) | Out-Null
        Start-Sleep -Milliseconds 100
        [ScreenFallback]::mouse_event(2, 0, 0, [uint]0, [UIntPtr]::Zero)
        [ScreenFallback]::mouse_event(4, 0, 0, [uint]0, [UIntPtr]::Zero)
        "clicked $ClickX,$ClickY"
    }
    'stats' {
        $target.Refresh()
        'responding={0} cpu={1:N1}s ws={2:N0}MB' -f $target.Responding, $target.CPU, ($target.WorkingSet64 / 1MB)
    }
    default { throw "no action '$Action': find, move, focus, keys, click, stats" }
}
