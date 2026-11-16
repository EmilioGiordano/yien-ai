<#
.SYNOPSIS
Checks that an OpenCode pane, from its profile and typed in a shell, reaches
Working and Idle through OSC 7501 (ADR-21, RF-AGE-05).

.DESCRIPTION
Usage:
    ./scripts/test-control/scenario-opencode.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Keep]

Starts the build with launch.ps1 -MockOpenCode (a throwaway HOME under the
slot, a project-level opencode.json in work/ pointing OpenCode at a local
OpenAI-compatible mock, no account or quota), drives both panes through test
control, prints PASS or FAIL per step, stops the run with stop.ps1 unless
-Keep, and exits with the number of failed steps.

    1. profile   a pane started with the opencode profile: Working while the
                 mock streams, Idle when the turn ends (the pane is seen)
    2. typed     `opencode` typed in a shell pane: the head renames the pane
                 to opencode, then Working and Idle through OSC 7501

OpenCode reports its state with OSC 7501 records, which YIEN only trusts for
the versions they were run against (2.2.7 or newer); the prerequisite check
fails loudly otherwise. A slow mock stream (~40 s) keeps Working observable.

Needs: node on PATH, `opencode` on PATH (2.2.7 or newer), PowerShell as
YIEN's shell.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Root,
    [switch]$Keep,
    [double]$StartTimeoutSec = 60,
    [double]$StateTimeoutSec = 90
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

# The prerequisite check runs before anything is launched.
$opencode = Get-Command opencode -ErrorAction SilentlyContinue
$versionText = if ($opencode) { (& $opencode.Source --version 2>&1 | Out-String) } else { '' }
$version = $null
if ($versionText -match '(\d+)\.(\d+)\.(\d+)') { $version = [version]"$($Matches[1]).$($Matches[2]).$($Matches[3])" }
if (-not $opencode -or -not $version -or $version -lt [version]'2.2.7') {
    $why = if (-not $opencode) { '`opencode` is not on PATH' }
        elseif (-not $version) { "could not read the version from '$versionText'" }
        else { "OpenCode $version does not report over OSC 7501 (2.2.7 or newer does)" }
    Write-Host "FAIL precheck  opencode  $why"
    Write-Host ("0 passed, 1 failed")
    exit 1
}
Write-Host "opencode $version found at $($opencode.Source)"

$launchArgs = @{ Exe = $Exe; MockOpenCode = $true }
if ($Root) { $launchArgs.Root = $Root }
$run = & (Join-Path $PSScriptRoot 'launch.ps1') @launchArgs
$name = $run.name
$serverPid = $run.pid
Write-Host "run $name, yien pid $serverPid, folder $($run.root)"

$results = New-Object System.Collections.Generic.List[object]

function Step {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$StepPane,
        [Parameter(Mandatory)][string]$StepName,
        [Parameter(Mandatory)][scriptblock]$Check
    )
    $started = [DateTime]::UtcNow
    try {
        $detail = & $Check
        $ok = $true
    }
    catch {
        $detail = "$_"
        $ok = $false
    }
    $result = [pscustomobject]@{
        pane   = $StepPane
        step   = $StepName
        result = if ($ok) { 'PASS' } else { 'FAIL' }
        secs   = [math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
        detail = "$detail"
    }
    $results.Add($result)
    Write-Host ("{0,-4} {1,-8} {2,-8} {3}" -f $result.result, $StepPane, $StepName, $result.detail)
    return $ok
}

function State {
    [CmdletBinding()]
    param()
    return Get-YienState -Name $name -ServerPid $serverPid
}

function Wait-Pane {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][int]$Pane,
        [Parameter(Mandatory)][scriptblock]$Until,
        [Parameter(Mandatory)][string]$What,
        [double]$TimeoutSec,
        [switch]$Text
    )
    $timeout = if ($TimeoutSec) { $TimeoutSec } else { 20 }
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec $timeout -Text:$Text -Until {
        param($s)
        $p = Get-YienPane $s $Pane
        $null -ne $p -and (& $Until $p $s)
    }
    if (-not $state) {
        $p = Get-YienPane (State) $Pane
        $now = if ($p) { "name=$($p.name) state=$($p.state) running=$($p.running)" } else { 'gone' }
        throw "pane $Pane never got $What within $timeout s; now $now"
    }
    return Get-YienPane $state $Pane
}

