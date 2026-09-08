<#
.SYNOPSIS
    Meadow installer for Windows.

.DESCRIPTION
    irm https://raw.githubusercontent.com/mcdearman/meadow/master/install.ps1 | iex

    Downloads a prebuilt meadow.exe for this machine from GitHub Releases and
    puts it in %USERPROFILE%\.meadow\bin. If there is no release build for this
    platform (or you pass -FromSource) it builds from source instead, which
    needs a Rust toolchain.

.PARAMETER Version
    Install a specific release tag, e.g. -Version v0.1.0. Defaults to latest.

.PARAMETER FromSource
    Build from source with cargo instead of downloading a prebuilt binary.

.PARAMETER NoModifyPath
    Do not add the install directory to your user PATH.

.PARAMETER Uninstall
    Remove meadow and its PATH entry.

.EXAMPLE
    .\install.ps1
.EXAMPLE
    .\install.ps1 -FromSource
#>
[CmdletBinding()]
param(
    [string] $Version = 'latest',
    [switch] $FromSource,
    [switch] $NoModifyPath,
    [switch] $Uninstall
)

$ErrorActionPreference = 'Stop'

$Repo = 'mcdearman/meadow'
$MeadowHome = if ($env:MEADOW_HOME) { $env:MEADOW_HOME } else { Join-Path $HOME '.meadow' }
$BinDir = Join-Path $MeadowHome 'bin'

function Write-Say($msg) { Write-Host "meadow: $msg" }
function Write-Fail($msg) { Write-Host "meadow: error: $msg" -ForegroundColor Red; exit 1 }

function Test-Command($name) {
    $null -ne (Get-Command $name -ErrorAction SilentlyContinue)
}

# Run a native command and judge it by its exit code.
#
# Windows PowerShell turns anything a native exe writes to stderr into an
# ErrorRecord, so with $ErrorActionPreference = 'Stop' a perfectly successful
# `git clone` (which announces "Cloning into ..." on stderr) would blow up. Drop
# to 'Continue' for the call and check $LASTEXITCODE instead.
function Invoke-Native {
    param(
        [Parameter(Mandatory)] [string] $Exe,
        [string[]] $Arguments = @(),
        [Parameter(Mandatory)] [string] $ErrorMessage
    )
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Exe @Arguments
    } finally {
        $ErrorActionPreference = $prev
    }
    if ($LASTEXITCODE -ne 0) { Write-Fail $ErrorMessage }
}

# The Rust target triple for this machine, or $null if we do not publish one.
function Get-Target {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch) {
        'X64'   { return 'x86_64-pc-windows-msvc' }
        'Arm64' { return 'aarch64-pc-windows-msvc' }
        default { return $null }
    }
}

