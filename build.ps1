# Build a local Windows Ruffle desktop package (Windows 7 x86 by default).
#
# Usage:
#   .\build.ps1              # cargo --profile dist for i686-win7-windows-msvc
#   .\build.ps1 -Release     # cargo --release instead of dist
#   .\build.ps1 -Open        # run ruffle.exe after building
#   .\build.ps1 -Zip         # also create package\Ruffle-windows-x86_32-win7.zip
#   .\build.ps1 -X64         # build x86_64-win7-windows-msvc instead
#   .\build.ps1 -Modern      # use *-pc-windows-msvc (Win10+, no build-std)
#
# Win7 targets are Tier 3 and need a nightly toolchain with rust-src
# plus -Zbuild-std. Install Visual Studio Build Tools with the
# "MSVC v143 - VS 2022 C++ x86/x64 build tools" and Windows SDK workloads.
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$Dist,
    [switch]$Open,
    [switch]$Zip,
    [switch]$X64,
    [switch]$X86,
    [switch]$Modern,
    [switch]$Win7,
    [switch]$Help
)

$ErrorActionPreference = "Stop"

if ($Help) {
    @(
        "Build a local Windows Ruffle desktop package (Windows 7 x86 by default).",
        "",
        "Usage:",
        "  .\build.ps1              # cargo --profile dist for i686-win7-windows-msvc",
        "  .\build.ps1 -Release     # cargo --release instead of dist",
        "  .\build.ps1 -Open        # run ruffle.exe after building",
        "  .\build.ps1 -Zip         # also create package\Ruffle-windows-x86_32-win7.zip",
        "  .\build.ps1 -X64         # build x86_64-win7-windows-msvc instead",
        "  .\build.ps1 -Modern      # use *-pc-windows-msvc (Win10+, no build-std)",
        "",
        "Requires: rustup, nightly+rust-src (Win7), MSVC x86/x64 tools, Windows SDK."
    ) | Write-Output
    exit 0
}

$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

$ProfileName = if ($Release) { "release" } else { "dist" }
if ($Dist) { $ProfileName = "dist" }

$WinArch = if ($X64) { "x64" } else { "x86" }
if ($X86) { $WinArch = "x86" }

$WinModern = [bool]$Modern
if ($Win7) { $WinModern = $false }

function Require-Command([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command not found: $Name"
    }
}

function Ensure-Win7Toolchain {
    Require-Command rustup
    Write-Host "==> Ensuring nightly toolchain with rust-src"
    & rustup toolchain install nightly --profile minimal --component rust-src
    if ($LASTEXITCODE -ne 0) {
        throw "rustup toolchain install failed with exit code $LASTEXITCODE"
    }
}

function Resolve-Target {
    if ($WinModern) {
        if ($WinArch -eq "x64") {
            return @{
                Target    = "x86_64-pc-windows-msvc"
                BuildName = "windows-x86_64"
                BuildStd  = $false
            }
        }
        return @{
            Target    = "i686-pc-windows-msvc"
            BuildName = "windows-x86_32"
            BuildStd  = $false
        }
    }

    if ($WinArch -eq "x64") {
        return @{
            Target    = "x86_64-win7-windows-msvc"
            BuildName = "windows-x86_64-win7"
            BuildStd  = $true
        }
    }

    return @{
        Target    = "i686-win7-windows-msvc"
        BuildName = "windows-x86_32-win7"
        BuildStd  = $true
    }
}

Require-Command cargo

$Resolved = Resolve-Target
$Target = $Resolved.Target
$BuildName = $Resolved.BuildName
$UseBuildStd = $Resolved.BuildStd

if ($UseBuildStd) {
    Ensure-Win7Toolchain
}

$BinSrc = Join-Path $Root "target\$Target\$ProfileName\ruffle_desktop.exe"
$OutDir = Join-Path $Root "package"
$OutExe = Join-Path $OutDir "ruffle.exe"

Write-Host "==> Building ruffle_desktop (profile=$ProfileName target=$Target)"

# Static CRT matches official Windows release builds and avoids VCRUNTIME DLLs.
$prevRustflags = $env:RUSTFLAGS
if ([string]::IsNullOrWhiteSpace($prevRustflags)) {
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
} elseif ($prevRustflags -notlike "*crt-static*") {
    $env:RUSTFLAGS = "$prevRustflags -C target-feature=+crt-static"
}

$cargoArgs = @(
    "build",
    "--package", "ruffle_desktop",
    "--profile", $ProfileName,
    "--target", $Target
)

try {
    if ($UseBuildStd) {
        # dist/release use panic=abort; include panic_abort when rebuilding std.
        $cargoArgs += @("-Z", "build-std=std,panic_abort")
        & cargo +nightly @cargoArgs
    } else {
        & cargo @cargoArgs
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
} finally {
    $env:RUSTFLAGS = $prevRustflags
}

if (-not (Test-Path $BinSrc)) {
    throw "Missing binary: $BinSrc"
}

Write-Host "==> Assembling $OutDir"
if (Test-Path $OutDir) {
    Remove-Item -Recurse -Force $OutDir
}
New-Item -ItemType Directory -Path $OutDir | Out-Null
Copy-Item $BinSrc $OutExe
Copy-Item (Join-Path $Root "README.md") (Join-Path $OutDir "README.md")
Copy-Item (Join-Path $Root "LICENSE.md") (Join-Path $OutDir "LICENSE.md")

if ($Zip) {
    $Archive = Join-Path $OutDir "Ruffle-$BuildName.zip"
    Write-Host "==> Creating $Archive"
    $zipInputs = @(
        (Join-Path $OutDir "ruffle.exe"),
        (Join-Path $OutDir "README.md"),
        (Join-Path $OutDir "LICENSE.md")
    )
    if (Test-Path $Archive) {
        Remove-Item -Force $Archive
    }
    Compress-Archive -Path $zipInputs -DestinationPath $Archive
}

Write-Host ""
Write-Host "Done: $OutExe"
Write-Host "Run:  `"$OutExe`" C:\path\to\file.swf"
if ($UseBuildStd) {
    Write-Host "Note: binary targets Windows 7+ ($Target)."
}

if ($Open) {
    Start-Process -FilePath $OutExe
}
