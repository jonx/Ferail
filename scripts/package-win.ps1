<#
.SYNOPSIS
    Produce a distributable Windows build of Ferail — the Windows twin of
    scripts/package-mac.sh.

.DESCRIPTION
    Pipeline:
      1. Build the release binaries (Ferail GUI + the `ferail` CLI).
      2. Stage them into target/package/Ferail alongside the licence files.
         Licences travel with the binary: MIT/Apache-2.0, the MIT tree-sitter
         grammars and the ISC/MIT icon artwork all require their notices to
         accompany a redistributed copy, so a ZIP of just the .exe would
         under-attribute.
      3. Authenticode-sign the payload, if a certificate was supplied.
      4. Emit a portable ZIP (always) and an Inno Setup installer (when the
         Inno compiler `iscc` is available).
      5. Sign the installer and print a verification summary.

    macOS parity note: the Apple side has to satisfy Gatekeeper, so
    package-mac.sh signs with a Developer ID, notarizes, and staples. Windows
    has no notarization service — the equivalent trust signal is an
    Authenticode signature plus SmartScreen reputation, which accrues to the
    signing certificate over downloads. An UNSIGNED build is fully functional
    but every downloader gets a "Windows protected your PC" SmartScreen
    interstitial, so treat signing as required for a public release even though
    this script will happily produce an unsigned artifact for local testing.

.PARAMETER SignCert
    Path to a .pfx, or the SHA1 thumbprint of a cert in the user's store.
    Defaults to $env:FERAIL_SIGN_CERT. When empty, signing is skipped.

.PARAMETER SignPassword
    Password for a .pfx. Defaults to $env:FERAIL_SIGN_PASSWORD.

.PARAMETER TimestampUrl
    RFC-3161 timestamp server. A timestamp is what keeps the signature valid
    after the certificate expires, so this is not optional in practice.

.PARAMETER NoInstaller
    Emit only the portable ZIP, even if `iscc` is present.

.PARAMETER SkipBuild
    Package whatever is already in target/release (for iterating on packaging).

.PARAMETER Features
    Cargo features for the build. Defaults to "mpv": the mpv video provider
    is a runtime dlopen (no build-time link, no bundled DLL), so the package
    still runs on a machine with no libmpv — the viewer just falls back to
    the native player. -Features '' builds without it.

.EXAMPLE
    ./scripts/package-win.ps1
    ./scripts/package-win.ps1 -SignCert C:\certs\ferail.pfx -SignPassword hunter2
    ./scripts/package-win.ps1 -Features '' -NoInstaller
#>
[CmdletBinding()]
param(
    [string]$SignCert = $env:FERAIL_SIGN_CERT,
    [string]$SignPassword = $env:FERAIL_SIGN_PASSWORD,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    [switch]$NoInstaller,
    [switch]$SkipBuild,
    [string]$Features = 'mpv'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Write-Warn($msg) { Write-Host "warning: $msg" -ForegroundColor Yellow }

$RepoRoot = Split-Path -Parent $PSScriptRoot
$StageRoot = Join-Path $RepoRoot 'target\package'
$StageDir = Join-Path $StageRoot 'Ferail'

# ---------------------------------------------------------------------------
# Version — single source of truth is [workspace.package] in Cargo.toml.
# ---------------------------------------------------------------------------
$cargoToml = Get-Content (Join-Path $RepoRoot 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^\s*version\s*=\s*"([^"]+)"') {
    throw 'Could not read workspace version from Cargo.toml'
}
$Version = $Matches[1]
Write-Step "Ferail $Version (x86_64-pc-windows-msvc)"

# ---------------------------------------------------------------------------
# 1. Build
# ---------------------------------------------------------------------------
if (-not $SkipBuild) {
    $cargoArgs = @('build', '--release', '--bin', 'ferail-gpui', '--bin', 'ferail')
    if ($Features) { $cargoArgs += @('--features', $Features) }
    Write-Step "cargo $($cargoArgs -join ' ')"
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} else {
    Write-Step 'Skipping build (-SkipBuild)'
}

$GuiSrc = Join-Path $RepoRoot 'target\release\ferail-gpui.exe'
$CliSrc = Join-Path $RepoRoot 'target\release\ferail.exe'
foreach ($p in @($GuiSrc, $CliSrc)) {
    if (-not (Test-Path $p)) { throw "missing build output: $p" }
}

# ---------------------------------------------------------------------------
# 2. Stage
# ---------------------------------------------------------------------------
Write-Step "Staging $StageDir"
if (Test-Path $StageDir) { Remove-Item $StageDir -Recurse -Force }
New-Item -ItemType Directory -Path $StageDir -Force | Out-Null

# Shipped under the product name: the crate/target name `ferail-gpui.exe` is a
# build artefact, but this .exe is what the user double-clicks and sees in the
# Start Menu and Task Manager.
#
# Side effect worth knowing: build.rs stamps VERSIONINFO `OriginalFilename` as
# `ferail-gpui.exe` for every binary in the crate, so the shipped GUI reports a
# name it no longer has. Cosmetic (Explorer's Properties tab). Left alone
# because winresource applies one block per crate, not per binary — setting it
# to `Ferail.exe` would just move the inaccuracy onto the CLI.
$GuiDst = Join-Path $StageDir 'Ferail.exe'
# The CLI keeps its own name (every doc says `ferail magic` / `ferail du`) but
# CANNOT sit next to the GUI: Windows filesystems are case-insensitive, so
# `ferail.exe` and `Ferail.exe` are one path and the second copy silently wins.
# A `cli\` subdirectory keeps both natural names. (This is not hypothetical —
# it shipped a package whose `Ferail.exe` was actually the CLI.)
$CliDir = Join-Path $StageDir 'cli'
New-Item -ItemType Directory -Path $CliDir -Force | Out-Null
$CliDst = Join-Path $CliDir 'ferail.exe'

