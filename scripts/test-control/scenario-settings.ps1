<#
.SYNOPSIS
Checks the settings screen (RF-CFG-05) through test control: it opens, a
change writes exactly its key to config.toml with the comment above it kept,
the window applies it, and Escape closes the screen.

.DESCRIPTION
Usage:
    ./scripts/test-control/scenario-settings.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Keep]

Starts the build with launch.ps1 (a throwaway profile under the slot, so the
config.toml written is the run's own), then:

    1. open       settings.open: overlay "settings", section Appearance,
                  the find field has the keyboard
    2. size       settings.set ui.font.size 17: the file's line reads
                  "ui.font.size = 17", the line above it is still the
                  setting's comment, no other line changed, and the state's
                  layout.ui_size becomes 17
    3. theme      settings.set theme washi, then sumi: the line changes
                  each time and the screen shows it
    4. keys       down, right on the theme row writes the next theme; the
                  cursor is on "theme"
    5. escape     Escape on the rows closes the screen

Stops the run with stop.ps1 unless -Keep, and exits with the number of
failed steps.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Root,
    [switch]$Keep,
    [double]$StateTimeoutSec = 15
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

$launchArgs = @{ Exe = $Exe }
if ($Root) { $launchArgs.Root = $Root }
$run = & (Join-Path $PSScriptRoot 'launch.ps1') @launchArgs
$name = $run.name
$serverPid = $run.pid
$config = Join-Path $run.data 'config.toml'
Write-Host "run $name, yien pid $serverPid, settings $config"

$results = New-Object System.Collections.Generic.List[object]

function Step {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$StepName,
        [Parameter(Mandatory)][scriptblock]$Check
    )
    try {
        $detail = & $Check
        $ok = $true
    }
    catch {
        $detail = "$_"
        $ok = $false
    }
    $results.Add([pscustomobject]@{ step = $StepName; ok = $ok; detail = "$detail" })
    Write-Host ("{0,-4} {1,-8} {2}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $StepName, $detail)
}

function Wait-Settings([scriptblock]$Until, [string]$What) {
    $state = Wait-YienState -Name $name -ServerPid $serverPid -TimeoutSec $StateTimeoutSec -Until $Until
    if (-not $state) { throw "never saw $What within $StateTimeoutSec s" }
    return $state
}

function Lines { return @(Get-Content -LiteralPath $config -Encoding UTF8) }

function Set-Setting([string]$Key, [string]$Value) {
    Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'settings.set' -Arguments @{ key = $Key; value = $Value }
}

try {
    $before = Lines

    Step 'open' {
        Invoke-YienCommand -Name $name -ServerPid $serverPid -Id 'settings.open'
        $s = Wait-Settings { param($s) $s.window.overlay -eq 'settings' -and $null -ne $s.settings } 'the settings overlay'
        if ($s.settings.section -ne 'Appearance') { throw "section $($s.settings.section)" }
        if ($null -ne $s.settings.cursor) { throw "the find field should have the keyboard, cursor is $($s.settings.cursor)" }
        "rows: $(@($s.settings.rows | ForEach-Object { $_.key }) -join ', ')"
    }

    Step 'size' {
        Set-Setting 'ui.font.size' '17'
        $null = Wait-Settings { param($s) $s.layout.ui_size -eq 17 } 'layout.ui_size 17'
        $after = Lines
        if ($after.Count -ne $before.Count) { throw "the file went from $($before.Count) to $($after.Count) lines" }
        $changed = @(0..($after.Count - 1) | Where-Object { $after[$_] -ne $before[$_] })
        if ($changed.Count -ne 1) { throw "$($changed.Count) lines changed: $($changed -join ', ')" }
        $at = $changed[0]
        if ($after[$at] -ne 'ui.font.size = 17') { throw "line $($at + 1) reads '$($after[$at])'" }
        if ($after[$at - 1] -ne $before[$at - 1] -or $after[$at - 1] -notlike '# *') {
            throw "the comment above changed: '$($after[$at - 1])'"
        }
        "line $($at + 1): $($after[$at]); above: $($after[$at - 1].Substring(0, [Math]::Min(50, $after[$at - 1].Length)))"
    }

    Step 'theme' {
        foreach ($theme in @('washi', 'sumi')) {
            Set-Setting 'theme' $theme
            $null = Wait-Settings {
                param($s)
                @($s.settings.rows | Where-Object { $_.key -eq 'theme' -and $_.value -eq $theme }).Count -eq 1
            } "theme $theme on the screen"
            if (-not ((Lines) -contains "theme = `"$theme`"")) { throw "the file has no theme = `"$theme`"" }
        }
        'washi, then sumi'
    }

    Step 'keys' {
        $null = Send-YienKey -Name $name -ServerPid $serverPid -Key 'down'
        $null = Send-YienKey -Name $name -ServerPid $serverPid -Key 'right'
        $s = Wait-Settings {
            param($s)
            @($s.settings.rows | Where-Object { $_.key -eq 'theme' -and $_.value -eq 'washi' }).Count -eq 1
        } 'theme washi after Right'
        if ($s.settings.cursor -ne 'theme') { throw "cursor on $($s.settings.cursor)" }
        if (-not $s.settings.body_has_keyboard) { throw 'the rows do not have the keyboard' }
        'down, right: theme washi'
    }

    Step 'escape' {
        $null = Send-YienKey -Name $name -ServerPid $serverPid -Key 'escape'
        $s = Wait-Settings { param($s) $null -eq $s.window.overlay } 'the screen closed'
        if ($null -ne $s.settings) { throw 'settings state still there' }
        "overlay none, region $($s.window.region)"
    }
}
finally {
    if (-not $Keep) { & (Join-Path $PSScriptRoot 'stop.ps1') -Root $run.root | Out-Null }
}

$failed = @($results | Where-Object { -not $_.ok }).Count
Write-Host "$($results.Count - $failed) passed, $failed failed"
exit $failed
