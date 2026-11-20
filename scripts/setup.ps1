#Requires -Version 5.1
# SPDX-License-Identifier: GPL-3.0-or-later
#
# One-time development setup for Windows. Installs the Rust toolchain
# pieces, LLVM for bindgen, the pinned FFmpeg shared build, protoc and
# the Android SDK components, then checks that everything is visible.
# Safe to re-run: it only installs what is missing.
#
#   powershell -ExecutionPolicy Bypass -File scripts\setup.ps1 `
#       [-Yes] [-HostOnly] [-Persist] [-AndroidSdk DIR]
#
#   -Yes         do not ask before installing anything
#   -HostOnly    skip the Android SDK, NDK and Rust target (host work)
#   -Persist     write FFMPEG_DIR/LIBCLANG_PATH/PATH/ANDROID_HOME to the
#                user environment
#   -AndroidSdk  use this SDK instead of ANDROID_HOME / %LOCALAPPDATA%

param(
    [switch]$Yes,
    [switch]$HostOnly,
    [switch]$Persist,
    [string]$AndroidSdk
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib.ps1"
$script:AssumeYes = [bool]$Yes

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
    Fail 'setup.ps1 is for Windows; use scripts/setup.sh on Linux'
}

Step 'Checking the Rust toolchain'
if (-not (Have 'cargo')) {
    if (Have 'winget') {
        if (-not (Confirm 'Install Rust with winget (Rustlang.Rustup)?')) { Fail 'aborted' }
        winget install --id Rustlang.Rustup -e --silent --accept-package-agreements --accept-source-agreements
        Refresh-Path
        if ($env:USERPROFILE) {
            Add-SessionPath (Join-Path $env:USERPROFILE '.cargo\bin')
        }
    }
    else {
        Fail 'Rust not found; install it from https://rustup.rs and re-run setup'
    }
}
Ok (& cargo --version)

& rustup component add rustfmt clippy
Assert-Exit 'rustup component add'
if (-not $HostOnly) {
    & rustup target add aarch64-linux-android
    Assert-Exit 'rustup target add'
}

Step 'Checking the Cargo tools'
Install-CargoTool 'cargo-deny' 'cargo-deny'
Install-CargoTool 'cargo-fuzz' 'cargo-fuzz'
if (-not $HostOnly) {
    Install-CargoTool 'cargo-ndk' 'cargo-ndk'
}

Step 'Checking the MSVC toolchain'
$vcTools = $null
if ($env:ProgramFiles -and ${env:ProgramFiles(x86)}) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $vcTools = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    }
}
if ($vcTools) {
    Ok "Visual Studio C++ tools at $vcTools"
}
else {
    Warn 'Visual Studio Build Tools with the C++ workload not found; rustc needs the MSVC linker (https://visualstudio.microsoft.com/visual-cpp-build-tools/)'
}

Step 'Checking LLVM (libclang for bindgen)'
if (-not (Find-LibclangDir)) {
    if (Have 'winget') {
        if (-not (Confirm 'Install LLVM with winget (LLVM.LLVM)? bindgen needs libclang.dll')) { Fail 'aborted' }
        winget install --id LLVM.LLVM -e --silent --accept-package-agreements --accept-source-agreements
        if ($LASTEXITCODE -ne 0) {
            Fail "winget install LLVM.LLVM failed (exit $LASTEXITCODE)"
        }
        Refresh-Path
    }
    else {
        Warn 'LLVM not found and winget unavailable; install LLVM by hand (https://releases.llvm.org/)'
    }
}
Resolve-Libclang
if ($Persist) {
    [Environment]::SetEnvironmentVariable('LIBCLANG_PATH', $env:LIBCLANG_PATH, 'User')
    Add-UserPath $env:LIBCLANG_PATH
}

Step 'Checking FFmpeg'
$localFfmpeg = Join-Path (Join-Path (Join-Path $Root 'tools') 'ffmpeg') $FfmpegFolder
if (Test-FfmpegDir $env:FFMPEG_DIR) {
    Ok "FFMPEG_DIR=$env:FFMPEG_DIR"
    Add-SessionPath (Join-Path $env:FFMPEG_DIR 'bin')
}
elseif (Test-FfmpegDir $localFfmpeg) {
    $env:FFMPEG_DIR = $localFfmpeg
    Add-SessionPath (Join-Path $localFfmpeg 'bin')
    Ok "using $localFfmpeg"
}
else {
    if (-not (Confirm "Download the pinned FFmpeg $FfmpegVersion shared build into tools\ffmpeg?")) { Fail 'aborted' }
    $tools = Join-Path (Join-Path $Root 'tools') 'ffmpeg'
    New-Item -ItemType Directory -Force -Path $tools | Out-Null
    $zip = Join-Path $env:TEMP 'ffmpeg-shared.zip'
    Invoke-WebRequest -Uri $FfmpegUrl -OutFile $zip -UseBasicParsing
    Expand-Archive -Path $zip -DestinationPath $tools -Force
    Remove-Item $zip -Force
    $env:FFMPEG_DIR = $localFfmpeg
    Add-SessionPath (Join-Path $localFfmpeg 'bin')
    Ok "downloaded to $localFfmpeg"
}
if ($Persist) {
    [Environment]::SetEnvironmentVariable('FFMPEG_DIR', $env:FFMPEG_DIR, 'User')
    Add-UserPath (Join-Path $env:FFMPEG_DIR 'bin')
}
else {
    Ok "persist with: [Environment]::SetEnvironmentVariable('FFMPEG_DIR', '$env:FFMPEG_DIR', 'User')"
}

