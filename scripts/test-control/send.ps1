<#
.SYNOPSIS
Sends one request to a YIEN's test control pipe and prints the reply line.

.DESCRIPTION
Usage:
    ./scripts/test-control/send.ps1 -Name <name> [-ServerPid <pid>] '<json request>'

Examples:
    ./scripts/test-control/send.ps1 -Name tc-1f2e '{"op":"state","text":true}'
    ./scripts/test-control/send.ps1 -Name tc-1f2e '{"op":"command","id":"pane.split_right"}'
    ./scripts/test-control/send.ps1 -Name tc-1f2e '{"op":"key","key":"ctrl-c"}'

-Name is what YIEN was started with after --test-control. -ServerPid refuses
a pipe whose server is not that process. Exits 1 when the reply says ok false.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Name,
    [Parameter(Mandatory, Position = 0)][string]$Request,
    [int]$ServerPid = 0
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

$reply = Send-YienRequest -Name $Name -ServerPid $ServerPid -Request $Request
$reply | ConvertTo-Json -Compress -Depth 20
if (-not $reply.ok) { exit 1 }
