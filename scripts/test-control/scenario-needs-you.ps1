<#
.SYNOPSIS
Checks that Claude Code in a folder it does not trust yet shows its folder
trust screen, that the pane and its tab are Needs you and the counter counts
it, and that answering the screen clears it (RF-ATN-08, RF-ATN-09, RN-01).

.DESCRIPTION
Usage:
    ./scripts/test-control/scenario-needs-you.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Keep]

Starts the build with launch.ps1 -MockClaude -NoTrust (a throwaway profile
under the slot, Claude Code pointed at a local mock API, the work folder not
trusted), opens the Claude pane through test control, prints PASS or FAIL
per step, stops the run with stop.ps1 unless -Keep, and exits with the
number of failed steps.

    1. trust       the trust screen shows and YIEN reads the pane as Needs you:
                    pane and tab, the tab's red mark and the title bar counter
    2. unselected  another tab selected: the Claude tab still shows Needs you,
                    its mark and the counter (RF-ATN-15)
    3. answer      picking "Yes, I trust this folder" clears it: the pane and
                    the tab leave Needs you and the counter drops to zero

The answer step reads the screen to find which choice is under the cursor,
so it works whichever option the build defaults to.

Needs: node on PATH, `claude` on PATH, PowerShell as YIEN's shell.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Root,
    [switch]$Keep,
    [double]$StartTimeoutSec = 60,
    [double]$StateTimeoutSec = 30
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

$launchArgs = @{ Exe = $Exe; MockClaude = $true; NoTrust = $true }
if ($Root) { $launchArgs.Root = $Root }
$run = & (Join-Path $PSScriptRoot 'launch.ps1') @launchArgs
$name = $run.name
$serverPid = $run.pid
Write-Host "run $name, yien pid $serverPid, folder $($run.root)"

$results = New-Object System.Collections.Generic.List[object]

function Step([string]$StepPane, [string]$StepName, [scriptblock]$Check) {
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

function Focus-Pane([int]$Pane) {
    Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'pane.focus' -Arguments @{ pane = $Pane }
    $null = Wait-Pane $Pane -What 'the keyboard' -Until { param($p) $p.keyboard }
}

function Key([int]$Pane, [string]$Key) {
    $null = Send-YienKey -Name $name -ServerPid $serverPid -Pane $Pane -Key $Key
}

# The launch tab, the one that is not the Claude tab.
function Shell-Tab-Id([int]$ClaudeTab) {
    $s = State
    $other = $s.tabs | Where-Object { $_.id -ne $ClaudeTab } | Select-Object -First 1
    if (-not $other) { throw 'no other tab to select' }
    return $other.id
}

function Select-Tab([int]$Tab) {
    Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'tab.select' -Arguments @{ tab = $Tab }
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec 10 -Until {
        param($s) $s.selected_tab -eq $Tab
    }
    if (-not $state) { throw "tab $Tab was not selected" }
}

try {
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
        $ok = Step 'claude' 'trust' {
            $p = Wait-Pane $claude.id -Text -What 'the trust screen as Needs you' -Until {
                param($p) $p.state -eq 'Needs you'
            }
            $text = Get-YienPaneText $p
            if ($text -notmatch 'I trust this folder') {
                throw "the pane is Needs you but the trust screen is not drawn: $text"
            }
            $s = State
            if ($s.waiting -ne 1) { throw "the counter counts $($s.waiting), not the one pane that needs you" }
            $tab = $s.tabs | Where-Object { $_.id -eq $p.tab }
            if ($tab.state -ne 'Needs you') { throw "the tab's state is '$($tab.state)', not Needs you" }
            if ($tab.mark -ne 'needs_you') { throw "the tab's mark is '$($tab.mark)', not needs_you" }
            if (-not $tab.needs_you) { throw 'the tab row does not carry the red mark' }
            "pane and tab Needs you, counter $($s.waiting), tab mark $($tab.mark)"
        }
        if ($ok) {
            $claudeTab = (Get-YienPane (State) $claude.id).tab
            $shellTab = Shell-Tab-Id $claudeTab
            $ok = Step 'claude' 'unselected' {
                Select-Tab $shellTab
                $s = State
                $tab = $s.tabs | Where-Object { $_.id -eq $claudeTab }
                if ($tab.state -ne 'Needs you') { throw "the unselected tab's state is '$($tab.state)'" }
                if ($tab.mark -ne 'needs_you') { throw "the unselected tab's mark is '$($tab.mark)'" }
                if (-not $tab.needs_you) { throw 'the unselected tab lost its red mark' }
                if ($s.waiting -ne 1) { throw "the counter counts $($s.waiting), not one" }
                Select-Tab $claudeTab
                "the unselected tab still Needs you, mark $($tab.mark), counter $($s.waiting)"
            }
        }
        if ($ok) {
            $null = Step 'claude' 'answer' {
                # The trust screen lists two choices with the cursor on one of
                # them; read which, move to "Yes, I trust this folder" if it is
                # not there, and confirm.
                $p = Wait-Pane $claude.id -Text -What "the trust screen's choices" -Until {
                    param($p) (Get-YienPaneText $p) -match 'I trust this folder'
                }
                Focus-Pane $claude.id
                $text = Get-YienPaneText $p
                $onYes = $text -match '(?m)[\u276f>]\s*Yes, I trust this folder'
                if (-not $onYes) { Key $claude.id 'down' }
                Key $claude.id 'enter'
                $p = Wait-Pane $claude.id -What 'leaving Needs you' -Until {
                    param($p) $p.state -ne 'Needs you'
                }
                $s = State
                if ($s.waiting -ne 0) { throw "the counter counts $($s.waiting) after the answer" }
                $tab = $s.tabs | Where-Object { $_.id -eq $p.tab }
                if ($tab.needs_you) { throw 'the tab still carries the red mark' }
                if ($tab.state -eq 'Needs you') { throw "the tab's state is still '$($tab.state)'" }
                "pane $($p.state), tab '$($tab.state)', counter $($s.waiting), mark $($tab.mark)"
            }
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
