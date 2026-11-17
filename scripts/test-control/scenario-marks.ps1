<#
.SYNOPSIS
Checks the agent/shell badge and the state of each tab row and pane head,
as test control reports them, for a Claude profile pane, a PowerShell pane
and a cmd pane (RF-PAN-12). The needs-you mark is scenario-needs-you.ps1's
subject.

.DESCRIPTION
Usage:
    ./scripts/test-control/scenario-marks.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Keep]

Starts the build with launch.ps1 -MockClaude (a throwaway profile under the
slot, Claude Code pointed at a local mock API), opens the three panes through
test control, checks each row and head, stops the run with stop.ps1 unless
-Keep, and exits with the number of failed steps.

    1. claude      a Claude profile pane: badge claude in pane and tab,
                   tag claude, state Idle, the tab's mark idle (an agent's),
                   never a red mark
    2. powershell  a pane started with shell:powershell: badge powershell,
                   state Idle, the tab's mark shell (no mark drawn)
    3. cmd         a pane started with shell:cmd: badge cmd, state Idle,
                   the tab's mark shell (no mark drawn)

Requires: node on PATH, `claude` on PATH, PowerShell as YIEN's shell.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Root,
    [switch]$Keep,
    [double]$StartTimeoutSec = 60,
    [double]$StateTimeoutSec = 20
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

$launchArgs = @{ Exe = $Exe; MockClaude = $true }
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
    Write-Host ("{0,-4} {1,-11} {2,-8} {3}" -f $result.result, $StepPane, $StepName, $result.detail)
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
    $timeout = if ($TimeoutSec) { $TimeoutSec } else { $StateTimeoutSec }
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec $timeout -Text:$Text -Until {
        param($s)
        $p = Get-YienPane $s $Pane
        $null -ne $p -and (& $Until $p $s)
    }
    if (-not $state) {
        $p = Get-YienPane (State) $Pane
        $now = if ($p) { "name=$($p.name) badge=$($p.badge) state=$($p.state) running=$($p.running)" } else { 'gone' }
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

# Checks one fresh pane and its tab against the badge and state they must show.
function Check-Badge {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Label,
        [Parameter(Mandatory)][int]$Pane,
        [Parameter(Mandatory)][string]$WantBadge,
        [Parameter(Mandatory)][string]$WantTag,
        [Parameter(Mandatory)][string]$WantMark
    )
    $p = Wait-Pane $Pane -What "a running $Label pane" -Until { param($p) $p.running }
    if ($p.badge -ne $WantBadge) { throw "the pane's badge is '$($p.badge)', not '$WantBadge'" }
    if ($p.tag -ne $WantTag) { throw "the pane's tag is '$($p.tag)', not '$WantTag'" }
    if ($p.state -ne 'Idle') { throw "the pane's state is '$($p.state)', not Idle" }
    $s = State
    $tab = $s.tabs | Where-Object { $_.id -eq $p.tab }
    if (-not $tab) { throw "no tab for pane $Pane" }
    if ($tab.badge -ne $WantBadge) { throw "the tab's badge is '$($tab.badge)', not '$WantBadge'" }
    if ($tab.mark -ne $WantMark) { throw "the tab's mark is '$($tab.mark)', not '$WantMark'" }
    if ($tab.state -ne 'Idle') { throw "the tab's state is '$($tab.state)', not Idle" }
    if ($tab.needs_you) { throw 'the tab carries the red mark without anything needing you' }
    "badge $($p.badge)/$($tab.badge), tag $($p.tag), pane and tab Idle, tab mark $($tab.mark)"
}

try {
    # A Claude Code pane from its profile.
    $before = @((State).panes | ForEach-Object { $_.id })
    $claude = $null
    $ok = Step 'claude' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new_claude'
        $script:claude = Wait-NewPane $before -What 'the claude profile running' -Until {
            param($p) $p.profile_id -eq 'claude' -and $p.running
        }
        "pane $($script:claude.id)"
    }
    if ($ok) {
        $null = Step 'claude' 'badge' {
            Check-Badge 'claude' $claude.id 'claude' 'claude' 'idle'
        }
    }

    # A Windows PowerShell pane started by its kind profile.
    $before = @((State).panes | ForEach-Object { $_.id })
    $powershell = $null
    $ok = Step 'powershell' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new_with' -Arguments @{ profile = 'shell:powershell' }
        $script:powershell = Wait-NewPane $before -What 'a running powershell pane' -Until {
            param($p) $p.profile_id -eq 'shell:powershell' -and $p.running
        }
        "pane $($script:powershell.id)"
    }
    if ($ok) {
        $null = Step 'powershell' 'badge' {
            Check-Badge 'powershell' $powershell.id 'powershell' 'powershell' 'shell'
        }
    }

    # A cmd pane started by its kind profile.
    $before = @((State).panes | ForEach-Object { $_.id })
    $cmd = $null
    $ok = Step 'cmd' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new_with' -Arguments @{ profile = 'shell:cmd' }
        $script:cmd = Wait-NewPane $before -What 'a running cmd pane' -Until {
            param($p) $p.profile_id -eq 'shell:cmd' -and $p.running
        }
        "pane $($script:cmd.id)"
    }
    if ($ok) {
        $null = Step 'cmd' 'badge' {
            Check-Badge 'cmd' $cmd.id 'cmd' 'cmd' 'shell'
        }
    }
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
