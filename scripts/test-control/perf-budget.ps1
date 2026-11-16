<#
.SYNOPSIS
Measures the three performance budgets of a YIEN release build on this
machine, with a throwaway profile and test control (ADR-21).

.DESCRIPTION
Usage:
    ./scripts/test-control/perf-budget.ps1 -Exe <path to yien.exe> [-Runs 5] [-Keys 30]

RNF-01 start-up to "can type": <= 1 s. Per run the script starts YIEN with a
fresh throwaway data folder and a fresh test control pipe, waits for the
focused pane to show a live shell prompt (last non-empty visible row ends in
'>', '$' or '#'; override with -PromptRegex), types a marker and waits for it
to echo in the visible rows. "Can type" is the moment the marker echoes.
The time is from process start (just before Start-Process) to that moment;
it therefore includes the shell's own start-up, which the ERS measurement
excludes. The first (cold) run is reported apart; the median and max of the
remaining runs are what the budget is checked against.

RNF-02 key to screen: <= 50 ms. The script clears the line and injects one
unique random 8-hex marker per key through test control (commit_text, the same
path IME text takes), then takes the time until a state dump shows the marker
in the pane's visible rows. The timer resolution is raised to 1 ms
(timeBeginPeriod, restored afterwards), the poll spins instead of sleeping,
and the resolution (one state dump round trip, min/median) is measured,
printed and recorded. The end time is frame_at_ms from the dump when present
(the frame time; excludes poll quantization), otherwise the stopwatch at poll
time. Both are reported as p50/p95/max over -Keys keys; the budget is checked
against the frame-based p95 when available, else the poll-observed p95. A
random marker cannot match anything but its own echo, so no accumulation or
line-wrap artifacts occur. Each injection carries 8 bytes, not 1; for this
pipeline (one commit_text call, one PTY write, one frame either way) the cost
is the same as a single keystroke.
What this measures: test-control injection -> YIEN's request handling -> PTY
write -> shell echo -> VT parse -> frame taken by the terminal view -> state
dump as seen by this script. What it leaves out: a real keyboard (hardware,
HID, Windows input, window activation, gpui key dispatch), the keystroke path
for special keys (only printable text through commit_text is exercised) and
photons (the frame's timestamp is when the view took it, not when the display
presented it). It is an upper bound in the injection direction and says
nothing about a real keyboard, which the ERS checks manually with a typometer
or camera on the reference machine.

