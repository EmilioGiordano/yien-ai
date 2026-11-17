<#
.SYNOPSIS
A client for YIEN's test control pipe (ADR-21), for Windows PowerShell 5.1
and PowerShell 7.

.DESCRIPTION
YIEN started with `--test-control <name>` listens on
\\.\pipe\yien-test-control-<name>. Each request is one JSON line and gets one
JSON reply line. This module opens one connection per request, so a waiting
script never holds the pipe, which serves one client at a time.

    Import-Module ./scripts/test-control/YienTestControl.psm1
    $s = Get-YienState -Name tc-1234 -Text
    Invoke-YienCommand -Name tc-1234 -Id tab.new_claude
    Send-YienText -Name tc-1234 -Pane 7 -Text 'hello'
    Send-YienKey -Name tc-1234 -Pane 7 -Key enter
    Wait-YienState -Name tc-1234 -TimeoutSec 20 -Until { param($s) $s.waiting -gt 0 }

Requests (see the report of feat/test-control for the full protocol):
    {"op":"state","text":true}
    {"op":"command","id":"pane.split_right"}
    {"op":"command","id":"tab.new_with","args":{"profile":"claude"}}
    {"op":"command","id":"pane.split_with","args":{"profile":"shell","direction":"down"}}
    {"op":"command","id":"pane.focus","args":{"pane":7}}
    {"op":"text","pane":7,"text":"claude"}
    {"op":"key","pane":7,"key":"ctrl-c"}
    {"op":"resize","width":1200,"height":800}
    {"op":"window.close"}

The state's `window.window_visible` says whether the window is shown. With
`window.close_to_tray` on (the default) and the notification-area icon up,
`window.close` hides the window and YIEN keeps running; with the setting off,
or with the icon missing, it quits the app, asking first while agents work.

Pass -ServerPid to check that the pipe's server is the YIEN the script
started, so another process that took the name first is refused.
#>

Set-StrictMode -Version Latest

$script:Utf8 = New-Object System.Text.UTF8Encoding($false)

function Assert-YienName([string]$Name) {
    if ($Name -cnotmatch '^[A-Za-z0-9_-]{1,64}$') {
        throw "not a test control name: '$Name'"
    }
}

function Get-YienServerPid($Pipe) {
    if (-not ('YienTestControl.Native' -as [type])) {
        Add-Type -Namespace YienTestControl -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool GetNamedPipeServerProcessId(
    Microsoft.Win32.SafeHandles.SafePipeHandle pipe, out uint serverProcessId);
'@
    }
    $serverPid = [uint32]0
    if (-not [YienTestControl.Native]::GetNamedPipeServerProcessId($Pipe.SafePipeHandle, [ref]$serverPid)) {
        throw 'could not read the pipe server process id'
    }
    return [int]$serverPid
}

<#
.SYNOPSIS
Sends one request and returns the reply as an object. -Request is a JSON
string or anything ConvertTo-Json takes.
#>
function Send-YienRequest {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)]$Request,
        [int]$ServerPid = 0,
        [int]$ConnectTimeoutMs = 5000
    )
    Assert-YienName $Name
    $line = if ($Request -is [string]) { $Request } else { $Request | ConvertTo-Json -Compress -Depth 10 }
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(
        '.', "yien-test-control-$Name",
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::None,
        [System.Security.Principal.TokenImpersonationLevel]::Anonymous)
    try {
        $pipe.Connect($ConnectTimeoutMs)
        if ($ServerPid -ne 0) {
            $actual = Get-YienServerPid $pipe
            if ($actual -ne $ServerPid) {
                throw "the pipe's server is process $actual, not $ServerPid"
            }
        }
        $writer = New-Object System.IO.StreamWriter($pipe, $script:Utf8)
        $writer.NewLine = "`n"
        $writer.AutoFlush = $true
        $reader = New-Object System.IO.StreamReader($pipe, $script:Utf8)
        $writer.WriteLine($line)
        $reply = $reader.ReadLine()
        if ($null -eq $reply) { throw 'the pipe closed without a reply' }
        return $reply | ConvertFrom-Json
    }
    finally {
        $pipe.Dispose()
    }
}

