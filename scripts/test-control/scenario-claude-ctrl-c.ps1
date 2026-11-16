<#
.SYNOPSIS
Checks that Ctrl+C on a working Claude Code turn brings the pane back to
Idle, and that Ctrl+C twice ends Claude, in a Claude profile pane and in a
PowerShell pane where `claude` was typed.

.DESCRIPTION
Usage:
    ./scripts/test-control/scenario-claude-ctrl-c.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Keep]

Starts the build with launch.ps1 -MockClaude (a throwaway profile under the
slot, Claude Code pointed at a local mock API), runs the steps below in each
pane through test control, prints PASS or FAIL per step, stops the run with
stop.ps1 unless -Keep, and exits with the number of failed steps.

For each pane, Claude profile tab first, then a shell tab with `claude` typed:
    1. open      the pane runs Claude and its prompt is drawn
    2. working   a prompt with "slow" (the mock streams ~40 s) shows Working
    3. idle      Ctrl+C shows Idle again, the turn interrupted
    4. exit      Ctrl+C twice ends Claude: the profile pane's program exits
                 with no red and no error; the shell pane is back at its shell

Needs: node on PATH, `claude` on PATH, PowerShell as YIEN's shell.
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

function Step([string]$Pane, [string]$Step, [scriptblock]$Check) {
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
        pane   = $Pane
        step   = $Step
        result = if ($ok) { 'PASS' } else { 'FAIL' }
        secs   = [math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
        detail = "$detail"
    }
    $results.Add($result)
    Write-Host ("{0,-4} {1,-7} {2,-8} {3}" -f $result.result, $Pane, $Step, $result.detail)
    return $ok
}

function State([switch]$Text) {
    return Get-YienState -Name $name -ServerPid $serverPid -Text:$Text
}

function Wait-Pane([int]$Pane, [scriptblock]$Until, [string]$What, [switch]$Text) {
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec $StateTimeoutSec -Text:$Text -Until {
        param($s)
        $p = Get-YienPane $s $Pane
        $null -ne $p -and (& $Until $p $s)
    }
    if (-not $state) {
        $p = Get-YienPane (State) $Pane
        $now = if ($p) { "name=$($p.name) state=$($p.state) running=$($p.running) exited=$($p.exited)" } else { 'gone' }
        throw "pane $Pane never got $What within $StateTimeoutSec s; now $now"
    }
    return Get-YienPane $state $Pane
}

function Wait-NewPane([int[]]$Before, [scriptblock]$Until, [string]$What) {
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec $StartTimeoutSec -Until {
        param($s)
        @($s.panes | Where-Object { $Before -notcontains $_.id -and (& $Until $_ $s) }).Count -gt 0
    }
    if (-not $state) { throw "no new pane with $What within $StartTimeoutSec s" }
    return $state.panes | Where-Object { $Before -notcontains $_.id -and (& $Until $_ $state) } | Select-Object -First 1
}

function Wait-Prompt([int]$Pane) {
    # Claude's input box is drawn once its prompt is ready for typing.
    $null = Wait-Pane $Pane -Text -What "Claude's prompt" -Until {
        param($p)
        (Get-YienPaneText $p) -match '(?m)(\? for shortcuts|^\s*>\s|\u2502\s*>)'
    }
    Start-Sleep -Milliseconds 1500
}

function Focus-Pane([int]$Pane) {
    Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'pane.focus' -Arguments @{ pane = $Pane }
    $null = Wait-Pane $Pane -What 'the keyboard' -Until { param($p) $p.keyboard }
}

function Key([int]$Pane, [string]$Key) {
    $null = Send-YienKey -Name $name -ServerPid $serverPid -Pane $Pane -Key $Key
}

function Run-Turn([string]$Label, [int]$Pane, [switch]$ProfilePane) {
    $ok = Step $Label 'working' {
        Focus-Pane $Pane
        Send-YienText -Name $name -ServerPid $serverPid -Pane $Pane -Text 'please answer slow'
        Key $Pane 'enter'
        $p = Wait-Pane $Pane -What 'Working' -Until { param($p) $p.state -eq 'Working' }
        "state $($p.state)"
    }
    if (-not $ok) { return }
    $ok = Step $Label 'idle' {
        Key $Pane 'ctrl-c'
        $p = Wait-Pane $Pane -What 'Idle' -Until { param($p) $p.state -eq 'Idle' }
        $s = State
        if ($s.waiting -ne 0) { throw "$($s.waiting) panes need you after the interrupt" }
        "state $($p.state), nobody waiting"
    }
    if (-not $ok) { return }
    $null = Step $Label 'exit' {
        Start-Sleep -Milliseconds 1500
        Key $Pane 'ctrl-c'
        Key $Pane 'ctrl-c'
        if ($ProfilePane) {
            $p = Wait-Pane $Pane -What 'its program ending' -Until { param($p) $p.exited }
            if ($p.state -ne 'Idle') { throw "the ended pane shows $($p.state), not Idle" }
            $tab = (State).tabs | Where-Object { $_.id -eq $p.tab }
            if ($tab.needs_you) { throw 'the tab is red after a clean exit' }
            "exited with code $($p.exit_code), state $($p.state), tab mark $($tab.mark)"
        }
        else {
            $p = Wait-Pane $Pane -What 'its shell back' -Until { param($p) $p.name -ne 'claude' -and $p.running }
            if ($p.state -ne 'Idle') { throw "the shell pane shows $($p.state), not Idle" }
            "name $($p.name) running, state $($p.state)"
        }
    }
}

try {
    # Claude Code profile pane.
    $before = @((State).panes | ForEach-Object { $_.id })
    $claude = $null
    $ok = Step 'profile' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new_claude'
        $script:claude = Wait-NewPane $before -What 'the claude profile running' -Until {
            param($p) $p.profile_id -eq 'claude' -and $p.running
        }
        Wait-Prompt $script:claude.id
        "pane $($script:claude.id) $($script:claude.name)"
    }
    if ($ok) { Run-Turn 'profile' $claude.id -ProfilePane }

    # A shell pane with `claude` typed into it.
    $before = @((State).panes | ForEach-Object { $_.id })
    $shell = $null
    $ok = Step 'shell' 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.new'
        $script:shell = Wait-NewPane $before -What 'a running shell' -Until {
            param($p) $p.profile_id -eq 'shell' -and $p.running
        }
        $id = $script:shell.id
        $null = Wait-Pane $id -Text -What 'a shell prompt' -Until { param($p) (Get-YienPaneText $p) -match 'PS .*>' }
        Focus-Pane $id
        Send-YienText -Name $name -ServerPid $serverPid -Pane $id -Text 'claude'
        Key $id 'enter'
        $null = Wait-Pane $id -What "the pane named after claude" -Until { param($p) $p.name -eq 'claude' }
        Wait-Prompt $id
        "pane $id runs claude under $($script:shell.name)"
    }
    if ($ok) { Run-Turn 'shell' $shell.id }
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