RNF-03 memory with 20 panes: <= 300 MB. The script opens 20 panes: the
default tab plus 4 new shell tabs (tab.new_with), then in every tab 2 splits
right and 1 split down (pane.split_with), for 5 tabs x 4 panes. With
-FillLines above 0 it prints that many lines in every PowerShell pane
(1..FillLines | ForEach-Object { 'yienperf' }) so histories are full, per the
ERS measurement. Test control refuses text to a pane that is not the keyboard
pane, so every pane is tab.select'ed and pane.focus'ed in turn, and each fill
is verified by waiting for a per-pane sentinel in its visible rows (timeout
-FillTimeoutSec per pane). Panes that are not PowerShell-like are skipped and
panes that fail to fill are reported. After settling, it reads the YIEN
process's working set and private bytes with Get-Process, and the working set
and private bytes of every descendant process (shells, ConPTY hosts: not
YIEN's memory) with Win32_Process. The budget is checked against the YIEN
process's private bytes; the working set and the descendants are reported
separately.

Per-run folders and run.json files (for stop.ps1) are written under
<slot>/run/perf-budget-<timestamp>/, and the JSON summary under
<slot>/run/perf-budget-<timestamp>.json, both under the worktree's slot.
Exit code = the number of budgets missed (0, 1, 2 or 3).
Never run this against a profile or a YIEN you did not start for the test.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [double]$Runs = 5,
    [int]$Keys = 30,
    [int]$FillLines = 10000,
    [double]$FillTimeoutSec = 300,
    [double]$SettleSec = 3,
    [double]$ReadyTimeoutSec = 60,
    [string]$PromptRegex = '[>$#]\s*$',
    [double]$SplitTimeoutSec = 10,
    [double]$KeyTimeoutSec = 2,
    [string]$OutDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:CurrentRun = $null
trap {
    if ($script:CurrentRun -and (Get-Command Stop-YienRun -ErrorAction SilentlyContinue)) {
        Stop-YienRun $script:CurrentRun.Root
    }
    Write-Host "perf-budget failed: $_"
    exit 10
}
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

$BudgetRnf01Ms = 1000
$BudgetRnf02Ms = 50
$BudgetRnf03Mb = 300

if (-not $OutDir) { $OutDir = Join-Path (Get-YienSlotDir) 'run' }
$OutDir = (Resolve-Path -LiteralPath $OutDir -ErrorAction SilentlyContinue).Path
if (-not $OutDir) {
    $null = New-Item -ItemType Directory -Path $OutDir
    $OutDir = (Resolve-Path -LiteralPath $OutDir).Path
}
$Stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$WorkDir = Join-Path $OutDir "perf-budget-$Stamp"
$null = New-Item -ItemType Directory -Path $WorkDir
$JsonPath = Join-Path $OutDir "perf-budget-$Stamp.json"

function UtcMs { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }

function Stop-YienRun([string]$Root) {
    $stop = Join-Path $PSScriptRoot 'stop.ps1'
    try { & $stop -Root $Root } catch { Write-Warning "stop.ps1 failed for ${Root}: $_" }
    $script:CurrentRun = $null
}

<# Starts YIEN with a throwaway data folder and a fresh test control pipe.
   Returns a run object with the pipe name, pid and the stopwatch that started
   just before the process did. #>
function Start-ThrowawayRun([string]$Tag) {
    $name = "tc-$(New-YienNonce)"
    $root = Join-Path $WorkDir $Tag
    $data = Join-Path $root 'data'
    $work = Join-Path $root 'work'
    foreach ($dir in @($root, $data, $work)) { $null = New-Item -ItemType Directory -Path $dir }
    $root = (Resolve-Path -LiteralPath $root).Path
    $data = (Resolve-Path -LiteralPath $data).Path
    $work = (Resolve-Path -LiteralPath $work).Path

    $sw = New-Object System.Diagnostics.Stopwatch
    $saved = [Environment]::GetEnvironmentVariable('YIEN_DATA_DIR', 'Process')
    [Environment]::SetEnvironmentVariable('YIEN_DATA_DIR', $data, 'Process')
    $sw.Start()
    try {
        $yien = Start-Process -FilePath $Exe -WorkingDirectory $work -PassThru `
            -ArgumentList @('--test-control', $name)
    }
    finally {
        [Environment]::SetEnvironmentVariable('YIEN_DATA_DIR', $saved, 'Process')
    }

    $run = [ordered]@{
        name      = $name
        pid       = $yien.Id
        exe       = $Exe
        root      = $root
        data      = $data
        work      = $work
        mock_pid  = $null
        mock_port = $null
    }
    Write-YienUtf8 (Join-Path $root 'run.json') ($run | ConvertTo-Json)
    $script:CurrentRun = [pscustomobject]@{
        Name      = $name
        Pid       = $yien.Id
        Root      = $root
        Stopwatch = $sw
    }
    return $script:CurrentRun
}

function Get-KeyboardPane($State) {
    if ($null -eq $State.keyboard_pane) { return $null }
    return $State.panes | Where-Object { $_.id -eq $State.keyboard_pane } | Select-Object -First 1
}

function Test-YienAlive($Run) {
    return [bool](Get-Process -Id $Run.Pid -ErrorAction SilentlyContinue)
}

function Get-PaneFrameAtMs($Pane) {
    $p = $Pane.PSObject.Properties['frame_at_ms']
    if ($null -ne $p) { return $p.Value }
    return $null
}

function Get-PaneTextOf($Pane) {
    return (Get-YienPaneText $Pane)
}

<# A needle in a pane's visible rows, ignoring row breaks (a marker may straddle a wrap). #>
function Test-PaneText($Pane, [string]$Needle) {
    return ((Get-PaneTextOf $Pane) -replace '\r?\n', '').Contains($Needle)
}

<# Waits for the keyboard pane to look like a live shell at its prompt:
   running, and the last non-empty visible row matches -Regex. #>
function Wait-Prompt($Run, [double]$TimeoutSec) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
    do {
        if (-not (Test-YienAlive $Run)) { return $null }
        $state = $null
        try { $state = Get-YienState -Name $Run.Name -Text -ServerPid $Run.Pid } catch { $state = $null }
        if ($state) {
            $pane = Get-KeyboardPane $state
            if ($pane -and $pane.running -and -not $pane.exited) {
                $rows = (Get-PaneTextOf $pane) -split "`n"
                $last = ($rows | Where-Object { $_.Trim() -ne '' } | Select-Object -Last 1)
                if ($last -and ($last -match $PromptRegex)) { return $pane }
            }
        }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    return $null
}

