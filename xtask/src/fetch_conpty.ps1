$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$archive = $env:YIEN_CONPTY_ARCHIVE
$partial = "$archive.partial"
if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
    Remove-Item -LiteralPath $partial -Force -ErrorAction SilentlyContinue
    Invoke-WebRequest -Uri $env:YIEN_CONPTY_URL -OutFile $partial -UserAgent 'YIEN build packaging'
    Move-Item -LiteralPath $partial -Destination $archive
}

$actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
if (-not $actual.Equals($env:YIEN_CONPTY_SHA256, [StringComparison]::OrdinalIgnoreCase)) {
    throw "ConPTY package SHA-256 mismatch: expected $env:YIEN_CONPTY_SHA256, got $actual"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($archive)

function Extract-PackageEntry([string]$entryName, [string]$fileName) {
    $entry = $zip.GetEntry($entryName)
    if ($null -eq $entry -or $entry.Length -eq 0) {
        throw "ConPTY package entry missing or empty: $entryName"
    }
    $destination = Join-Path $env:YIEN_CONPTY_PACKAGE_DIR $fileName
    $temporary = "$destination.partial"
    $entryStream = $entry.Open()
    try {
        $outputStream = [IO.File]::Create($temporary)
        try { $entryStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
    } finally {
        $entryStream.Dispose()
    }
    Move-Item -LiteralPath $temporary -Destination $destination -Force
}

try {
    Extract-PackageEntry $env:YIEN_CONPTY_DLL_ENTRY 'conpty.dll'
    Extract-PackageEntry $env:YIEN_CONPTY_HOST_ENTRY 'OpenConsole.exe'
} finally {
    $zip.Dispose()
}

Write-Output "SHA256=$actual"