Step 'Checking protoc'
if (-not (Have 'protoc')) {
    if (Have 'winget') {
        winget install --id Google.Protobuf -e --silent --accept-package-agreements --accept-source-agreements
        Refresh-Path
    }
    if (-not (Have 'protoc') -and (Have 'choco')) {
        choco install protoc -y --no-progress
        Refresh-Path
    }
    if (-not (Have 'protoc')) {
        Ok 'falling back to the protoc release zip'
        $dest = Join-Path (Join-Path $Root 'tools') 'protoc'
        $release = Invoke-RestMethod -Uri $ProtocReleaseApi -Headers @{ 'User-Agent' = 'slateway-setup' }
        $asset = $release.assets |
            Where-Object { $_.name -match '^protoc-.*-win64\.zip$' } |
            Select-Object -First 1
        if (-not $asset) { Fail 'could not find a protoc win64 zip in the latest release' }
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        $zip = Join-Path $env:TEMP 'protoc.zip'
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing
        Expand-Archive -Path $zip -DestinationPath $dest -Force
        Remove-Item $zip -Force
        Add-SessionPath (Join-Path $dest 'bin')
        if ($Persist) { Add-UserPath (Join-Path $dest 'bin') }
    }
}
if (-not (Have 'protoc')) {
    Fail 'protoc still not on PATH; install it with "winget install Google.Protobuf" and re-run setup'
}
Ok (& protoc --version)

if ($HostOnly) {
    Warn 'skipping the Android SDK, NDK and Rust target (-HostOnly)'
}
else {
    Step 'Checking the Android SDK'
    if ($AndroidSdk) {
        if (-not (Test-Path $AndroidSdk)) { Fail "-AndroidSdk $AndroidSdk does not exist" }
        $env:ANDROID_HOME = $AndroidSdk
        $env:ANDROID_SDK_ROOT = $AndroidSdk
    }
    $sdk = Get-AndroidSdk
    if (-not $sdk) {
        Fail 'Android SDK not found. Install Android Studio or the command-line tools, then set ANDROID_HOME or pass -AndroidSdk DIR.'
    }
    Ok "SDK at $sdk"
    $env:ANDROID_HOME = $sdk
    $env:ANDROID_SDK_ROOT = $sdk
    if ($Persist) {
        [Environment]::SetEnvironmentVariable('ANDROID_HOME', $sdk, 'User')
        [Environment]::SetEnvironmentVariable('ANDROID_SDK_ROOT', $sdk, 'User')
    }

    $needsPackages = $false
    foreach ($piece in @(
            (Join-Path 'ndk' $NdkVersion),
            (Join-Path 'platforms' "android-$CompileSdk"),
            (Join-Path 'build-tools' $BuildTools)
        )) {
        if (-not (Test-Path (Join-Path $sdk $piece))) { $needsPackages = $true }
    }
    if ($needsPackages) {
        $sdkmanager = Get-SdkManager $sdk
        if (-not $sdkmanager) {
            Fail "sdkmanager not found under $sdk. Install the NDK $NdkVersion, platform android-$CompileSdk and build-tools $BuildTools from Android Studio's SDK Manager, or add the command-line tools and re-run setup."
        }
        if (-not (Confirm 'Accept the Android SDK licenses and install platform-tools, the platform, build-tools and the NDK?')) {
            Fail 'aborted'
        }
        # sdkmanager asks once per license; a handful of y's is enough.
        $yes = 1..20 | ForEach-Object { 'y' }
        $yes | & $sdkmanager --licenses | Out-Null
        & $sdkmanager --install 'platform-tools' "platforms;android-$CompileSdk" "build-tools;$BuildTools" "ndk;$NdkVersion"
        Assert-Exit 'sdkmanager --install'
    }
}

Step 'Verifying'
Ok (& protoc --version)
Ok "FFMPEG_DIR=$env:FFMPEG_DIR"
Ok "LIBCLANG_PATH=$env:LIBCLANG_PATH"
if (Have 'java') {
    Ok (cmd /c 'java -version 2>&1' | Select-Object -First 1)
}
else {
    Warn 'java not found; Gradle needs a JDK 17+ (Android Studio ships one under jbr\)'
}
if (-not $HostOnly) {
    Ok "NDK $(Join-Path (Join-Path $sdk 'ndk') $NdkVersion)"
    Ok (& cargo ndk --version)
}
if (-not (Have 'adb')) {
    Warn 'adb not found; install the Android platform-tools to test on a tablet'
}

Step 'Done'
Ok 'test:    powershell -ExecutionPolicy Bypass -File scripts\test.ps1'
Ok 'rebuild: powershell -ExecutionPolicy Bypass -File scripts\rebuild.ps1 [-Release]'