<# Waits until -Text appears in the keyboard pane's visible rows. #>
function Wait-Marker($Run, [string]$Text, [double]$TimeoutSec) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
    do {
        if (-not (Test-YienAlive $Run)) { return $null }
        $state = $null
        try { $state = Get-YienState -Name $Run.Name -Text -ServerPid $Run.Pid } catch { $state = $null }
        if ($state) {
            $pane = Get-KeyboardPane $state
            if ($pane -and (Test-PaneText $pane $Text)) { return $pane }
        }
        Start-Sleep -Milliseconds 10
    } while ([DateTime]::UtcNow -lt $deadline)
    return $null
}

$misses = 0
$results = [ordered]@{}
$notes = New-Object System.Collections.Generic.List[string]

# ---------------------------------------------------------------- RNF-01
Write-Host ""
Write-Host "== RNF-01 start-up to can type (budget ${BudgetRnf01Ms} ms) =="
$rnf01Runs = @()
for ($i = 1; $i -le [int]$Runs; $i++) {
    $run = Start-ThrowawayRun "rnf-01-$i"
    $promptPane = Wait-Prompt $run $ReadyTimeoutSec
    $promptMs = $null
    $canTypeMs = $null
    if ($promptPane) {
        $promptMs = [math]::Round($run.Stopwatch.Elapsed.TotalMilliseconds)
        $marker = "YIENPERF$i"
        Send-YienText -Name $run.Name -ServerPid $run.Pid -Pane ([int]$promptPane.id) -Text $marker
        $echoed = Wait-Marker $run $marker $ReadyTimeoutSec
        if ($echoed) { $canTypeMs = [math]::Round($run.Stopwatch.Elapsed.TotalMilliseconds) }
    }
    Stop-YienRun $run.Root
    $rnf01Runs += [pscustomobject]@{
        run        = $i
        cold       = ($i -eq 1)
        prompt_ms  = $promptMs
        can_type_ms = $canTypeMs
    }
    $shown = if ($null -eq $canTypeMs) { 'FAILED (no prompt or no echo)' } else { "$canTypeMs ms" }
    Write-Host ("run {0}{1}: prompt={2} can-type={3}" -f $i, $(if ($i -eq 1) { ' (cold)' } else { '' }), `
        $(if ($null -eq $promptMs) { 'n/a' } else { "$promptMs ms" }), $shown)
}
$cold = $rnf01Runs | Where-Object { $_.cold } | Select-Object -First 1
$warm = @($rnf01Runs | Where-Object { $_.can_type_ms -ne $null -and -not $_.cold } | ForEach-Object { $_.can_type_ms })
$warmOk = $warm.Count -gt 0
$sorted = @($warm | Sort-Object)
$median = if ($warmOk) { $sorted[[int][math]::Floor(($sorted.Count - 1) / 2)] } else { $null }
if (($sorted.Count % 2) -eq 0 -and $sorted.Count -ge 2) {
    $median = [math]::Round(($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2)
}
$max = if ($warmOk) { $sorted[$sorted.Count - 1] } else { $null }
$rnf01Miss = (-not $warmOk) -or ($median -gt $BudgetRnf01Ms)
if ($rnf01Miss) { $misses++ }
if (-not $warmOk) { $notes.Add('RNF-01: no warm run reached can-type; the verdict is unmeasurable (the cold run is reported apart, never substituted)') }
$rnf01 = [ordered]@{
    budget_ms      = $BudgetRnf01Ms
    definition     = 'process start (just before Start-Process) to the marker typed into the keyboard pane echoing in its visible rows; includes shell start-up'
    ers_note       = 'ERS: p95 of 20 cold starts, shell start excluded, to the first visible character. This script reports median/max of warm runs with the cold run reported apart.'
    runs           = $rnf01Runs
    cold_ms        = $cold.can_type_ms
    warm_median_ms = $median
    warm_max_ms    = $max
    verdict_measurable = $warmOk
    budget_missed  = $rnf01Miss
}
$results.rnf_01 = $rnf01
Write-Host ("cold run: {0} ms; warm median: {1} ms; warm max: {2} ms -> {3}" -f `
    $(if ($null -eq $cold.can_type_ms) { 'n/a' } else { $cold.can_type_ms }), `
    $(if ($null -eq $median) { 'n/a' } else { $median }), `
    $(if ($null -eq $max) { 'n/a' } else { $max }), `
    $(if ($rnf01Miss) { 'MISS' } else { 'PASS' }))

# ---------------------------------------------------------------- RNF-02
# The timer resolution is raised to 1 ms for this section only, and the
# poll spins instead of sleeping, so the poll period stays at one state
# dump round trip (measured and printed as the resolution). The measurement
# ends at frame_at_ms from the dump when present, so the latency is the
# frame time, not the poll time; poll-observed is reported alongside.
Write-Host ""
Write-Host "== RNF-02 key to screen (budget ${BudgetRnf02Ms} ms, p95 of $Keys keys) =="
$timerRaised = $false
if (-not ('YienPerf.Time' -as [type])) {
    try {
        Add-Type -Namespace YienPerf -Name Time -MemberDefinition @'
[DllImport("winmm.dll", EntryPoint="timeBeginPeriod")]
public static extern uint timeBeginPeriod(uint uPeriod);
[DllImport("winmm.dll", EntryPoint="timeEndPeriod")]
public static extern uint timeEndPeriod(uint uPeriod);
'@
    } catch { Write-Warning "timeBeginPeriod is unavailable; timer resolution unchanged: $_" }
}
if ('YienPerf.Time' -as [type]) { [void][YienPerf.Time]::timeBeginPeriod(1); $timerRaised = $true }
$pollProbe = New-Object System.Collections.Generic.List[double]
$pollLat = New-Object System.Collections.Generic.List[double]
$frameLat = New-Object System.Collections.Generic.List[double]
$timeouts = 0
try {
    $run = Start-ThrowawayRun 'rnf-02'
    $promptPane = Wait-Prompt $run $ReadyTimeoutSec
    if (-not $promptPane) {
        Write-Warning 'no prompt; RNF-02 not measured'
        $timeouts = $Keys
    } else {
        # Calibration: the cost of one state dump round trip is the floor of
        # the poll resolution.
        for ($k = 0; $k -lt 10; $k++) {
            $probe = New-Object System.Diagnostics.Stopwatch
            $probe.Start()
            $null = Get-YienState -Name $run.Name -Text -ServerPid $run.Pid
            $probe.Stop()
            $pollProbe.Add($probe.Elapsed.TotalMilliseconds)
        }
        $paneId = [int]$promptPane.id
        for ($i = 0; $i -lt $Keys; $i++) {
            # Clear the line, then inject one unique 8-hex marker; the needle is
            # random, so it can only match what this key echoed.
            $null = Send-YienKey -Name $run.Name -ServerPid $run.Pid -Key 'ctrl-c'
            $marker = New-YienNonce
            $sw = New-Object System.Diagnostics.Stopwatch
            $t0ms = UtcMs
            $sw.Start()
            Send-YienText -Name $run.Name -ServerPid $run.Pid -Pane $paneId -Text $marker
            $observed = $false
            while (-not $observed -and $sw.Elapsed.TotalSeconds -lt $KeyTimeoutSec) {
                if (-not (Test-YienAlive $run)) { throw 'yien exited during RNF-02' }
                $state = $null
                try { $state = Get-YienState -Name $run.Name -Text -ServerPid $run.Pid } catch { $state = $null }
                if ($state) {
                    $pane = Get-KeyboardPane $state
                    if ($pane -and (Test-PaneText $pane $marker)) {
                        $observed = $true
                        $pollLat.Add($sw.Elapsed.TotalMilliseconds)
                        $frameMs = Get-PaneFrameAtMs $pane
                        if ($null -ne $frameMs) { $frameLat.Add([math]::Max(0.0, [double]$frameMs - [double]$t0ms)) }
                    }
                }
            }
            if (-not $observed) { $timeouts++ }
        }
        $null = Send-YienKey -Name $run.Name -ServerPid $run.Pid -Key 'ctrl-c'
    }
    Stop-YienRun $run.Root
} finally {
    if ($timerRaised) { [void][YienPerf.Time]::timeEndPeriod(1) }
}

function Percentile([double[]]$Values, [double]$P) {
    if ($Values.Count -eq 0) { return $null }
    $s = @($Values | Sort-Object)
    $rank = [int][math]::Ceiling($P * $s.Count) - 1
    return [math]::Round($s[$rank], 1)
}
function Stat3([double[]]$Values) {
    if ($Values.Count -eq 0) { return [ordered]@{ p50 = $null; p95 = $null; max = $null } }
    return [ordered]@{
        p50 = Percentile $Values 0.50
        p95 = Percentile $Values 0.95
        max = [math]::Round(($Values | Measure-Object -Maximum).Maximum, 1)
    }
}
$poll = Stat3 $pollLat.ToArray()
$frame = Stat3 $frameLat.ToArray()
$resMin = $null; $resMed = $null
if ($pollProbe.Count -gt 0) {
    $resMin = [math]::Round(($pollProbe | Measure-Object -Minimum).Minimum, 2)
    $sortedProbe = @($pollProbe | Sort-Object)
    $resMed = [math]::Round($sortedProbe[[int][math]::Floor(($sortedProbe.Count - 1) / 2)], 2)
}
$frameBasis = $frameLat.Count -gt 0
$verdict = if ($frameBasis) { $frame } else { $poll }
$basis = if ($frameBasis) {
    'budget checked against frame_at.p95 (frame time taken by the terminal view, from the dump; excludes poll quantization)'
} else {
    'budget checked against poll_observed.p95 (frame_at_ms unavailable in this run)'
}
$rnf02Miss = ($timeouts -gt 0) -or ($null -eq $verdict.p95) -or ($verdict.p95 -gt $BudgetRnf02Ms)
if ($rnf02Miss) { $misses++ }
$rnf02 = [ordered]@{
    budget_ms     = $BudgetRnf02Ms
    keys          = $Keys
    timeouts      = $timeouts
    timer_raised  = $timerRaised
    poll_min_ms   = $resMin
    poll_median_ms = $resMed
    poll_observed = $poll
    frame_at      = $frame
    verdict_source = $(if ($frameBasis) { 'frame_at' } else { 'poll_observed' })
    verdict_p95_ms = $verdict.p95
    basis         = $basis
    measures      = 'test-control injection -> request handling -> PTY write -> shell echo -> VT parse -> frame taken by the view -> state dump observed by this script'
    leaves_out    = @(
        'a real keyboard: hardware, HID, Windows input, window activation, gpui key dispatch',
        'the keystroke path for special keys: only printable text through commit_text is exercised',
        'photons: frame_at_ms is when the terminal view took the frame, not when the display presented it'
    )
    ers_note      = 'ERS checks key-to-pixel manually on the reference machine (typometer or camera) and compares p95 to Windows Terminal; that comparison is not automated here'
    budget_missed = $rnf02Miss
}
$results.rnf_02 = $rnf02
Write-Host ("poll resolution: round trip min={0} ms median={1} ms; timer 1 ms: {2}" -f `
    $(if ($null -eq $resMin) { 'n/a' } else { $resMin }), `
    $(if ($null -eq $resMed) { 'n/a' } else { $resMed }), `
    $(if ($timerRaised) { 'raised' } else { 'not raised' }))
Write-Host ("poll-observed ms: p50={0} p95={1} max={2}; frame-based ms: p50={3} p95={4} max={5}; timeouts={6} -> verdict on {7} -> {8}" -f `
    $poll.p50, $poll.p95, $poll.max, $frame.p50, $frame.p95, $frame.max, $timeouts, `
    $(if ($frameBasis) { 'frame_at' } else { 'poll_observed' }), `
    $(if ($rnf02Miss) { 'MISS' } else { 'PASS' }))
$notes.Add('RNF-02 is test-control injection to rendered state, not keyboard to photons; see rnf_02.leaves_out')

# ---------------------------------------------------------------- RNF-03
Write-Host ""
Write-Host "== RNF-03 memory with 20 panes (budget ${BudgetRnf03Mb} MB) =="
$run = Start-ThrowawayRun 'rnf-03'
$promptPane = Wait-Prompt $run $ReadyTimeoutSec
if (-not $promptPane) { throw 'no prompt in the default tab; cannot open 19 more panes' }

# 4 new shell tabs -> 5 tabs, then 3 splits per tab -> 4 panes per tab -> 20.
for ($i = 0; $i -lt 4; $i++) {
    Invoke-YienCommand -Name $run.Name -ServerPid $run.Pid -Id 'tab.new_with' -Arguments @{ profile = 'shell' }
    $null = Wait-YienState -Name $run.Name -ServerPid $run.Pid -TimeoutSec $SplitTimeoutSec -Until {
        param($s) $s.tabs.Count -eq (2 + $i)
    }
}
$state = Get-YienState -Name $run.Name -ServerPid $run.Pid
$tabIds = @($state.tabs | ForEach-Object { [int]$_.id })
$paneTarget = @($state.panes).Count
$splits = @(@{ direction = 'right' }, @{ direction = 'right' }, @{ direction = 'down' })
foreach ($tab in $tabIds) {
    Invoke-YienCommand -Name $run.Name -ServerPid $run.Pid -Id 'tab.select' -Arguments @{ tab = $tab }
    foreach ($split in $splits) {
        $paneTarget += 1
        Invoke-YienCommand -Name $run.Name -ServerPid $run.Pid -Id 'pane.split_with' `
            -Arguments @{ profile = 'shell'; direction = $split.direction }
        $null = Wait-YienState -Name $run.Name -ServerPid $run.Pid -TimeoutSec $SplitTimeoutSec -Until {
            param($s) $s.panes.Count -eq $paneTarget
        }
    }
}
$state = Get-YienState -Name $run.Name -ServerPid $run.Pid
$paneCount = @($state.panes).Count
if ($paneCount -ne 20) { throw "expected 20 panes, got $paneCount; measurement invalid" }
Write-Host "panes open: $paneCount"

