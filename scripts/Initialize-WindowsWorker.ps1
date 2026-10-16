# One-time initialization of a dedicated Windows worker volume; never the steady-state entrypoint.
[CmdletBinding()]
param([string]$SourceToken = 'C:\ylcc-bootstrap\pool.token')
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run this one-time initializer as ContainerAdministrator.' }
$containerUser = (New-Object Security.Principal.NTAccount('ContainerUser')).Translate([Security.Principal.SecurityIdentifier])
$system = New-Object Security.Principal.SecurityIdentifier([Security.Principal.WellKnownSidType]::LocalSystemSid, $null)
$admins = New-Object Security.Principal.SecurityIdentifier([Security.Principal.WellKnownSidType]::BuiltinAdministratorsSid, $null)
$root = 'C:\ylcc-data'
function Assert-PlainPath([string]$Path, [bool]$Directory) {
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.PSIsContainer -ne $Directory) { throw "Expected a plain path without reparse points: $Path" }
}
function New-PrivateAcl([bool]$Directory) {
    if ($Directory) { $acl = New-Object Security.AccessControl.DirectorySecurity }
    else { $acl = New-Object Security.AccessControl.FileSecurity }
    $acl.SetOwner($admins)
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in @($containerUser, $system, $admins)) {
        if ($Directory) {
            $rule = New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        } else {
            $rule = New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'Allow')
        }
        $acl.AddAccessRule($rule)
    }
    return $acl
}
Assert-PlainPath $root $true
foreach ($directory in @((Join-Path $root 'config'), (Join-Path $root 'worker'))) {
    if (Test-Path -LiteralPath $directory) { Assert-PlainPath $directory $true }
}
Assert-PlainPath $SourceToken $false
$bytes = [IO.File]::ReadAllBytes($SourceToken)
$token = (New-Object Text.UTF8Encoding($false, $true)).GetString($bytes).Trim()
if ($token.Length -lt 32 -or $token -match '\s' -or $token.IndexOf([char]0) -ge 0) { throw 'The pool token must contain at least 32 non-whitespace characters.' }
$destination = Join-Path $root 'config\token'
if (Test-Path -LiteralPath $destination) {
    Assert-PlainPath $destination $false
    $existing = (New-Object Text.UTF8Encoding($false, $true)).GetString([IO.File]::ReadAllBytes($destination)).Trim()
    if ($existing -cne $token) { throw 'The volume already contains a different token; refusing to replace it.' }
    $links = @(& fsutil.exe hardlink list $destination 2>$null)
    if ($LASTEXITCODE -ne 0 -or $links.Count -ne 1) { throw 'The existing token must be a regular file with one hard link.' }
}
foreach ($directory in @($root, (Join-Path $root 'config'), (Join-Path $root 'worker'))) {
    if (-not (Test-Path -LiteralPath $directory)) { [IO.Directory]::CreateDirectory($directory) | Out-Null }
    Assert-PlainPath $directory $true
    Set-Acl -LiteralPath $directory -AclObject (New-PrivateAcl $true)
}
$fileAcl = New-PrivateAcl $false
if (Test-Path -LiteralPath $destination) {
    Set-Acl -LiteralPath $destination -AclObject $fileAcl
} else {
    $file = New-Object IO.FileStream($destination, [IO.FileMode]::CreateNew, [Security.AccessControl.FileSystemRights]::Write, [IO.FileShare]::None, 4096, [IO.FileOptions]::None, $fileAcl)
    try { $file.Write($bytes, 0, $bytes.Length); $file.Flush($true) } finally { $file.Dispose() }
}
Write-Host 'Initialized the dedicated Windows worker volume. Run the worker as ContainerUser and remove the bootstrap token mount.'