function Assert-YienOk($Reply, [string]$What) {
    if (-not $Reply.ok) { throw "$What failed: $($Reply.error)" }
    return $Reply
}

<# Returns the window's state; -Text adds each pane's visible rows. #>
function Get-YienState {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Name, [switch]$Text, [int]$ServerPid = 0)
    $reply = Send-YienRequest -Name $Name -ServerPid $ServerPid -Request @{ op = 'state'; text = [bool]$Text }
    return (Assert-YienOk $reply 'state').state
}

<# Runs a catalog command, or a palette row with -Arguments. #>
function Invoke-YienCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Id,
        [hashtable]$Arguments = @{},
        [int]$ServerPid = 0
    )
    $request = @{ op = 'command'; id = $Id; args = $Arguments }
    $null = Assert-YienOk (Send-YienRequest -Name $Name -ServerPid $ServerPid -Request $request) "command $Id"
}

<# Types printable text into the pane that has the keyboard. #>
function Send-YienText {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Text,
        [int]$Pane = 0,
        [int]$ServerPid = 0
    )
    $request = @{ op = 'text'; text = $Text }
    if ($Pane -ne 0) { $request.pane = $Pane }
    $null = Assert-YienOk (Send-YienRequest -Name $Name -ServerPid $ServerPid -Request $request) 'text'
}

<# Presses a key in gpui notation (ctrl-c, enter, escape, up); returns whether anything took it. #>
function Send-YienKey {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Key,
        [int]$Pane = 0,
        [int]$ServerPid = 0
    )
    $request = @{ op = 'key'; key = $Key }
    if ($Pane -ne 0) { $request.pane = $Pane }
    $reply = Assert-YienOk (Send-YienRequest -Name $Name -ServerPid $ServerPid -Request $request) "key $Key"
    return [bool]$reply.handled
}

<# Resizes the window's content area. #>
function Set-YienWindowSize {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][double]$Width,
        [Parameter(Mandatory)][double]$Height,
        [int]$ServerPid = 0
    )
    $request = @{ op = 'resize'; width = $Width; height = $Height }
    $null = Assert-YienOk (Send-YienRequest -Name $Name -ServerPid $ServerPid -Request $request) 'resize'
}

<# Closes the window the way the close button does: hides it to the
notification area with `window.close_to_tray` on (the default) and the icon
up, quits otherwise, asking first while agents work. The state's
`window.window_visible` says which happened. #>
function Close-YienWindow {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Name, [int]$ServerPid = 0)
    $request = @{ op = 'window.close' }
    $null = Assert-YienOk (Send-YienRequest -Name $Name -ServerPid $ServerPid -Request $request) 'window.close'
}

<#
.SYNOPSIS
Polls the state until -Until returns true for it; returns that state, or
$null after -TimeoutSec.
#>
function Wait-YienState {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Until,
        [double]$TimeoutSec = 10,
        [int]$IntervalMs = 250,
        [switch]$Text,
        [int]$ServerPid = 0
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
    do {
        $state = Get-YienState -Name $Name -Text:$Text -ServerPid $ServerPid
        if (& $Until $state) { return $state }
        Start-Sleep -Milliseconds $IntervalMs
    } while ([DateTime]::UtcNow -lt $deadline)
    return $null
}

<# The pane with this id in a state, or $null. #>
function Get-YienPane($State, [int]$Pane) {
    return $State.panes | Where-Object { $_.id -eq $Pane } | Select-Object -First 1
}

<# A pane's visible rows as one string, empty when the state has none (asked without -Text, or no frame yet). #>
function Get-YienPaneText($Pane) {
    if ($null -eq $Pane) { return '' }
    $rows = $Pane.PSObject.Properties['rows']
    if ($null -eq $rows -or $null -eq $rows.Value) { return '' }
    return ($rows.Value -join "`n")
}

Export-ModuleMember -Function Send-YienRequest, Get-YienState, Invoke-YienCommand, Send-YienText,
    Send-YienKey, Set-YienWindowSize, Close-YienWindow, Wait-YienState, Get-YienPane, Get-YienPaneText
