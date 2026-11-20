# SPDX-License-Identifier: GPL-3.0-or-later
#
# Shared helpers for the scripts/ entry points. Dot-source it, never
# run it. Linux equivalents live in lib.sh.

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot

# Keep in step with android/app/build.gradle.kts and
# docs/DEVELOPMENT.md ("Android native library").
$NdkVersion = '28.2.13676358'
$CompileSdk = '35'
$BuildTools = '35.0.0'

# The same pinned build the CI Windows job downloads (ADR-014).
$FfmpegVersion = '9.0.2'
$FfmpegFolder = "ffmpeg-$FfmpegVersion-full_build-shared"
$FfmpegUrl = "https://github.com/GyanD/codexffmpeg/releases/download/$FfmpegVersion/$FfmpegFolder.zip"

$ProtocReleaseApi = 'https://api.github.com/repos/protocolbuffers/protobuf/releases/latest'

function Step([string]$Message) { Write-Host "`n==> $Message" }
function Ok([string]$Message) { Write-Host "    $Message" }
function Warn([string]$Message) { Write-Warning $Message }
function Fail([string]$Message) {
    Write-Host "error: $Message" -ForegroundColor Red
    exit 1
}

function Have([string]$Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# Asks before a system change. -Yes skips the prompt.
function Confirm([string]$Message) {
    if ($script:AssumeYes) { return $true }
    $answer = Read-Host "$Message [y/N]"
    return ($answer -match '^(?i)y(es)?$')
}

function Assert-Exit([string]$What) {
    if ($LASTEXITCODE -ne 0) {
        Fail "$What failed (exit $LASTEXITCODE)"
    }
}

function Add-SessionPath([string]$Dir) {
    $parts = @($env:PATH -split ';' | Where-Object { $_ })
    if ($parts -notcontains $Dir) {
        $env:PATH = "$Dir;$env:PATH"
    }
}

# Picks up PATH changes made by an installer in this same session.
function Refresh-Path {
    $entries = @(
        [Environment]::GetEnvironmentVariable('Path', 'Machine'),
        [Environment]::GetEnvironmentVariable('Path', 'User')
    ) -join ';'
    foreach ($entry in ($entries -split ';')) {
        if ($entry) { Add-SessionPath $entry }
    }
}

function Add-UserPath([string]$Dir) {
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($current -split ';' | Where-Object { $_ })
    if ($parts -notcontains $Dir) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $Dir) -join ';'), 'User')
        Ok "added $Dir to the user PATH (new terminals only)"
    }
}

function Get-AndroidSdk {
    $candidates = @($env:ANDROID_HOME, $env:ANDROID_SDK_ROOT)
    if ($env:LOCALAPPDATA) {
        $candidates += Join-Path (Join-Path $env:LOCALAPPDATA 'Android') 'Sdk'
    }
    foreach ($dir in $candidates) {
        if ($dir -and (Test-Path $dir)) { return $dir }
    }
    return $null
}

function Get-SdkManager([string]$Sdk) {
    $candidates = @(
        (Join-Path (Join-Path (Join-Path $Sdk 'cmdline-tools') 'latest') (Join-Path 'bin' 'sdkmanager.bat')),
        (Join-Path (Join-Path $Sdk 'cmdline-tools') (Join-Path 'bin' 'sdkmanager.bat')),
        (Join-Path (Join-Path $Sdk 'tools') (Join-Path 'bin' 'sdkmanager.bat'))
    )
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { return $candidate }
    }
    if (Have 'sdkmanager') { return 'sdkmanager' }
    return $null
}

# True when $Dir is an FFmpeg install with the headers the crate needs.
function Test-FfmpegDir([string]$Dir) {
    if (-not $Dir) { return $false }
    return (Test-Path (Join-Path $Dir 'include/libavcodec/avcodec.h'))
}

# True when $Dir holds the libclang bindgen loads.
function Test-LibclangDir([string]$Dir) {
    if (-not $Dir) { return $false }
    return ((Test-Path (Join-Path $Dir 'clang.dll')) -or (Test-Path (Join-Path $Dir 'libclang.dll')))
}

