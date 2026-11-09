# Builds the Windows packages into packaging/out:
#   Rustket-<version>-<arch>-setup.exe    Inno Setup installer (rustket.iss)
#   rustket-<version>-windows-<arch>.zip  portable: Rustket\rustket.exe, LICENSE.txt,
#                                         THIRD-PARTY-NOTICES.txt
#
#   pwsh packaging/windows/build.ps1 -Arch x64
#   pwsh packaging/windows/build.ps1 -Arch arm64 -SkipBuild   # reuse target\<triple>\dist\rustket.exe
#
# Needs: Rust (with the target's std), MSVC build tools, Python 3 (`python`),
# Inno Setup 6.3+ (ISCC.exe on PATH or in its default folder). arm64 also needs
# clang-cl (aws-lc-sys); on x64 without NASM, aws-lc-sys uses its prebuilt
# NASM objects. Environment: CARGO_TARGET_DIR, RUSTKET_PACKAGE_OUT, ISCC.
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('x64', 'arm64')]
    [string]$Arch,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Triple = @{ x64 = 'x86_64-pc-windows-msvc'; arm64 = 'aarch64-pc-windows-msvc' }[$Arch]
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
$Out = if ($env:RUSTKET_PACKAGE_OUT) { $env:RUSTKET_PACKAGE_OUT } else { Join-Path $Root 'packaging\out' }
$Exe = Join-Path $TargetDir "$Triple\dist\rustket.exe"

# [workspace.package] version; release tags are v<version>.
$Version = (Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^version = "([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
# File version resources take four numbers: drop a pre-release/build suffix.
$NumericVersion = ($Version -replace '[-+].*$', '') + '.0'

function Invoke-Checked([string]$What, [scriptblock]$Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$What failed (exit code $LASTEXITCODE)" }
}

if (-not $SkipBuild) {
    if ($Arch -eq 'x64' -and -not $env:AWS_LC_SYS_PREBUILT_NASM -and -not (Get-Command nasm -ErrorAction SilentlyContinue)) {
        $env:AWS_LC_SYS_PREBUILT_NASM = '1'
    }
    Push-Location $Root
    try {
        Invoke-Checked 'cargo build' { cargo build --locked --profile dist -p rustket --bin rustket --target $Triple }
    } finally {
        Pop-Location
    }
}
if (-not (Test-Path $Exe)) { throw "$Exe not found (build with: cargo build --profile dist -p rustket --target $Triple)" }

$Stage = Join-Path $Out "windows-$Arch"
$AppDir = Join-Path $Stage 'Rustket'
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force $AppDir | Out-Null
Copy-Item $Exe (Join-Path $AppDir 'rustket.exe')
Copy-Item (Join-Path $Root 'LICENSE') (Join-Path $AppDir 'LICENSE.txt')
Invoke-Checked 'notices.py' {
    python -I (Join-Path $Root 'packaging\notices.py') $Triple -o (Join-Path $AppDir 'THIRD-PARTY-NOTICES.txt')
}

$Zip = Join-Path $Out "rustket-$Version-windows-$Arch.zip"
if (Test-Path $Zip) { Remove-Item -Force $Zip }
Compress-Archive -Path $AppDir -DestinationPath $Zip
Write-Output $Zip

$Iscc = $env:ISCC
if (-not $Iscc) {
    $Iscc = @(
        (Get-Command iscc -ErrorAction SilentlyContinue | ForEach-Object Source),
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $Iscc) { throw 'ISCC.exe (Inno Setup 6) not found; set ISCC or install Inno Setup' }
Invoke-Checked 'ISCC' {
    & $Iscc /Qp "/DAppVersion=$Version" "/DAppNumericVersion=$NumericVersion" "/DAppArch=$Arch" `
        "/DSourceDir=$AppDir" "/DOutputDir=$Out" (Join-Path $PSScriptRoot 'rustket.iss')
}
$Setup = Join-Path $Out "Rustket-$Version-$Arch-setup.exe"
if (-not (Test-Path $Setup)) { throw "ISCC did not produce $Setup" }
Write-Output $Setup
