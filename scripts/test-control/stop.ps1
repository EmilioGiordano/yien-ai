<#
.SYNOPSIS
Ends a run launch.ps1 started.

.DESCRIPTION
Usage:
    ./scripts/test-control/stop.ps1 -Root <the run's folder>

Asks YIEN to quit through test control (app.quit, accepting the "agents are
working" question if it comes), waits for it to exit, and stops the mock API.
Only processes named in <Root>/run.json, which launch.ps1 started, are ever
stopped; YIEN is forced only when it does not exit by itself.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Root,
    [double]$TimeoutSec = 15
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

$run = Get-Content -LiteralPath (Join-Path $Root 'run.json') -Raw | ConvertFrom-Json

function Get-Started([int]$Id, [string]$Path) {
    $process = Get-Process -Id $Id -ErrorAction SilentlyContinue
    if (-not $process) { return $null }
    # A pid may be reused once its process is gone: only the program the run
    # started counts.
    if ($Path -and $process.Path -and ($process.Path -ne $Path)) { return $null }
    return $process
}

$yien = Get-Started $run.pid $run.exe
if ($yien) {
    try {
        Invoke-YienCommand -Name $run.name -ServerPid $run.pid -Id 'app.quit'
        $asked = Wait-YienState -Name $run.name -ServerPid $run.pid -TimeoutSec 2 -Until {
            param($s) $s.window.overlay -eq 'confirm'
        }
        if ($asked) { $null = Send-YienKey -Name $run.name -ServerPid $run.pid -Key 'enter' }
    }
    catch { Write-Verbose "quit through test control failed: $_" }
    if (-not $yien.WaitForExit([int]($TimeoutSec * 1000))) {
        Write-Warning "yien $($run.pid) did not exit in $TimeoutSec s; stopping it"
        Stop-Process -Id $run.pid -Force
    }
}
if ($run.mock_pid) {
    $mock = Get-Started $run.mock_pid $null
    if ($mock -and $mock.ProcessName -eq 'node') { Stop-Process -Id $run.mock_pid -Force }
}
