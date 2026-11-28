# Install the agentic-footprint `afoot.exe` binary on Windows, then run its
# native setup wizard — the PowerShell counterpart of install.sh, plus the
# upgrade/uninstall handling the spec asks of the Windows installer.
#
# Distribution-neutral inputs (parameters win over environment variables):
#   AFOOT_INSTALL_BINARY=C:\path\to\afoot.exe   install an already-built binary
#   AFOOT_BINARY_URL=https://.../afoot.exe      download a release binary directly
#   AFOOT_BINARY_SHA256=<hex>                required SHA-256 of AFOOT_BINARY_URL
#
# From a source checkout, nothing is needed:
#   .\install.ps1        (builds target\release\afoot.exe with cargo)
#
# Upgrade: re-running against an existing install compares SHA-256 first —
# identical binaries are a no-op, different ones are replaced (with the old
# and new `afoot --version` reported). Replacement goes through a temp file +
# renaming the old executable before Move-Item, so a currently running
# afoot.exe (which Windows locks against overwrite but not rename) does not
# block placing the new version.
#
# Uninstall: .\install.ps1 -Uninstall removes the binary and its PATH entry;
# state under %LOCALAPPDATA%\agentic-footprint is left alone.

[CmdletBinding()]
param(
    [string]$BinaryPath = $env:AFOOT_INSTALL_BINARY,
    [string]$BinaryUrl = $env:AFOOT_BINARY_URL,
    [string]$BinarySha256 = $env:AFOOT_BINARY_SHA256,
    [string]$BinDir = $(if ($env:AFOOT_INSTALL_BIN_DIR) { $env:AFOOT_INSTALL_BIN_DIR }
                       else { Join-Path $env:LOCALAPPDATA 'Programs\agentic-footprint' }),
    [string]$Project = (Get-Location).Path,
    [switch]$Yes,
    [switch]$NoPython,
    [switch]$NoSetup,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$target = Join-Path $BinDir 'afoot.exe'

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-AfootVersion([string]$Path) {
    try { (& $Path --version) 2>$null | Select-Object -First 1 } catch { '(version unavailable)' }
}

# The *user* PATH (registry-backed), not the process PATH: idempotent add
# and removal, never touching machine-level configuration.
function Get-UserPathParts {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath) { $userPath -split ';' | Where-Object { $_ -ne '' } } else { @() }
}

function Add-ToUserPath([string]$Dir) {
    $parts = @(Get-UserPathParts)
    if ($parts -contains $Dir) { return }
    [Environment]::SetEnvironmentVariable('Path', (($parts + $Dir) -join ';'), 'User')
    Write-Host "Added $Dir to your user PATH (new terminals will see it)"
}

function Remove-FromUserPath([string]$Dir) {
    $parts = @(Get-UserPathParts)
    if ($parts -notcontains $Dir) { return }
    [Environment]::SetEnvironmentVariable('Path', (($parts | Where-Object { $_ -ne $Dir }) -join ';'), 'User')
    Write-Host "Removed $Dir from your user PATH"
}

if ($Uninstall) {
    if (Test-Path -LiteralPath $target) {
        Remove-Item -LiteralPath $target -Force
        Write-Host "Removed $target"
    } else {
        Write-Host "Nothing installed at $target"
    }
    Remove-FromUserPath $BinDir
    Write-Host 'State under %LOCALAPPDATA%\agentic-footprint was left in place.'
    exit 0
}

# --- obtain the new binary (into a temp file, never straight onto target) --

$staging = Join-Path ([System.IO.Path]::GetTempPath()) "afoot-install-$PID.exe"

if ($BinaryPath) {
    if (-not (Test-Path -LiteralPath $BinaryPath)) {
        throw "AFOOT_INSTALL_BINARY points at $BinaryPath, which does not exist"
    }
    Copy-Item -LiteralPath $BinaryPath -Destination $staging -Force
} elseif ($BinaryUrl) {
    # A URL alone is not a source of trust: whoever serves it decides what
    # runs next, with the user's permissions.
    if (-not $BinarySha256) {
        throw 'AFOOT_BINARY_URL needs AFOOT_BINARY_SHA256, the SHA-256 of what it points at (from the release''s SHA256SUMS); nothing was downloaded'
    }
    Write-Host "Downloading $BinaryUrl"
    Invoke-WebRequest -Uri $BinaryUrl -OutFile $staging
    $actual = Get-Sha256 $staging
    if ($actual -ne $BinarySha256.ToLowerInvariant()) {
        Remove-Item -LiteralPath $staging -Force
        throw "checksum mismatch for ${BinaryUrl}: expected $BinarySha256, got $actual"
    }
    Write-Host 'Checksum verified.'
} elseif (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml')) {
    Write-Host 'Building afoot.exe from this checkout (cargo build --release -p afoot-cli)'
    Push-Location $PSScriptRoot
    try {
        cargo build --release -p afoot-cli
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\afoot.exe') -Destination $staging -Force
} else {
    throw 'No input: set AFOOT_INSTALL_BINARY or AFOOT_BINARY_URL, or run from a source checkout'
}

# --- upgrade detection ----------------------------------------------------

$installNeeded = $true
if (Test-Path -LiteralPath $target) {
    if ((Get-Sha256 $target) -eq (Get-Sha256 $staging)) {
        Remove-Item -LiteralPath $staging -Force
        Write-Host "afoot.exe at $target is already up to date."
        $installNeeded = $false
    } else {
        $oldVersion = Get-AfootVersion $target
        $newVersion = Get-AfootVersion $staging
        Write-Host "Upgrading: $oldVersion -> $newVersion"
        if (-not $Yes) {
            $answer = Read-Host "Replace $target? [y/N]"
            if ($answer -notmatch '^[Yy]') {
                Remove-Item -LiteralPath $staging -Force
                Write-Host 'Aborted; nothing was changed.'
                exit 1
            }
        }
    }
}

# --- install --------------------------------------------------------------

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
if ($installNeeded) {
    $previous = $null
    if (Test-Path -LiteralPath $target) {
        $previous = "$target.old-$PID"
        Move-Item -LiteralPath $target -Destination $previous -Force
    }
    try {
        Move-Item -LiteralPath $staging -Destination $target
    } catch {
        if ($previous -and (Test-Path -LiteralPath $previous)) {
            Move-Item -LiteralPath $previous -Destination $target -Force
        }
        throw
    }
    if ($previous) {
        Remove-Item -LiteralPath $previous -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Installed $(Get-AfootVersion $target) at $target"
}
Add-ToUserPath $BinDir

# Only a published release binary can be self-updated later; a source build
# or AFOOT_INSTALL_BINARY copy must be updated the way it was installed.
if ($BinaryUrl -like 'https://github.com/mlco2/agentic-footprint/releases/download/*') {
    & $target self record-install --binary $target
    if ($LASTEXITCODE -ne 0) { throw "afoot self record-install exited with $LASTEXITCODE" }
}

# --- wizard ---------------------------------------------------------------

if (-not $NoSetup) {
    $setupArgs = @('setup', '--project', $Project)
    if ($Yes) { $setupArgs += '--yes' }
    & $target @setupArgs
    if ($LASTEXITCODE -ne 0) { throw "afoot setup exited with $LASTEXITCODE" }
}
if (-not $NoPython) {
    & $target python setup
    if ($LASTEXITCODE -ne 0) { throw "afoot python setup exited with $LASTEXITCODE" }
}