# Download and unpack the release zip. Returns $true on success, $false when
# there is no such asset (so the caller can fall back to a source build).
function Install-Prebuilt($target) {
    $asset = "meadow-$target.zip"
    $url = if ($Version -eq 'latest') {
        "https://github.com/$Repo/releases/latest/download/$asset"
    } else {
        "https://github.com/$Repo/releases/download/$Version/$asset"
    }

    Write-Say "downloading $url"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("meadow-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    $zip = Join-Path $tmp $asset

    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        return $false
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $exe = Join-Path $tmp 'meadow.exe'
    if (-not (Test-Path $exe)) {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        Write-Fail "$asset did not contain meadow.exe"
    }

    Copy-Item $exe (Join-Path $BinDir 'meadow.exe') -Force
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    return $true
}

function Install-FromSource {
    if (-not (Test-Command 'cargo')) {
        Write-Fail "need cargo (not found). Install a Rust toolchain from https://rustup.rs"
    }
    if (-not (Test-Command 'git')) { Write-Fail "need git (not found)" }

    $src = Join-Path $MeadowHome 'src'
    $url = "https://github.com/$Repo.git"
    Write-Say "fetching source into $src"
    if (Test-Path (Join-Path $src '.git')) {
        Invoke-Native git @('-C', $src, 'fetch', '--quiet', '--depth', '1', 'origin') `
            -ErrorMessage 'git fetch failed'
        Invoke-Native git @('-C', $src, 'reset', '--quiet', '--hard', 'FETCH_HEAD') `
            -ErrorMessage 'git reset failed'
    } else {
        Remove-Item -Recurse -Force $src -ErrorAction SilentlyContinue
        $cloneArgs = @('clone', '--quiet', '--depth', '1')
        if ($Version -ne 'latest') { $cloneArgs += @('--branch', $Version) }
        $cloneArgs += @($url, $src)
        Invoke-Native git $cloneArgs -ErrorMessage "git clone failed (no such tag: $Version?)"
    }

    Write-Say 'building (this takes a minute)'
    # The CLI lives in its own workspace; --root puts the binary in our bin dir
    # rather than ~\.cargo\bin.
    Invoke-Native cargo @(
        'install', '--path', (Join-Path $src 'meadow'), '--root', $MeadowHome, '--force'
    ) -ErrorMessage 'cargo install failed'
}

# The user PATH is read and written straight out of the registry, *not* through
# [Environment]::SetEnvironmentVariable. That API expands %VARS% before writing,
# so a PATH containing something like %JAVA_HOME%\bin would come back with the
# value baked in permanently. Reading with DoNotExpandEnvironmentNames and
# writing back with the original value kind keeps it exactly as the user had it.

function Get-UserPathEntry {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    try {
        if ($null -eq $key) { return @{ Value = ''; Kind = 'ExpandString' } }
        $raw = $key.GetValue(
            'Path', '',
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        $kind = try { $key.GetValueKind('Path') } catch { 'ExpandString' }
        return @{ Value = [string]$raw; Kind = $kind }
    } finally { if ($key) { $key.Close() } }
}

function Set-UserPathEntry($value, $kind) {
    # A PATH with %VARS% must stay REG_EXPAND_SZ or the references stop working.
    if ($value -like '*%*') { $kind = 'ExpandString' }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) { Write-Fail 'could not open HKCU\Environment for writing' }
    try {
        $key.SetValue('Path', $value, $kind)
    } finally { $key.Close() }
    Send-SettingChange
}

# Tell running programs (Explorer, and so any shell it launches) that the
# environment moved, which is what makes "open a new terminal" enough.
function Send-SettingChange {
    if (-not ('MeadowNativeMethods' -as [type])) {
        Add-Type -Namespace '' -Name 'MeadowNativeMethods' -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(
    IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam,
    uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@ | Out-Null
    }
    $HWND_BROADCAST = [IntPtr] 0xffff
    $WM_SETTINGCHANGE = 0x1a
    $SMTO_ABORTIFHUNG = 0x2
    $result = [UIntPtr]::Zero
    [void][MeadowNativeMethods]::SendMessageTimeout(
        $HWND_BROADCAST, $WM_SETTINGCHANGE, [UIntPtr]::Zero, 'Environment',
        $SMTO_ABORTIFHUNG, 5000, [ref] $result)
}

function Add-ToPath {
    $current = Get-UserPathEntry
    $entries = $current.Value -split ';' | Where-Object { $_ -ne '' }
    if ($entries -contains $BinDir) { return }
    Set-UserPathEntry ((@($entries) + $BinDir) -join ';') $current.Kind
    Write-Say "added $BinDir to your user PATH"
}

function Remove-FromPath {
    $current = Get-UserPathEntry
    $entries = $current.Value -split ';' | Where-Object { $_ -ne '' }
    # Nothing of ours in there — leave the registry untouched.
    if ($entries -notcontains $BinDir) { return }
    $kept = $entries | Where-Object { $_ -ne $BinDir }
    Set-UserPathEntry ($kept -join ';') $current.Kind
    Write-Say "removed $BinDir from your user PATH"
}

function Invoke-Uninstall {
    if (-not (Test-Path $MeadowHome)) { Write-Fail "meadow is not installed at $MeadowHome" }
    Remove-FromPath
    Remove-Item -Recurse -Force $MeadowHome
    Write-Say "removed $MeadowHome"
}

# --- main --------------------------------------------------------------------

if ($Uninstall) {
    Invoke-Uninstall
    return
}

Write-Say "installing meadow to $BinDir"
New-Item -ItemType Directory -Path $BinDir -Force | Out-Null

if ($FromSource) {
    Install-FromSource
} else {
    $target = Get-Target
    $ok = $false
    if ($target) { $ok = Install-Prebuilt $target }
    if (-not $ok) {
        $what = if ($target) { $target } else { 'this platform' }
        Write-Say "no prebuilt binary for $what - building from source"
        Install-FromSource
    }
}

if (-not $NoModifyPath) { Add-ToPath }

$exe = Join-Path $BinDir 'meadow.exe'
if (-not (Test-Path $exe)) { Write-Fail "install finished but $exe is missing" }

# Best-effort version banner: an older build may not know --version, and that is
# not a reason to fail the install (or to print clap's complaint). With
# ErrorActionPreference = Continue, `2>&1` folds stderr into $out as ErrorRecords
# instead of throwing, so nothing reaches the console; the exit code still tells
# us whether to trust it.
$installed = & {
    $ErrorActionPreference = 'Continue'
    $out = & $exe --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        ($out | Where-Object { $_ -is [string] } | Select-Object -First 1)
    } else {
        'meadow'
    }
}
Write-Say ''
Write-Say "installed $installed"
Write-Say ''
Write-Say '  meadow                 start the REPL'
Write-Say '  meadow run <path>      build and run a package'
Write-Say '  meadow build --release build with release checks'
Write-Say ''
if ($NoModifyPath) {
    Write-Say "Add to your PATH: $BinDir"
} else {
    Write-Say 'Open a new terminal to pick up the PATH change.'
}

# Do not let a probe's exit code leak out as the installer's.
exit 0