# Fill each PowerShell pane's scrollback to $FillLines lines, per the ERS
# measurement. Test control refuses text to a pane that is not the keyboard
# pane, so every pane is tab.select'ed and pane.focus'ed first, and the sentinel
# is verified in that pane's visible rows while it is the visible pane.
$sentinel = 'YIENPERFDONE'
$filled = 0
$skipped = 0
$fillFailed = New-Object System.Collections.Generic.List[int]
if ($FillLines -gt 0) {
    foreach ($pane in @($state.panes)) {
        $pname = [string]$pane.name
        if ($pname -notmatch '(?i)^(pwsh|powershell|ps)') { $skipped++; continue }
        $id = [int]$pane.id
        Invoke-YienCommand -Name $run.Name -ServerPid $run.Pid -Id 'tab.select' -Arguments @{ tab = [int]$pane.tab }
        Invoke-YienCommand -Name $run.Name -ServerPid $run.Pid -Id 'pane.focus' -Arguments @{ pane = $id }
        $focused = Wait-YienState -Name $run.Name -ServerPid $run.Pid -TimeoutSec 5 -Until {
            param($s) $null -ne $s.keyboard_pane -and [int]$s.keyboard_pane -eq $id
        }
        if (-not $focused) { Write-Warning "pane $id did not take the keyboard; skipping its fill"; $fillFailed.Add($id); continue }
        Send-YienText -Name $run.Name -ServerPid $run.Pid -Pane $id -Text "1..$FillLines | ForEach-Object { 'yienperf' }"
        Send-YienKey -Name $run.Name -ServerPid $run.Pid -Pane $id -Key 'enter'
        Send-YienText -Name $run.Name -ServerPid $run.Pid -Pane $id -Text "echo $sentinel$id"
        Send-YienKey -Name $run.Name -ServerPid $run.Pid -Pane $id -Key 'enter'
        $needle = "$sentinel$id"
        $filledOk = Wait-YienState -Name $run.Name -ServerPid $run.Pid -TimeoutSec $FillTimeoutSec -IntervalMs 250 -Text -Until {
            param($s)
            $p = $s.panes | Where-Object { [int]$_.id -eq $id } | Select-Object -First 1
            ($null -ne $p) -and (Test-PaneText $p $needle)
        }
        if ($filledOk) { $filled++ } else { Write-Warning "pane $id fill timed out after ${FillTimeoutSec}s"; $fillFailed.Add($id) }
    }
}
Start-Sleep -Seconds $SettleSec

