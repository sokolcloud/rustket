# CI helper (GitHub Actions Windows runners): installs the stable Rust
# toolchain with clippy and checks the C toolchain the native dependencies
# need. Installs rustup first when the runner image has none.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# RUNNER_ARCH is reliable even if this shell runs under x64 emulation.
$arm64 = ($env:RUNNER_ARCH -eq 'ARM64') -or ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64')
$triple = if ($arm64) { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    $init = Join-Path $env:RUNNER_TEMP 'rustup-init.exe'
    Invoke-WebRequest "https://static.rust-lang.org/rustup/dist/$triple/rustup-init.exe" -OutFile $init
    & $init -y --no-modify-path --profile minimal --default-toolchain none
    if ($LASTEXITCODE -ne 0) { throw 'rustup-init failed' }
    $bin = Join-Path $env:USERPROFILE '.cargo\bin'
    Add-Content -Path $env:GITHUB_PATH -Value $bin
    $env:PATH = "$bin;$env:PATH"
}
# The host triple is explicit so an emulated x64 toolchain is never the default
# on the ARM64 runner.
rustup toolchain install "stable-$triple" --profile minimal --component clippy
if ($LASTEXITCODE -ne 0) { throw 'rustup toolchain install failed' }
rustup default "stable-$triple"
if ($LASTEXITCODE -ne 0) { throw 'rustup default failed' }
rustc --version --verbose

# aws-lc-sys compiles C for aarch64-pc-windows-msvc only with clang-cl, taken
# from PATH or from Visual Studio's "C++ Clang tools" (VC\Tools\Llvm).
if ($arm64) {
    $clang = Get-Command clang-cl -ErrorAction SilentlyContinue | ForEach-Object Source
    if (-not $clang) {
        $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        $vs = & $vswhere -latest -products * -property installationPath
        $clang = @('ARM64', 'x64') | ForEach-Object { Join-Path $vs "VC\Tools\Llvm\$_\bin\clang-cl.exe" } |
            Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if (-not $clang) { throw 'clang-cl not found (needed by aws-lc-sys on Windows ARM64)' }
    Write-Output "clang-cl: $clang"
}
