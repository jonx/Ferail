<#
.SYNOPSIS
    Create, mutate or remove the disposable NTFS fixture used to qualify
    Ferail Fast NTFS Disk Usage.

.EXAMPLE
    ./scripts/testing/fast-ntfs-vhdx.ps1 Create
    ./scripts/testing/fast-ntfs-vhdx.ps1 Mutate -MutationSeconds 30
    ./scripts/testing/fast-ntfs-vhdx.ps1 Cleanup

.NOTES
    Requires an elevated PowerShell and the Hyper-V PowerShell module. The
    VHDX stays under target/ by default and is never committed. Cleanup will
    delete only a .vhdx carrying this script's matching marker file.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('Create', 'Mutate', 'Cleanup')]
    [string]$Action = 'Create',
    [string]$VhdxPath,
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
if (-not (Get-Command New-VHD -ErrorAction SilentlyContinue)) {
    throw 'The Hyper-V PowerShell module is required (New-VHD/Mount-VHD).'
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
        $vhd = New-VHD -Path $VhdxPath -Dynamic -SizeBytes 8GB -BlockSizeBytes 1MB
        try {
            $disk = Mount-VHD -Path $VhdxPath -Passthru
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
            Dismount-VHD -Path $VhdxPath -ErrorAction SilentlyContinue
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
    'Cleanup' {
        if (-not (Test-Path -LiteralPath $MarkerPath)) {
            throw "refusing to delete an unmarked VHDX: $VhdxPath"
        }
        $marker = Get-Content -Raw -LiteralPath $MarkerPath | ConvertFrom-Json
        if ([System.IO.Path]::GetFullPath([string]$marker.vhdx) -ne $VhdxPath -or
            [string]$marker.purpose -ne 'Ferail Fast NTFS disposable qualification fixture') {
            throw 'fixture marker does not match the requested VHDX'
        }
        Dismount-VHD -Path $VhdxPath -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $VhdxPath) {
            Remove-Item -LiteralPath $VhdxPath -Force
        }
        Remove-Item -LiteralPath $MarkerPath -Force
        Write-Host "Removed fixture: $VhdxPath"
    }
}