function Get-DescendantIds([int]$Root) {
    $ids = New-Object System.Collections.Generic.List[int]
    $queue = New-Object System.Collections.Generic.Queue[int]
    $queue.Enqueue($Root)
    while ($queue.Count -gt 0) {
        $cur = $queue.Dequeue()
        $kids = Get-CimInstance Win32_Process -Filter "ParentProcessId = $cur" -ErrorAction SilentlyContinue
        foreach ($k in $kids) {
            $ids.Add([int]$k.ProcessId)
            $queue.Enqueue([int]$k.ProcessId)
        }
    }
    return $ids
}

$yienProc = Get-Process -Id $run.Pid -ErrorAction SilentlyContinue
if (-not $yienProc) { throw "yien $($run.Pid) is not running; cannot read its memory" }
$yienWorking = $yienProc.WorkingSet64
$yienPrivate = $yienProc.PrivateMemorySize64
$descendants = @()
$childWorking = 0L
$childPrivate = 0L
foreach ($id in (Get-DescendantIds $run.Pid)) {
    $p = Get-Process -Id $id -ErrorAction SilentlyContinue
    if (-not $p) { continue }
    $descendants += [pscustomobject]@{
        pid          = $id
        name         = $p.ProcessName
        working_set_mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
        private_mb   = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
    }
    $childWorking += $p.WorkingSet64
    $childPrivate += $p.PrivateMemorySize64
}
Stop-YienRun $run.Root

