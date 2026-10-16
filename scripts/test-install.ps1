# Offline installation checks. Run with Windows PowerShell 5.1 or PowerShell 7 on Windows.
[CmdletBinding()]
param([string]$Installer = (Join-Path $PSScriptRoot 'install.ps1'))
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$taskTemp = Join-Path ([IO.Path]::GetTempPath()) ('ylcc-install-test-' + [Guid]::NewGuid().ToString('N'))
$release = Join-Path $taskTemp 'release'
$prefix = Join-Path $taskTemp "prefix with spaces and ' quote"
$archive = 'ylcc-windows-x86_64.zip'
[IO.Directory]::CreateDirectory($release) | Out-Null
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function New-Fixture([string]$Variant = 'valid') {
    $path = Join-Path $release $archive
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
    $zip = [IO.Compression.ZipFile]::Open($path, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in @('ylcc.exe', 'LICENSE', 'THIRD_PARTY_NOTICES.txt')) {
            $entry = $zip.CreateEntry($name)
            if ($name -eq 'ylcc.exe' -and $Variant -eq 'symlink') { $entry.ExternalAttributes = -1577123840 }
            if ($name -eq 'ylcc.exe' -and $Variant -eq 'directory') { $entry.ExternalAttributes = 0x10 }
            $writer = New-Object IO.StreamWriter($entry.Open())
            try { $writer.Write("fixture:$name") } finally { $writer.Dispose() }
        }
        switch ($Variant) {
            'extra' { $zip.CreateEntry('extra') | Out-Null }
            'duplicate' { $zip.CreateEntry('ylcc.exe') | Out-Null }
            'traversal' { $zip.CreateEntry('../escaped') | Out-Null }
            'case' { $zip.CreateEntry('YLCC.EXE') | Out-Null }
        }
    } finally { $zip.Dispose() }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText((Join-Path $release 'SHA256SUMS'), "$hash  $archive`n")
}
# This intercepts downloads, keeping the actual installer and its ZIP/hash handling under test.
function Invoke-WebRequest {
    param([switch]$UseBasicParsing, [string]$Uri, [string]$OutFile)
    $base = 'https://github.com/semanticist21/your-local-cloud-computing/releases/download/v0.3.0/'
    Assert ($Uri.StartsWith($base)) "Unexpected release URL: $Uri"
    [IO.File]::Copy((Join-Path $release $Uri.Substring($base.Length)), $OutFile, $true)
}
function Assert-Rejected([string]$Variant) {
    $failed = $false
    try { & $Installer -Version v0.3.0 -Prefix $prefix } catch { $failed = $true }
    Assert $failed "Unsafe fixture accepted: $Variant"
    Assert ([IO.File]::ReadAllText((Join-Path $prefix 'bin\ylcc.exe')) -eq 'fixture:ylcc.exe') "Existing executable changed: $Variant"
    Assert (-not (Test-Path -LiteralPath (Join-Path $taskTemp 'escaped'))) 'Archive traversal escaped the staging directory.'
}
try {
    New-Fixture
    & $Installer -Version v0.3.0 -Prefix $prefix
    Assert ([IO.File]::ReadAllText((Join-Path $prefix 'bin\ylcc.exe')) -eq 'fixture:ylcc.exe') 'Executable did not install.'
    Assert ([IO.File]::ReadAllText((Join-Path $prefix 'share\licenses\ylcc\LICENSE')) -eq 'fixture:LICENSE') 'License did not install.'
    & $Installer -Version v0.3.0 -Prefix $prefix
    [IO.File]::AppendAllText((Join-Path $release $archive), 'corruption')
    Assert-Rejected 'checksum mismatch'
    foreach ($variant in @('extra', 'duplicate', 'traversal', 'case', 'symlink', 'directory')) {
        New-Fixture $variant
        Assert-Rejected $variant
    }
    New-Fixture
    $manifest = Join-Path $release 'SHA256SUMS'
    [IO.File]::AppendAllText($manifest, [IO.File]::ReadAllText($manifest))
    Assert-Rejected 'duplicate checksum'
    Write-Host 'Windows installer checks passed: checksum, exact members, links, traversal, spaces, replacement, and license notices.'
} finally {
    Remove-Item -LiteralPath $taskTemp -Recurse -Force
}