# The directory holding clang.dll/libclang.dll: LIBCLANG_PATH, LLVM's
# default install location, whatever clang/llvm-config resolves to,
# winget's per-package folder or the uninstall registry entry.
function Find-LibclangDir {
    $candidates = New-Object System.Collections.Generic.List[string]
    if ($env:LIBCLANG_PATH) { $candidates.Add($env:LIBCLANG_PATH) }
    foreach ($base in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if ($base) { $candidates.Add((Join-Path (Join-Path $base 'LLVM') 'bin')) }
    }
    foreach ($tool in @('clang', 'llvm-config')) {
        if (Have $tool) {
            $candidates.Add((Split-Path -Parent (Get-Command $tool).Source))
        }
    }
    if ($env:LOCALAPPDATA) {
        # Forward slashes: PowerShell accepts them on Windows too, so
        # this branch stays testable on any OS.
        $winget = Join-Path $env:LOCALAPPDATA 'Microsoft/WinGet/Packages'
        if (Test-Path $winget) {
            Get-ChildItem -Path $winget -Filter 'LLVM.LLVM*' -Directory -ErrorAction SilentlyContinue |
                ForEach-Object {
                    foreach ($name in @('clang.dll', 'libclang.dll')) {
                        Get-ChildItem -Path $_.FullName -Filter $name -Recurse -Depth 3 -ErrorAction SilentlyContinue |
                            ForEach-Object { $candidates.Add($_.DirectoryName) }
                    }
                }
        }
    }
    foreach ($hive in @(
            'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
        )) {
        if (-not (Test-Path $hive)) { continue }
        Get-ChildItem $hive -ErrorAction SilentlyContinue | ForEach-Object {
            $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
            if ($props.DisplayName -like 'LLVM*' -and $props.InstallLocation) {
                $candidates.Add((Join-Path $props.InstallLocation 'bin'))
            }
        }
    }
    foreach ($dir in $candidates) {
        if (Test-LibclangDir $dir) { return $dir }
    }
    return $null
}

# Points LIBCLANG_PATH at libclang. Without it bindgen panics with
# "Unable to find libclang" while ffmpeg-sys-next builds.
function Resolve-Libclang {
    $dir = Find-LibclangDir
    if (-not $dir) {
        Fail 'libclang not found (bindgen needs it); run scripts\setup.ps1, or install LLVM (winget install LLVM.LLVM) and set LIBCLANG_PATH to its bin directory'
    }
    $env:LIBCLANG_PATH = $dir
    Add-SessionPath $dir
    Ok "LIBCLANG_PATH=$dir"
}

# Points FFMPEG_DIR at an existing dev build: the environment, a
# previous setup download, or vcpkg. The crate cannot build without
# the headers, so a miss is fatal here with the fix in the message.
function Resolve-Ffmpeg {
    $local = Join-Path (Join-Path (Join-Path $Root 'tools') 'ffmpeg') $FfmpegFolder
    if (Test-FfmpegDir $env:FFMPEG_DIR) {
        $dir = $env:FFMPEG_DIR
    }
    elseif (Test-FfmpegDir $local) {
        $dir = $local
    }
    else {
        Fail 'FFmpeg dev build not found; run scripts\setup.ps1 or set FFMPEG_DIR to the extracted shared build'
    }
    $env:FFMPEG_DIR = $dir
    Add-SessionPath (Join-Path $dir 'bin')
    Ok "FFMPEG_DIR=$dir"
}

# Fails with the missing piece when the Android side cannot build.
# test.ps1 and rebuild.ps1 call this: the Android half is required.
function Require-Android {
    $sdk = Get-AndroidSdk
    if (-not $sdk) {
        Fail 'Android SDK not found; set ANDROID_HOME or run scripts\setup.ps1'
    }
    $env:ANDROID_HOME = $sdk
    $env:ANDROID_SDK_ROOT = $sdk

    if (-not (Have 'java')) {
        Fail 'JDK not found (Java 17+); install one or set JAVA_HOME'
    }
    # cmd /c keeps java's stderr out of PowerShell's error stream, which
    # would otherwise trip $ErrorActionPreference = 'Stop' on 5.1.
    $line = (cmd /c 'java -version 2>&1' | Select-Object -First 1)
    if ($line -notmatch '"(\d+)') {
        Fail "cannot parse 'java -version' output: $line"
    }
    if ([int]$Matches[1] -lt 17) {
        Fail "Java 17+ required (found $line)"
    }

    if (-not (Test-Path (Join-Path (Join-Path $sdk 'ndk') $NdkVersion))) {
        Fail "NDK $NdkVersion missing in $sdk; run scripts\setup.ps1"
    }
    if (-not (Have 'cargo-ndk')) {
        Fail 'cargo-ndk missing; cargo install cargo-ndk'
    }
    $targets = & rustup target list --installed 2>$null
    if ($targets -notcontains 'aarch64-linux-android') {
        Fail 'Android Rust target missing; rustup target add aarch64-linux-android'
    }
}

function Install-CargoTool([string]$Binary, [string]$Crate) {
    if (Have $Binary) {
        Ok "$Binary already installed"
        return
    }
    if (-not (Confirm "Install $Crate with cargo install?")) { Fail 'aborted' }
    cargo install $Crate
    Assert-Exit "cargo install $Crate"
}