Copy-Item $GuiSrc $GuiDst
Copy-Item $CliSrc $CliDst

# Guard the invariant rather than trusting it: if these ever land on the same
# path again (a rename, a flattened layout), fail loudly here instead of
# shipping one binary under the other's name.
if ((Get-FileHash $GuiDst -Algorithm SHA256).Hash -eq (Get-FileHash $CliDst -Algorithm SHA256).Hash) {
    throw "staged GUI and CLI are the same file — case-insensitive name collision"
}
if ((Get-FileHash $GuiDst -Algorithm SHA256).Hash -ne (Get-FileHash $GuiSrc -Algorithm SHA256).Hash) {
    throw "staged Ferail.exe does not match target\release\ferail-gpui.exe"
}

$LicDir = Join-Path $StageDir 'licenses'
New-Item -ItemType Directory -Path $LicDir -Force | Out-Null
$licCount = 0
foreach ($f in @('LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')) {
    $src = Join-Path $RepoRoot $f
    if (Test-Path $src) { Copy-Item $src (Join-Path $LicDir $f); $licCount++ }
    else { Write-Warn "$f missing — package will under-attribute" }
}
Write-Step "Copied licenses ($licCount files)"

# ---------------------------------------------------------------------------
# 3. Sign the payload
# ---------------------------------------------------------------------------
function Resolve-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # Not on PATH outside a Developer Prompt — search the SDK, newest first.
    $roots = @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin",
        "$env:ProgramFiles\Windows Kits\10\bin"
    ) | Where-Object { $_ -and (Test-Path $_) }
    foreach ($root in $roots) {
        $hit = Get-ChildItem $root -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\' } |
            Sort-Object FullName -Descending |
            Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    return $null
}

function Invoke-Sign {
    param([string[]]$Files)
    if (-not $SignCert) {
        Write-Warn 'no -SignCert / $env:FERAIL_SIGN_CERT — producing an UNSIGNED build.'
        Write-Warn 'SmartScreen will warn every downloader. Do not publish this artifact.'
        return $false
    }
    $signtool = Resolve-SignTool
    if (-not $signtool) { throw 'signtool.exe not found (install the Windows SDK)' }

    $args = @('sign', '/fd', 'SHA256', '/tr', $TimestampUrl, '/td', 'SHA256')
    if (Test-Path $SignCert) {
        $args += @('/f', $SignCert)
        if ($SignPassword) { $args += @('/p', $SignPassword) }
    } else {
        # Not a file — treat as a store thumbprint.
        $args += @('/sha1', $SignCert)
    }
    $args += $Files
    Write-Step "Signing $($Files.Count) file(s)"
    & $signtool @args
    if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    return $true
}

$signed = Invoke-Sign -Files @($GuiDst, $CliDst)

# ---------------------------------------------------------------------------
# 4. Portable ZIP
# ---------------------------------------------------------------------------
$ZipPath = Join-Path $StageRoot "Ferail-$Version-win-x64.zip"
Write-Step "Writing $ZipPath"
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path $StageDir -DestinationPath $ZipPath -CompressionLevel Optimal

# ---------------------------------------------------------------------------
# 5. Installer (optional — needs Inno Setup's iscc)
# ---------------------------------------------------------------------------
function Resolve-Iscc {
    $cmd = Get-Command iscc.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    )
    foreach ($c in $candidates) { if ($c -and (Test-Path $c)) { return $c } }
    return $null
}

$InstallerPath = $null
if ($NoInstaller) {
    Write-Step 'Skipping installer (-NoInstaller)'
} else {
    $iscc = Resolve-Iscc
    if (-not $iscc) {
        Write-Warn 'Inno Setup (iscc.exe) not found — portable ZIP only.'
        Write-Warn 'Install it for an installer: winget install JRSoftware.InnoSetup'
    } else {
        $iss = Join-Path $RepoRoot 'packaging\windows\ferail.iss'
        Write-Step "Building installer via $iscc"
        & $iscc "/DAppVersion=$Version" "/DSourceDir=$StageDir" $iss
        if ($LASTEXITCODE -ne 0) { throw "iscc failed ($LASTEXITCODE)" }
        $InstallerPath = Join-Path $StageRoot "Ferail-$Version-win-x64-setup.exe"
        if (Test-Path $InstallerPath) {
            # The installer is the file users actually download, so it needs a
            # signature of its own — signing the payload inside it is not
            # enough for SmartScreen.
            [void](Invoke-Sign -Files @($InstallerPath))
        } else {
            Write-Warn "expected installer at $InstallerPath but it is missing"
            $InstallerPath = $null
        }
    }
}

# ---------------------------------------------------------------------------
# 6. Verify + summary
# ---------------------------------------------------------------------------
Write-Step 'Artifacts'
foreach ($a in @($ZipPath, $InstallerPath) | Where-Object { $_ -and (Test-Path $_) }) {
    $size = [math]::Round((Get-Item $a).Length / 1MB, 1)
    Write-Host ("  {0}  ({1} MB)" -f $a, $size)
}

Write-Step 'Signature status'
foreach ($a in @($GuiDst, $CliDst, $InstallerPath) | Where-Object { $_ -and (Test-Path $_) }) {
    $sig = Get-AuthenticodeSignature $a
    Write-Host ("  {0}: {1}" -f (Split-Path $a -Leaf), $sig.Status)
}
if (-not $signed) {
    Write-Warn 'UNSIGNED build — for local testing only.'
}
Write-Step 'Done'
