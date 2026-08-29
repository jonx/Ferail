<#
.SYNOPSIS
    Create, mutate or remove the disposable NTFS fixture used to qualify
    Ferail Fast NTFS Disk Usage.

.EXAMPLE
    ./scripts/testing/fast-ntfs-vhdx.ps1 Create
    ./scripts/testing/fast-ntfs-vhdx.ps1 Diagnose
    ./scripts/testing/fast-ntfs-vhdx.ps1 Mutate -MutationSeconds 30
    ./scripts/testing/fast-ntfs-vhdx.ps1 Cleanup

.NOTES
    Requires an elevated PowerShell. The Hyper-V PowerShell module is used
    when available; otherwise the script falls back to built-in DiskPart and
    Storage cmdlets. The VHDX stays under target/ by default and is never
    committed. Cleanup will delete only a .vhdx carrying this script's
    matching marker file.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('Create', 'Diagnose', 'Mutate', 'Cleanup')]
    [string]$Action = 'Create',
    [string]$VhdxPath,
    [string]$HelperPath,
    [ValidateRange(1, 600)]
    [int]$MutationSeconds = 15
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $VhdxPath) {
    $VhdxPath = Join-Path $RepoRoot 'target\fast-ntfs-fixture.vhdx'
}
$VhdxPath = [System.IO.Path]::GetFullPath($VhdxPath)
$MarkerPath = "$VhdxPath.ferail-fixture.json"

if ([System.IO.Path]::GetExtension($VhdxPath) -ne '.vhdx' -or
    [System.IO.Path]::GetPathRoot($VhdxPath) -eq $VhdxPath) {
    throw "refusing unsafe VHDX path: $VhdxPath"
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this fixture script from an elevated PowerShell.'
}

$UseHyperV = [bool](Get-Command New-VHD -ErrorAction SilentlyContinue) -and
    [bool](Get-Command Mount-VHD -ErrorAction SilentlyContinue) -and
    [bool](Get-Command Dismount-VHD -ErrorAction SilentlyContinue)

function Invoke-DiskPart {
    param([Parameter(Mandatory)][string[]]$Commands)

    $scriptPath = [System.IO.Path]::GetTempFileName()
    try {
        [System.IO.File]::WriteAllLines(
            $scriptPath,
            $Commands,
            [System.Text.Encoding]::ASCII
        )
        $output = & diskpart.exe /s $scriptPath 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "DiskPart failed with exit code $LASTEXITCODE`n$($output -join [Environment]::NewLine)"
        }
        $output
    } finally {
        Remove-Item -LiteralPath $scriptPath -Force -ErrorAction SilentlyContinue
    }
}

