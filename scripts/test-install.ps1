# Exercises install.ps1 against a tempdir: fresh install, idempotent
# re-run, upgrade, and uninstall — with stub binaries so no build or
# network is involved. The PowerShell sibling of scripts/test-install.sh.
#
# The stubs are plain text files named .exe: install.ps1 compares installs
# by SHA-256 and its Get-AfVersion already degrades to '(version
# unavailable)' for a binary it cannot run, so nothing here needs a real
# executable — and the asserts below compare content hashes, not
# `af --version` output.

$ErrorActionPreference = 'Stop'
$root = Join-Path ([System.IO.Path]::GetTempPath()) "af-install-test-$PID"
New-Item -ItemType Directory -Force -Path $root | Out-Null

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        Write-Error "FAIL: $Message"
        exit 1
    }
    Write-Host "ok: $Message"
}

function Get-Sha([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

try {
    $binDir = Join-Path $root 'bin'
    $installer = Join-Path $PSScriptRoot '..\install.ps1'

    Set-Content -Path (Join-Path $root 'af-v1.exe') -Encoding ascii -Value 'stub v1'
    Set-Content -Path (Join-Path $root 'af-v2.exe') -Encoding ascii -Value 'stub v2'

    # Fresh install (no setup/python: the stub knows no subcommands).
    & $installer -BinaryPath (Join-Path $root 'af-v1.exe') -BinDir $binDir -Yes -NoSetup -NoPython
    Assert (Test-Path (Join-Path $binDir 'af.exe')) 'fresh install places af.exe'
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    Assert ($userPath -split ';' -contains $binDir) 'bin dir lands on the user PATH'

    # Idempotent re-run: same bytes, no-op.
    $before = (Get-Item (Join-Path $binDir 'af.exe')).LastWriteTimeUtc
    & $installer -BinaryPath (Join-Path $root 'af-v1.exe') -BinDir $binDir -Yes -NoSetup -NoPython
    $after = (Get-Item (Join-Path $binDir 'af.exe')).LastWriteTimeUtc
    Assert ($before -eq $after) 'identical re-install is a no-op'

    # Even a no-op binary install repairs a missing PATH entry.
    [Environment]::SetEnvironmentVariable('Path', '', 'User')
    & $installer -BinaryPath (Join-Path $root 'af-v1.exe') -BinDir $binDir -Yes -NoSetup -NoPython
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    Assert ($userPath -split ';' -contains $binDir) 'identical re-install repairs PATH'

    # Upgrade: different bytes replace the install.
    & $installer -BinaryPath (Join-Path $root 'af-v2.exe') -BinDir $binDir -Yes -NoSetup -NoPython
    Assert ((Get-Sha (Join-Path $binDir 'af.exe')) -eq (Get-Sha (Join-Path $root 'af-v2.exe'))) `
        'upgrade replaces the binary'

    # Checksum: a wrong hash must abort. (file:// URI keeps it offline.)
    $failed = $false
    try {
        & $installer -BinaryUrl ([Uri](Join-Path $root 'af-v1.exe')).AbsoluteUri `
            -BinarySha256 ('0' * 64) -BinDir $binDir -Yes -NoSetup -NoPython
    } catch { $failed = $true }
    Assert $failed 'checksum mismatch aborts the install'
    Assert ((Get-Sha (Join-Path $binDir 'af.exe')) -eq (Get-Sha (Join-Path $root 'af-v2.exe'))) `
        'failed install leaves the old binary intact'

    # Uninstall: binary and PATH entry go; nothing else.
    & $installer -BinDir $binDir -Uninstall
    Assert (-not (Test-Path (Join-Path $binDir 'af.exe'))) 'uninstall removes af.exe'
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    Assert (-not ($userPath -split ';' -contains $binDir)) 'uninstall removes the PATH entry'

    Write-Host 'test-install.ps1: all assertions passed'
} finally {
    # The test touched the real user PATH; make sure the tempdir entry is
    # gone even when an assertion failed mid-way.
    $binDir = Join-Path $root 'bin'
    $parts = ([Environment]::GetEnvironmentVariable('Path', 'User') -split ';') |
        Where-Object { $_ -and $_ -ne $binDir }
    [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