$yienPrivateMb = [math]::Round($yienPrivate / 1MB, 1)
$yienWorkingMb = [math]::Round($yienWorking / 1MB, 1)
$childPrivateMb = [math]::Round($childPrivate / 1MB, 1)
$childWorkingMb = [math]::Round($childWorking / 1MB, 1)
$rnf03Miss = $yienPrivateMb -gt $BudgetRnf03Mb
if ($rnf03Miss) { $misses++ }
$rnf03 = [ordered]@{
    budget_mb            = $BudgetRnf03Mb
    basis                = 'budget checked against the YIEN process''s own private bytes; ERS excludes shells, agents and ConPTY (OpenConsole) processes, which are reported separately'
    panes                = $paneCount
    fill_lines           = $FillLines
    panes_filled         = $filled
    panes_fill_skipped   = $skipped
    panes_fill_failed    = $fillFailed.Count
    fill_failed_panes    = $fillFailed.ToArray()
    yien_private_mb      = $yienPrivateMb
    yien_working_set_mb  = $yienWorkingMb
    children_private_mb  = $childPrivateMb
    children_working_set_mb = $childWorkingMb
    children             = $descendants
    budget_missed        = $rnf03Miss
}
$results.rnf_03 = $rnf03
Write-Host ("yien: private={0} MB working set={1} MB; descendants: private={2} MB working set={3} MB across {4} processes -> {5}" -f `
    $yienPrivateMb, $yienWorkingMb, $childPrivateMb, $childWorkingMb, $descendants.Count, `
    $(if ($rnf03Miss) { 'MISS' } else { 'PASS' }))

