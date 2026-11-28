<#
.SYNOPSIS
Inventory and compliance check for Rustket on Windows.

.DESCRIPTION
Finds the copy installed by the Rustket installer (for all users, then for
the user that runs the script), reports its version and, when -Key is given,
whether that policy key is applied, from Rustket's own --dump-policy report.

Exit code 0 when Rustket 0.4.0 or later is installed and the key (if given)
is applied; 1 otherwise. A summary line goes to standard output in both
cases, so the script works as an Intune detection or remediation-detection
script and as a compliance check of other tools. Intune passes no
parameters to such scripts: set the default of -Key in the param block of
the copy you upload, for example [string]$Key = 'RequireHttps'.

Run as SYSTEM it sees the computer's policy (HKLM) and SYSTEM's own user
policy, not the signed-in user's. Writes only a temporary file. Works in
Windows PowerShell 5.1 (also as a 32-bit process) and PowerShell 7.

.PARAMETER Key
A policy key, for example RequireHttps. Without it only the installation and
the version are checked.

.PARAMETER Exe
The path of rustket.exe, for a copy the installer did not install. Run as
SYSTEM or an administrator, the script starts this file, and Windows loads
DLLs from its folder. Use only a folder that only administrators can change
(not a portable copy in a user's profile). To check such a copy, run the
script as that user.

.EXAMPLE
powershell.exe -NoProfile -ExecutionPolicy Bypass -File rustket-policy-check.ps1 -Key AllowedServers
#>
param(
    # Tools that pass no parameters (Intune): put the key here.
    [string]$Key = '',
    [string]$Exe = ''
)
$ErrorActionPreference = 'Stop'

function Finish([int]$Code, [string]$Line) {
    Write-Output $Line
    exit $Code
}

# The installer's uninstall key (its AppId plus "_is1"), read in the 64-bit
# view so a 32-bit host finds it too.
$AppKey = 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{77810904-1236-4291-B123-CCC8D0A6CCF1}_is1'
if (-not $Exe) {
    foreach ($hive in 'LocalMachine', 'CurrentUser') {
        $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, [Microsoft.Win32.RegistryView]::Registry64)
        $uninstall = $base.OpenSubKey($AppKey)
        if ($uninstall) {
            $dir = [string]$uninstall.GetValue('InstallLocation')
            $uninstall.Close()
            if ($dir) {
                $Exe = Join-Path $dir 'rustket.exe'
                break
            }
        }
    }
}
if (-not $Exe -or -not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    Finish 1 'Rustket not installed'
}

# The executable's version resource carries the release version (X.Y.Z).
$text = (Get-Item -LiteralPath $Exe).VersionInfo.ProductVersion
$version = $null
if (-not ($text -match '^\d+\.\d+\.\d+') -or -not [version]::TryParse($Matches[0], [ref]$version)) {
    Finish 1 "Rustket at ${Exe}: cannot read the version"
}
# Policy exists from 0.4.0. Older copies ignore --dump-policy and open their
# window, so they are never asked for the report.
if ($version -lt [version]'0.4.0') {
    Finish 1 "Rustket $version at ${Exe}: older than 0.4.0, reads no policy"
}
if (-not $Key) {
    Finish 0 "Rustket $version at $Exe"
}

# The Windows build has no console: the report goes to a file, and
# Start-Process -Wait waits for the process to end.
$report = [System.IO.Path]::GetTempFileName()
try {
    $process = Start-Process -FilePath $Exe -ArgumentList "--dump-policy `"$report`"" -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -ne 0) {
        Finish 1 "Rustket ${version}: --dump-policy failed (exit code $($process.ExitCode))"
    }
    try {
        $dump = Get-Content -LiteralPath $report -Raw -Encoding UTF8 | ConvertFrom-Json
    } catch {
        Finish 1 "Rustket ${version}: cannot read the policy report: $_"
    }
} finally {
    Remove-Item -LiteralPath $report -Force -ErrorAction SilentlyContinue
}

$rows = @($dump.rows | Where-Object { $_.key -eq $Key })
$applied = @($rows | Where-Object { $_.status -eq 'applied' })
$blocked = if ($dump.blocked) { ' (blocked)' } else { '' }
if ($applied.Count -gt 0) {
    Finish 0 "Rustket ${version}: $Key applied: $($applied[0].value) from $($applied[0].source)$blocked"
} elseif ($rows.Count -gt 0) {
    Finish 1 "Rustket ${version}: $Key $($rows[0].status): $($rows[0].value) from $($rows[0].source)$blocked"
} else {
    Finish 1 "Rustket ${version}: $Key not set$blocked"
}
