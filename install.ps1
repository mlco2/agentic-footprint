# Install the agentic-footprint `af.exe` binary on Windows, then run its
# native setup wizard — the PowerShell counterpart of install.sh, plus the
# upgrade/uninstall handling the spec asks of the Windows installer.
#
# Distribution-neutral inputs (parameters win over environment variables):
#   AF_INSTALL_BINARY=C:\path\to\af.exe   install an already-built binary
#   AF_BINARY_URL=https://.../af.exe      download a release binary directly
#   AF_BINARY_SHA256=<hex>                optional checksum for AF_BINARY_URL
#
# From a source checkout, nothing is needed:
#   .\install.ps1        (builds target\release\af.exe with cargo)
#
# Upgrade: re-running against an existing install compares SHA-256 first —
# identical binaries are a no-op, different ones are replaced (with the old
# and new `af --version` reported). Replacement goes through a temp file +
# renaming the old executable before Move-Item, so a currently running
# af.exe (which Windows locks against overwrite but not rename) does not
# block placing the new version.
#
# Uninstall: .\install.ps1 -Uninstall removes the binary and its PATH entry;
# state under %LOCALAPPDATA%\agentic-footprint is left alone.

[CmdletBinding()]
param(
    [string]$BinaryPath = $env:AF_INSTALL_BINARY,
    [string]$BinaryUrl = $env:AF_BINARY_URL,
    [string]$BinarySha256 = $env:AF_BINARY_SHA256,
    [string]$BinDir = $(if ($env:AF_INSTALL_BIN_DIR) { $env:AF_INSTALL_BIN_DIR }
                       else { Join-Path $env:LOCALAPPDATA 'Programs\agentic-footprint' }),
    [string]$Project = (Get-Location).Path,
    [switch]$Yes,
    [switch]$NoPython,
    [switch]$NoSetup,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$target = Join-Path $BinDir 'af.exe'

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-AfVersion([string]$Path) {
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

$staging = Join-Path ([System.IO.Path]::GetTempPath()) "af-install-$PID.exe"

if ($BinaryPath) {
    if (-not (Test-Path -LiteralPath $BinaryPath)) {
        throw "AF_INSTALL_BINARY points at $BinaryPath, which does not exist"
    }
    Copy-Item -LiteralPath $BinaryPath -Destination $staging -Force
} elseif ($BinaryUrl) {
    Write-Host "Downloading $BinaryUrl"
    Invoke-WebRequest -Uri $BinaryUrl -OutFile $staging
    if ($BinarySha256) {
        $actual = Get-Sha256 $staging
        if ($actual -ne $BinarySha256.ToLowerInvariant()) {
            Remove-Item -LiteralPath $staging -Force
            throw "checksum mismatch for ${BinaryUrl}: expected $BinarySha256, got $actual"
        }
        Write-Host 'Checksum verified.'
    }
} elseif (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml')) {
    Write-Host 'Building af.exe from this checkout (cargo build --release -p af-cli)'
    Push-Location $PSScriptRoot
    try {
        cargo build --release -p af-cli
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\af.exe') -Destination $staging -Force
} else {
    throw 'No input: set AF_INSTALL_BINARY or AF_BINARY_URL, or run from a source checkout'
}

# --- upgrade detection ----------------------------------------------------

$installNeeded = $true
if (Test-Path -LiteralPath $target) {
    if ((Get-Sha256 $target) -eq (Get-Sha256 $staging)) {
        Remove-Item -LiteralPath $staging -Force
        Write-Host "af.exe at $target is already up to date."
        $installNeeded = $false
    } else {
        $oldVersion = Get-AfVersion $target
        $newVersion = Get-AfVersion $staging
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
    Write-Host "Installed $(Get-AfVersion $target) at $target"
}
Add-ToUserPath $BinDir

# --- wizard ---------------------------------------------------------------

if (-not $NoSetup) {
    $setupArgs = @('setup', '--project', $Project)
    if ($Yes) { $setupArgs += '--yes' }
    & $target @setupArgs
    if ($LASTEXITCODE -ne 0) { throw "af setup exited with $LASTEXITCODE" }
}
if (-not $NoPython) {
    & $target python setup
    if ($LASTEXITCODE -ne 0) { throw "af python setup exited with $LASTEXITCODE" }
}
