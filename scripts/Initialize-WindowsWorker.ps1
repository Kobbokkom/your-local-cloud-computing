# One-time initialization of a dedicated Windows worker volume; never the steady-state entrypoint.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^S-1-[0-9]+(?:-[0-9]+)+$')]
    [string]$WorkerSid,
    [string]$SourceToken = 'C:\ylcc-bootstrap\pool.token'
)
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run this one-time initializer as ContainerAdministrator.' }
$containerUser = New-Object Security.Principal.SecurityIdentifier($WorkerSid)
if ($containerUser.Equals($identity.User)) { throw 'WorkerSid must identify the unprivileged ContainerUser, not the initializer account.' }
foreach ($kind in @('LocalSystemSid', 'BuiltinAdministratorsSid', 'WorldSid', 'AnonymousSid', 'AuthenticatedUserSid', 'BuiltinUsersSid')) {
    if ($containerUser.IsWellKnown([Security.Principal.WellKnownSidType]::$kind)) { throw 'WorkerSid must identify the unprivileged ContainerUser, not a privileged account or shared group.' }
}
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
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class YlccTokenFileInfo {
    [StructLayout(LayoutKind.Sequential)]
    private struct Information {
        public uint Attributes;
        public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime, LastAccessTime, LastWriteTime;
        public uint VolumeSerialNumber, FileSizeHigh, FileSizeLow, NumberOfLinks, FileIndexHigh, FileIndexLow;
    }
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFileInformationByHandle(SafeFileHandle file, out Information information);
    public static uint LinkCount(SafeFileHandle file) {
        Information information;
        if (!GetFileInformationByHandle(file, out information))
            throw new Win32Exception(Marshal.GetLastWin32Error());
        if ((information.Attributes & 0x410) != 0)
            throw new InvalidOperationException("The token must not be a directory or reparse point.");
        return information.NumberOfLinks;
    }
}
'@
    $existingFile = New-Object IO.FileStream($destination, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ([YlccTokenFileInfo]::LinkCount($existingFile.SafeFileHandle) -ne 1) { throw 'The existing token must be a regular file with one hard link.' }
    } finally { $existingFile.Dispose() }
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
