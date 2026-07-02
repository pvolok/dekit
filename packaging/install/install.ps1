$ErrorActionPreference = "Stop"

$Repo = if ($env:DEKIT_REPO) { $env:DEKIT_REPO } else { "pvolok/dekit" }
$Version = if ($env:DEKIT_VERSION) { $env:DEKIT_VERSION } else { "latest" }
$InstallDir = if ($env:DEKIT_INSTALL_DIR) { $env:DEKIT_INSTALL_DIR } else { Join-Path $HOME ".local\bin" }

function Fail($Message) {
    Write-Error "dekit: $Message"
    exit 1
}

$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
switch ($Architecture) {
    "x64" { $Target = "x86_64-pc-windows-msvc" }
    "arm64" {
        # No native arm64 build; the x64 binary runs under Windows' built-in emulation.
        Write-Warning "No native arm64 build; installing the x64 binary (runs under emulation)."
        $Target = "x86_64-pc-windows-msvc"
    }
    default { Fail "unsupported CPU: $Architecture" }
}

$Asset = "dekit-$Target.zip"
$ReleaseVersion = if ($Version -eq "latest" -or $Version -eq "canary" -or $Version.StartsWith("v")) {
    $Version
} else {
    "v$Version"
}

$BaseUrl = if ($ReleaseVersion -eq "latest") {
    "https://github.com/$Repo/releases/latest/download"
} else {
    "https://github.com/$Repo/releases/download/$ReleaseVersion"
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TempDir | Out-Null

try {
    $ArchivePath = Join-Path $TempDir $Asset
    Invoke-WebRequest -UseBasicParsing -Uri "$BaseUrl/$Asset" -OutFile $ArchivePath

    $SumsPath = Join-Path $TempDir "SHA256SUMS"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$BaseUrl/SHA256SUMS" -OutFile $SumsPath
        $Expected = $null
        foreach ($Line in Get-Content $SumsPath) {
            $Parts = $Line -split "\s+"
            if ($Parts.Length -ge 2 -and ($Parts[-1] -eq $Asset -or $Parts[-1] -eq "*$Asset")) {
                $Expected = $Parts[0].ToLowerInvariant()
                break
            }
        }
        if ($Expected) {
            $Actual = (Get-FileHash -Algorithm SHA256 $ArchivePath).Hash.ToLowerInvariant()
            if ($Actual -ne $Expected) {
                Fail "checksum mismatch for $Asset"
            }
        } else {
            Write-Warning "No checksum found for $Asset; skipping checksum verification"
        }
    } catch {
        Write-Warning "Could not verify checksum: $($_.Exception.Message)"
    }

    Expand-Archive -Path $ArchivePath -DestinationPath $TempDir -Force
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Path (Join-Path $TempDir "dekit.exe") -Destination (Join-Path $InstallDir "dekit.exe") -Force

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $PathEntries = @()
    if ($UserPath) {
        $PathEntries = $UserPath -split ";" | Where-Object { $_ }
    }
    $TrimChars = [char[]]"\/"
    $NormalizedInstallDir = $InstallDir.TrimEnd($TrimChars)
    $HasInstallDir = $false
    foreach ($Entry in $PathEntries) {
        if ([string]::Equals($Entry.TrimEnd($TrimChars), $NormalizedInstallDir, [StringComparison]::OrdinalIgnoreCase)) {
            $HasInstallDir = $true
            break
        }
    }

    if (-not $HasInstallDir) {
        $NewUserPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
    }

    if (($env:Path -split ";") -notcontains $InstallDir) {
        $env:Path = "$InstallDir;$env:Path"
    }

    try {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class DekitEnvironmentBroadcast {
    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
    public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
}
"@
        $Result = [UIntPtr]::Zero
        [void][DekitEnvironmentBroadcast]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, "Environment", 0x2, 5000, [ref]$Result)
    } catch {
        Write-Warning "Could not broadcast PATH update: $($_.Exception.Message)"
    }

    Write-Host "dekit installed to $(Join-Path $InstallDir 'dekit.exe')"
} finally {
    Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
}
