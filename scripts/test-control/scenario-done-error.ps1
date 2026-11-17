<#
.SYNOPSIS
Checks that a finished turn in an unseen pane shows Done in the tab and the
pane, and that the mock API answering with an error shows Error instead,
never Needs you (RN-01, RN-02).

.DESCRIPTION
Usage:
    ./scripts/test-control/scenario-done-error.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Keep]

Starts the build with launch.ps1 -MockClaude (a throwaway profile under the
slot, Claude Code pointed at a local mock API), drives both cases through
test control, prints PASS or FAIL per step, stops the run with stop.ps1
unless -Keep, and exits with the number of failed steps.

The pane is hidden (the shell tab is selected) before each turn ends, as
RN-07 wants the termination kept only for panes that are not seen; without
it both would end as Idle.

    1. done    a "slow" turn (~40 s) that lands while unseen: Done in pane
               and tab, no red mark, the counter does not move
    2. error   a "mockerror" turn (the mock answers an API error after 8 s):
               Error in pane and tab, never Needs you
    3. seen    selecting the tab again clears both to Idle (RF-ATN-08)

Needs: node on PATH, `claude` on PATH, PowerShell as YIEN's shell.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Root,
    [switch]$Keep,
    [double]$StartTimeoutSec = 60,
    # The slow turn takes ~40 s, so the unseen waits allow for it.
    [double]$StateTimeoutSec = 90
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
    Write-Host ("{0,-4} {1,-7} {2,-8} {3}" -f $result.result, $StepPane, $StepName, $result.detail)
    return $ok
}

function State {
    [CmdletBinding()]
    param([switch]$Text)
    return Get-YienState -Name $name -ServerPid $serverPid -Text:$Text
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
        $now = if ($p) { "name=$($p.name) state=$($p.state) running=$($p.running) exited=$($p.exited)" } else { 'gone' }
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

function Wait-Prompt {
    [CmdletBinding()]
    param([Parameter(Mandatory)][int]$Pane)
    # Claude's input box is drawn once its prompt is ready for typing.
    $null = Wait-Pane $Pane -Text -What "Claude's prompt" -TimeoutSec 30 -Until {
        param($p)
        (Get-YienPaneText $p) -match '(?m)(\? for shortcuts|^\s*[>\u276F](\s|$)|\u2502\s*>)'
    }
    Start-Sleep -Milliseconds 1500
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

# The launch tab, the one that is not the Claude tab.
function Shell-Tab-Id {
    [CmdletBinding()]
    param([Parameter(Mandatory)][int]$ClaudeTab)
    $s = State
    $other = $s.tabs | Where-Object { $_.id -ne $ClaudeTab } | Select-Object -First 1
    if (-not $other) { throw 'no other tab to select' }
    return $other.id
}

function Select-Tab {
    [CmdletBinding()]
    param([Parameter(Mandatory)][int]$Tab)
    Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.select' -Arguments @{ tab = $Tab }
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec 10 -Until {
        param($s) $s.selected_tab -eq $Tab
    }
    if (-not $state) { throw "tab $Tab was not selected" }
}

function Check-Ended {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][int]$Pane,
        [Parameter(Mandatory)][string]$Want,
        [Parameter(Mandatory)][string]$Mark
    )
    $s = State
    $p = Get-YienPane $s $Pane
    $tab = $s.tabs | Where-Object { $_.id -eq $p.tab }
    if ($p.state -ne $Want) { throw "the pane's state is '$($p.state)', not $Want" }
    if ($tab.state -ne $Want) { throw "the tab's state is '$($tab.state)', not $Want" }
    if ($tab.needs_you) { throw 'the tab carries the red mark' }
    if ($tab.mark -ne $Mark) { throw "the tab's mark is '$($tab.mark)', not $Mark" }
    if ($s.waiting -ne 0) { throw "the counter counts $($s.waiting), not zero" }
    "pane and tab $Want, tab mark $($tab.mark), counter $($s.waiting)"
}

$claude = $null
$claudeTab = $null
try {
    $before = @((State).panes | ForEach-Object { $_.id })
    $ok = Step 'claude' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new_claude'
        $script:claude = Wait-NewPane $before -What 'the claude profile running' -Until {
            param($p) $p.profile_id -eq 'claude' -and $p.running
        }
        Wait-Prompt $script:claude.id
        $script:claudeTab = (Get-YienPane (State) $script:claude.id).tab
        "pane $($script:claude.id), tab $claudeTab"
    }
    if ($ok) {
        $shellTab = Shell-Tab-Id $claudeTab

        $ok = Step 'done' 'turn' {
            Focus-Pane $claude.id
            Send-YienText -Name $name -ServerPid $serverPid -Pane $claude.id -Text 'please answer slow'
            Key $claude.id 'enter'
            $null = Wait-Pane $claude.id -What 'Working' -TimeoutSec 20 -Until { param($p) $p.state -eq 'Working' }
            # Hide the pane before the turn ends; RN-07 keeps Done only for unseen panes.
            Select-Tab $shellTab
            $p = Get-YienPane (State) $claude.id
            if ($p.visible) { throw 'the pane is still visible after another tab was selected' }
            'Working, then hidden'
        }
        if ($ok) {
            $null = Step 'done' 'lands-done' {
                $null = Wait-Pane $claude.id -What 'Done' -Until { param($p) $p.state -eq 'Ready to review' }
                Check-Ended $claude.id 'Ready to review' 'done'
            }
        }

        # Select the Claude tab to clear Done to Idle before the error case.
        $ok = Step 'error' 'turn' {
            Select-Tab $claudeTab
            Focus-Pane $claude.id
            Send-YienText -Name $name -ServerPid $serverPid -Pane $claude.id -Text 'please answer with a mockerror'
            Key $claude.id 'enter'
            $null = Wait-Pane $claude.id -What 'Working' -TimeoutSec 20 -Until { param($p) $p.state -eq 'Working' }
            Select-Tab $shellTab
            'Working, then hidden'
        }
        if ($ok) {
            $null = Step 'error' 'lands-error' {
                $null = Wait-Pane $claude.id -What 'Error' -Until { param($p) $p.state -eq 'Error' }
                $detail = Check-Ended $claude.id 'Error' 'error'
                "$detail, never Needs you"
            }
        }

        $null = Step 'seen' 'clears' {
            Select-Tab $claudeTab
            $p = Wait-Pane $claude.id -What 'Idle' -TimeoutSec 20 -Until { param($p) $p.state -eq 'Idle' }
            if ((State).waiting -ne 0) { throw "the counter counts $((State).waiting) after seeing the pane" }
            "pane $($p.state), counter 0"
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