function New-FixtureVhd {
    if ($UseHyperV) {
        New-VHD -Path $VhdxPath -Dynamic -SizeBytes 8GB -BlockSizeBytes 1MB | Out-Null
        return
    }

    if (-not (Get-Command diskpart.exe -ErrorAction SilentlyContinue)) {
        throw 'Neither the Hyper-V PowerShell module nor built-in DiskPart is available.'
    }
    Invoke-DiskPart @(
        "create vdisk file=`"$VhdxPath`" maximum=8192 type=expandable"
    ) | Out-Null
    if (-not (Test-Path -LiteralPath $VhdxPath)) {
        throw "DiskPart did not create the requested VHDX: $VhdxPath"
    }
}

function Mount-FixtureVhd {
    if ($UseHyperV) {
        return Mount-VHD -Path $VhdxPath -Passthru
    }

    $image = Mount-DiskImage -ImagePath $VhdxPath -PassThru
    return $image | Get-Disk
}

function Dismount-FixtureVhd {
    if ($UseHyperV) {
        Dismount-VHD -Path $VhdxPath -ErrorAction SilentlyContinue
    } elseif (Test-Path -LiteralPath $VhdxPath) {
        Dismount-DiskImage -ImagePath $VhdxPath -ErrorAction SilentlyContinue
    }
}

function Get-FixtureRoot {
    $volume = Get-Volume | Where-Object FileSystemLabel -eq 'FERAIL_NTFS_TEST' |
        Select-Object -First 1
    if (-not $volume -or -not $volume.DriveLetter) {
        throw 'FERAIL_NTFS_TEST is not mounted.'
    }
    Join-Path "$($volume.DriveLetter):\" 'FerailFastNtfs'
}

switch ($Action) {
    'Create' {
        if ((Test-Path -LiteralPath $VhdxPath) -or (Test-Path -LiteralPath $MarkerPath)) {
            throw "fixture already exists: $VhdxPath"
        }
        New-Item -ItemType Directory -Path (Split-Path -Parent $VhdxPath) -Force | Out-Null
        New-FixtureVhd
        try {
            $disk = Mount-FixtureVhd
            Initialize-Disk -Number $disk.DiskNumber -PartitionStyle GPT -PassThru | Out-Null
            $partition = New-Partition -DiskNumber $disk.DiskNumber -UseMaximumSize -AssignDriveLetter
            $volume = Format-Volume -Partition $partition -FileSystem NTFS `
                -NewFileSystemLabel 'FERAIL_NTFS_TEST' -Confirm:$false
            $root = Join-Path "$($volume.DriveLetter):\" 'FerailFastNtfs'
            New-Item -ItemType Directory -Path $root | Out-Null

            $nested = New-Item -ItemType Directory -Path (Join-Path $root 'nested\deep') -Force
            [IO.File]::WriteAllBytes((Join-Path $root 'ordinary.bin'), [byte[]](0..255))
            [IO.File]::WriteAllText((Join-Path $nested.FullName 'unicode-é-漢字-😀.txt'), 'unicode')

            $hardlink = Join-Path $nested.FullName 'ordinary-hardlink.bin'
            & fsutil hardlink create $hardlink (Join-Path $root 'ordinary.bin') | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'fsutil hardlink create failed' }

            $sparse = Join-Path $root 'sparse-64m.bin'
            & fsutil file createnew $sparse 67108864 | Out-Null
            & fsutil sparse setflag $sparse | Out-Null
            & fsutil sparse setrange $sparse 1048576 65011712 | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'fsutil sparse setup failed' }

            $compressed = Join-Path $root 'compressed.bin'
            [IO.File]::WriteAllBytes($compressed, [byte[]]::new(8MB))
            & compact.exe /c /i /q $compressed | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'NTFS compression setup failed' }

            [IO.File]::WriteAllText("$compressed`:ferail-ads", 'named stream is not charged in v1')
            New-Item -ItemType Junction -Path (Join-Path $root 'junction-leaf') `
                -Target (Join-Path $root 'nested') | Out-Null

            $cursor = $root
            foreach ($index in 1..18) {
                $cursor = Join-Path $cursor ("long-component-{0:D2}-abcdefghijklmnop" -f $index)
                New-Item -ItemType Directory -Path $cursor | Out-Null
            }
            [IO.File]::WriteAllText((Join-Path $cursor 'deep-leaf.txt'), 'deep')

            $locked = New-Item -ItemType Directory -Path (Join-Path $root 'permission-denied')
            [IO.File]::WriteAllText((Join-Path $locked.FullName 'hidden.txt'), 'locked')
            $user = $identity.Name
            & icacls.exe $locked.FullName /inheritance:r /deny "${user}:(OI)(CI)(RX)" /q | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'fixture ACL setup failed' }

            [ordered]@{
                schema = 1
                purpose = 'Ferail Fast NTFS disposable qualification fixture'
                vhdx = $VhdxPath
                volume_label = 'FERAIL_NTFS_TEST'
                root = $root
                created_utc = (Get-Date).ToUniversalTime().ToString('o')
            } | ConvertTo-Json | Set-Content -LiteralPath $MarkerPath -Encoding UTF8
            Write-Host "Fixture ready: $root"
        } catch {
            Dismount-FixtureVhd
            throw
        }
    }
    'Mutate' {
        if (-not (Test-Path -LiteralPath $MarkerPath)) {
            throw 'fixture marker is missing; run Create first'
        }
        $root = Get-FixtureRoot
        $churn = Join-Path $root 'concurrent-mutation'
        New-Item -ItemType Directory -Path $churn -Force | Out-Null
        $deadline = (Get-Date).AddSeconds($MutationSeconds)
        $iteration = 0
        while ((Get-Date) -lt $deadline) {
            $path = Join-Path $churn ("churn-{0:D6}.tmp" -f $iteration)
            [IO.File]::WriteAllBytes($path, [byte[]]::new(4096))
            Remove-Item -LiteralPath $path -Force
            $iteration++
        }
        Write-Host "Mutation complete: $iteration create/delete cycles"
    }
    'Diagnose' {
        if (-not (Test-Path -LiteralPath $MarkerPath)) {
            throw 'fixture marker is missing; run Create first'
        }
        $root = Get-FixtureRoot
        if (-not $HelperPath) {
            $releaseHelper = Join-Path $RepoRoot 'target\release\ferail-ntfs-helper.exe'
            $debugHelper = Join-Path $RepoRoot 'target\debug\ferail-ntfs-helper.exe'
            $HelperPath = if (Test-Path -LiteralPath $releaseHelper) {
                $releaseHelper
            } else {
                $debugHelper
            }
        }
        $HelperPath = [System.IO.Path]::GetFullPath($HelperPath)
        if (-not (Test-Path -LiteralPath $HelperPath -PathType Leaf)) {
            throw "Fast NTFS helper not found: $HelperPath"
        }
        Write-Host "Running direct Fast NTFS diagnostic against the disposable fixture..."
        & $HelperPath --diagnose $root
        if ($LASTEXITCODE -ne 0) {
            throw "Fast NTFS diagnostic failed with exit code $LASTEXITCODE"
        }
    }
    'Cleanup' {
        if (-not (Test-Path -LiteralPath $MarkerPath)) {
            throw "refusing to delete an unmarked VHDX: $VhdxPath"
        }
        $marker = Get-Content -Raw -LiteralPath $MarkerPath | ConvertFrom-Json
        if ([System.IO.Path]::GetFullPath([string]$marker.vhdx) -ne $VhdxPath -or
            [string]$marker.purpose -ne 'Ferail Fast NTFS disposable qualification fixture') {
            throw 'fixture marker does not match the requested VHDX'
        }
        Dismount-FixtureVhd
        if (Test-Path -LiteralPath $VhdxPath) {
            Remove-Item -LiteralPath $VhdxPath -Force
        }
        Remove-Item -LiteralPath $MarkerPath -Force
        Write-Host "Removed fixture: $VhdxPath"
    }
}