# ---------------------------------------------------------------- summary
$notes.Add('RNF-01 includes shell start-up and uses N runs with the cold one reported apart; the ERS measures p95 of 20 cold starts, shell start excluded')
$notes.Add('RNF-03 budget basis is the YIEN process private bytes; working set and descendants are reported alongside')
if ($fillFailed.Count -gt 0) { $notes.Add("RNF-03: $($fillFailed.Count) panes could not be filled; the 10000-line history is unmeasured for them") }
$results.budgets_missed = $misses
$results.notes = $notes.ToArray()
$results.exe = $Exe
$results.generated = (Get-Date).ToString('o')
$results.machine = [ordered]@{
    os         = (Get-CimInstance Win32_OperatingSystem -ErrorAction SilentlyContinue | ForEach-Object { $_.Caption + ' ' + $_.Version })
    powershell = $PSVersionTable.PSVersion.ToString()
}

Write-YienUtf8 $JsonPath (($results | ConvertTo-Json -Depth 10))

Write-Host ""
Write-Host "+-------+-------------------------+----------------+--------+"
Write-Host "| RNF   | measurement             | budget         | result |"
Write-Host "+-------+-------------------------+----------------+--------+"
Write-Host ("| RNF-01 | can-type warm median {0,5} ms | <= {1,5} ms   | {2,-6} |" -f $median, $BudgetRnf01Ms, $(if ($rnf01Miss) { 'MISS' } else { 'PASS' }))
Write-Host ("| RNF-02 | p95 {0,5} ms {1,5} ({2,2} keys) | <= {3,5} ms   | {4,-6} |" -f $verdict.p95, $(if ($frameBasis) { 'frame' } else { 'poll ' }), $Keys, $BudgetRnf02Ms, $(if ($rnf02Miss) { 'MISS' } else { 'PASS' }))
Write-Host ("| RNF-03 | yien private {0,6} MB    | <= {1,5} MB    | {2,-6} |" -f $yienPrivateMb, $BudgetRnf03Mb, $(if ($rnf03Miss) { 'MISS' } else { 'PASS' }))
Write-Host "+-------+-------------------------+----------------+--------+"
Write-Host "JSON: $JsonPath"
Write-Host "Budgets missed: $misses (exit code = budgets missed)"
exit $misses
