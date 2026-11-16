<#
.SYNOPSIS
Starts a YIEN build with test control on and a throwaway profile.

.DESCRIPTION
Usage:
    ./scripts/test-control/launch.ps1 -Exe <path to yien.exe> [-Root <folder>] [-Name <name>] [-MockClaude] [-NoTrust]

Everything the run writes goes under -Root, a new folder made for it:
    data/     YIEN's data and config (YIEN_DATA_DIR): session, settings, logs
    work/     the folder YIEN starts in, so its first project
    claude/   CLAUDE_CONFIG_DIR, with -MockClaude
    mock.log  one line per mock API request, with -MockClaude
    run.json  the run's facts: name, pids, folders, mock port
-Root defaults to run/tc-<name> under the worktree's slot (the folder named in
the untracked .yien-slot file).

-Name is the test control name; by default a random tc-<16 hex digits>.

-MockClaude starts mock-messages-api.js with Node on a free loopback port and
points Claude Code at it: a throwaway CLAUDE_CONFIG_DIR that has finished
onboarding and trusts work/, a dummy API key, ANTHROPIC_BASE_URL on the mock.
No real account or quota is used.

-NoTrust (with -MockClaude) pre-trusts nothing: Claude Code shows its folder
trust screen in work/, for the scenarios that drive the red mark through it.

Writes the run's facts as an object, and as JSON to <Root>/run.json. Stop the
run with stop.ps1. Never run this against a profile or a YIEN you did not
start for the test.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Root,
    [string]$Name,
    [switch]$MockClaude,
    [switch]$NoTrust,
    [double]$ReadyTimeoutSec = 30
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'YienTestControl.psm1') -Force

function New-Nonce {
    $bytes = New-Object byte[] 8
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
    return -join ($bytes | ForEach-Object { $_.ToString('x2') })
}

function Get-SlotDir {
    $top = & git -C $PSScriptRoot rev-parse --show-toplevel 2>$null
    if (-not $top) { throw 'not inside a git worktree; pass -Root' }
    $slotFile = Join-Path $top '.yien-slot'
    if (-not (Test-Path -LiteralPath $slotFile)) { throw "$slotFile is missing; pass -Root" }
    $slot = (Get-Content -LiteralPath $slotFile -TotalCount 1).Trim()
    if (-not $slot) { throw "$slotFile is empty; pass -Root" }
    return $slot
}

function Get-FreePort {
    $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try { return $listener.LocalEndpoint.Port } finally { $listener.Stop() }
}

function Wait-Port([int]$Port, [double]$TimeoutSec) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
    do {
        $client = New-Object System.Net.Sockets.TcpClient
        try {
            $client.Connect([System.Net.IPAddress]::Loopback, $Port)
            return $true
        }
        catch { Start-Sleep -Milliseconds 100 }
        finally { $client.Dispose() }
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Write-Utf8([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

$Exe = (Resolve-Path -LiteralPath $Exe).Path
if (-not $Name) { $Name = "tc-$(New-Nonce)" }
if ($Name -cnotmatch '^[A-Za-z0-9_-]{1,64}$') { throw "not a test control name: '$Name'" }
if (-not $Root) { $Root = Join-Path (Join-Path (Get-SlotDir) 'run') $Name }
if (Test-Path -LiteralPath $Root) { throw "$Root exists; every run gets a new folder" }

$data = Join-Path $Root 'data'
$work = Join-Path $Root 'work'
foreach ($dir in @($Root, $data, $work)) { $null = New-Item -ItemType Directory -Path $dir }
$Root = (Resolve-Path -LiteralPath $Root).Path
$data = (Resolve-Path -LiteralPath $data).Path
$work = (Resolve-Path -LiteralPath $work).Path

$env_ = [ordered]@{ YIEN_DATA_DIR = $data }
$mock = $null
$mockPort = $null
if ($MockClaude) {
    $node = Get-Command node -ErrorAction SilentlyContinue
    if (-not $node) { throw 'node is not on PATH; it runs the mock Messages API' }
    $claudeDir = Join-Path $Root 'claude'
    $null = New-Item -ItemType Directory -Path $claudeDir
    $mockPort = Get-FreePort
    $mockLog = Join-Path $Root 'mock.log'
    $script = Join-Path $PSScriptRoot 'mock-messages-api.js'
    $mock = Start-Process -FilePath $node.Source -PassThru -WindowStyle Hidden `
        -ArgumentList @("`"$script`"", '--port', $mockPort, '--log', "`"$mockLog`"")
    if (-not (Wait-Port $mockPort 10)) {
        Stop-Process -Id $mock.Id -ErrorAction SilentlyContinue
        throw "the mock API did not listen on port $mockPort"
    }
    # A dummy key in the shape Claude Code expects; it never leaves the machine.
    $key = 'sk-ant-api03-' + ('y' * 80) + '-MOCKAA'
    # -NoTrust leaves the folder untrusted so the trust screen shows.
    $projects = @{}
    if (-not $NoTrust) {
        $trusted = @{ hasTrustDialogAccepted = $true; hasCompletedProjectOnboarding = $true; allowedTools = @() }
        $projects[$work] = $trusted
        $projects[$work.Replace('\', '/')] = $trusted
    }
    $config = @{
        hasCompletedOnboarding         = $true
        theme                          = 'dark'
        numStartups                    = 5
        autoUpdates                    = $false
        customApiKeyResponses          = @{ approved = @($key.Substring($key.Length - 20)); rejected = @() }
        projects                       = $projects
        bypassPermissionsModeAccepted  = $true
    }
    Write-Utf8 (Join-Path $claudeDir '.claude.json') ($config | ConvertTo-Json -Depth 10)
    $env_.CLAUDE_CONFIG_DIR = $claudeDir
    $env_.ANTHROPIC_API_KEY = $key
    $env_.ANTHROPIC_BASE_URL = "http://127.0.0.1:$mockPort"
    $env_.DISABLE_AUTOUPDATER = '1'
    $env_.DISABLE_TELEMETRY = '1'
    $env_.DISABLE_ERROR_REPORTING = '1'
    $env_.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'
}

# Start-Process passes this process's environment on; set it for the start
# only and put the old values back.
$saved = @{}
foreach ($k in $env_.Keys) {
    $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
    [Environment]::SetEnvironmentVariable($k, $env_[$k], 'Process')
}
try {
    $yien = Start-Process -FilePath $Exe -WorkingDirectory $work -PassThru `
        -ArgumentList @('--test-control', $Name)
}
finally {
    foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }
}

$run = [ordered]@{
    name      = $Name
    pid       = $yien.Id
    exe       = $Exe
    root      = $Root
    data      = $data
    work      = $work
    mock_pid  = if ($mock) { $mock.Id } else { $null }
    mock_port = $mockPort
}
Write-Utf8 (Join-Path $Root 'run.json') ($run | ConvertTo-Json)

$deadline = [DateTime]::UtcNow.AddSeconds($ReadyTimeoutSec)
$ready = $false
while (-not $ready -and [DateTime]::UtcNow -lt $deadline) {
    if ($yien.HasExited) { throw "yien exited with code $($yien.ExitCode) before test control was ready; see $data\logs" }
    try {
        $state = Get-YienState -Name $Name -ServerPid $yien.Id
        $ready = $state.tabs.Count -gt 0
    }
    catch { $ready = $false }
    if (-not $ready) { Start-Sleep -Milliseconds 250 }
}
if (-not $ready) { throw "test control '$Name' did not answer with a tab within $ReadyTimeoutSec s" }

[pscustomobject]$run