function Wait-NewPane {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][int[]]$Before,
        [Parameter(Mandatory)][scriptblock]$Until,
        [Parameter(Mandatory)][string]$What
    )
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec $StartTimeoutSec -Until {
        param($s)
        @($s.panes | Where-Object { $Before -notcontains $_.id -and (& $Until $_ $s) }).Count -gt 0
    }
    if (-not $state) { throw "no new pane with $What within $StartTimeoutSec s" }
    return $state.panes | Where-Object { $Before -notcontains $_.id -and (& $Until $_ $state) } | Select-Object -First 1
}

function Focus-Pane {
    [CmdletBinding()]
    param([Parameter(Mandatory)][int]$Pane)
    Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'pane.focus' -Arguments @{ pane = $Pane }
    $null = Wait-Pane $Pane -What 'the keyboard' -Until { param($p) $p.keyboard } -TimeoutSec 10
}

function Key {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][int]$Pane,
        [Parameter(Mandatory)][string]$Key
    )
    $null = Send-YienKey -Name $name -ServerPid $serverPid -Pane $Pane -Key $Key
}

function Run-Turn {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Label,
        [Parameter(Mandatory)][int]$Pane
    )
    $ok = Step $Label 'working' {
        Focus-Pane $Pane
        Send-YienText -Name $name -ServerPid $serverPid -Pane $Pane -Text 'please answer slow'
        Key $Pane 'enter'
        $p = Wait-Pane $Pane -What 'Working' -Until { param($p) $p.state -eq 'Working' }
        "state $($p.state)"
    }
    if (-not $ok) { return }
    $null = Step $Label 'idle' {
        $p = Wait-Pane $Pane -What 'Idle' -TimeoutSec $StateTimeoutSec -Until { param($p) $p.state -eq 'Idle' }
        if ((State).waiting -ne 0) { throw "the counter counts $((State).waiting) after the turn" }
        "state $($p.state), counter 0"
    }
}

try {
    # An OpenCode pane started from its profile.
    $before = @((State).panes | ForEach-Object { $_.id })
    $profile = $null
    $ok = Step 'profile' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new_with' -Arguments @{ profile = 'opencode' }
        $script:profile = Wait-NewPane $before -What 'the opencode profile running' -Until {
            param($p) $p.profile_id -eq 'opencode' -and $p.running
        }
        # Let the TUI draw; the PTY buffer holds early input, this just keeps it out.
        Start-Sleep -Seconds 3
        "pane $($script:profile.id)"
    }
    if ($ok) { Run-Turn 'profile' $profile.id }

    # `opencode` typed in a shell pane.
    $before = @((State).panes | ForEach-Object { $_.id })
    $shell = $null
    $ok = Step 'typed' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new'
        $script:shell = Wait-NewPane $before -What 'a running shell' -Until {
            param($p) $p.profile_id -eq 'shell' -and $p.running
        }
        $id = $script:shell.id
        Focus-Pane $id
        Send-YienText -Name $name -ServerPid $serverPid -Pane $id -Text 'opencode'
        Key $id 'enter'
        $p = Wait-Pane $id -What "the pane renamed to opencode" -TimeoutSec 15 -Until { param($p) $p.name -eq 'opencode' }
        # Let the TUI draw; the PTY buffer holds early input, this just keeps it out.
        Start-Sleep -Seconds 3
        "pane $($p.id), renamed to $($p.name)"
    }
    if ($ok) { Run-Turn 'typed' $shell.id }
}
finally {
    if ($Keep) {
        Write-Host "kept running: stop it with stop.ps1 -Root $($run.root)"
    }
    else {
        & (Join-Path $PSScriptRoot 'stop.ps1') -Root $run.root
    }
}

$results | Format-Table -AutoSize | Out-String | Write-Host
$failed = @($results | Where-Object { $_.result -eq 'FAIL' }).Count
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failed), $failed)
exit $failed
