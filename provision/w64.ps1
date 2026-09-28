# Provision a Windows machine for row W64: MSYS2 with the UCRT64 environment, gcc as the reference,
# and meson as the build system, which is how Postgres itself builds on MinGW.
#
#   powershell -ExecutionPolicy Bypass -File provision\w64.ps1 [-MsysRoot C:\msys64]
#
# What it does:
#
#   1. installs MSYS2 with winget when it is not already at the given root;
#   2. updates it and installs, in the UCRT64 environment, gcc, meson, ninja, pkgconf and perl,
#      and from the MSYS environment bison, flex, make, diffutils, curl, tar and bzip2;
#   3. installs IPC::Run into the UCRT64 perl with cpan, for the TAP tests.
#
# It creates no user and changes no system setting. Run rpg from the UCRT64 shell
# (C:\msys64\ucrt64.exe), not from PowerShell.
#
# This script has not been run on the W64 machine yet. Treat its first run as a test of it.

param(
    [string]$MsysRoot = "C:\msys64"
)

$ErrorActionPreference = "Stop"

$bash = Join-Path $MsysRoot "usr\bin\bash.exe"
if (-not (Test-Path $bash)) {
    Write-Host "MSYS2 is not at $MsysRoot, installing it with winget"
    winget install --id MSYS2.MSYS2 --exact --accept-source-agreements --accept-package-agreements
    if (-not (Test-Path $bash)) {
        throw "MSYS2 did not appear at $MsysRoot; install it by hand and pass -MsysRoot"
    }
}

function Invoke-Ucrt64([string]$Command) {
    $env:MSYSTEM = "UCRT64"
    $env:CHERE_INVOKING = "1"
    & $bash -lc $Command
    if ($LASTEXITCODE -ne 0) {
        throw "failed in MSYS2: $Command"
    }
}

# The first update can replace the MSYS2 runtime itself and then asks for a restart, so it runs
# twice.
Invoke-Ucrt64 "pacman -Syuu --noconfirm || true"
Invoke-Ucrt64 "pacman -Syuu --noconfirm"

$packages = @(
    "mingw-w64-ucrt-x86_64-gcc",
    "mingw-w64-ucrt-x86_64-meson",
    "mingw-w64-ucrt-x86_64-ninja",
    "mingw-w64-ucrt-x86_64-pkgconf",
    "mingw-w64-ucrt-x86_64-perl",
    "bison",
    "flex",
    "make",
    "diffutils",
    "curl",
    "tar",
    "bzip2",
    "git"
)
Invoke-Ucrt64 ("pacman -S --needed --noconfirm " + ($packages -join " "))

Invoke-Ucrt64 "PERL_MM_USE_DEFAULT=1 cpan -T IPC::Run"
Invoke-Ucrt64 "perl -MIPC::Run -e 'print qq(IPC::Run `$IPC::Run::VERSION\n)'"
Invoke-Ucrt64 "gcc --version | head -1; meson --version; ninja --version"

Write-Host "row W64 is provisioned; run rpg from $MsysRoot\ucrt64.exe"
