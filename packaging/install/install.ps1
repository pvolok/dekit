# Installs dekit. `dekit update` runs this same script.
#
#   DEKIT_VERSION       latest (default), canary, or a version like 1.2.3
#   DEKIT_INSTALL_DIR   where dekit.exe goes (default: ~\.local\bin)
#   DEKIT_RELEASES_URL  a mirror of https://github.com/pvolok/dekit/releases

# The block keeps these settings out of the session that ran `irm | iex`.
& {
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $Releases = if ($env:DEKIT_RELEASES_URL) { $env:DEKIT_RELEASES_URL } else { "https://github.com/pvolok/dekit/releases" }
    $Version = if ($env:DEKIT_VERSION) { $env:DEKIT_VERSION } else { "latest" }
    $InstallDir = if ($env:DEKIT_INSTALL_DIR) { $env:DEKIT_INSTALL_DIR } else { Join-Path $HOME ".local\bin" }

    $Cpu = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    if ($Cpu -ne "X64" -and $Cpu -ne "Arm64") {
        throw "dekit: unsupported CPU: $Cpu"
    }
    # Arm64 runs the x64 build under Windows' emulation.
    $Asset = "dekit-x86_64-pc-windows-msvc.zip"

    $Url = if ($Version -eq "latest") {
        "$Releases/latest/download"
    } elseif ($Version -eq "canary" -or $Version.StartsWith("v")) {
        "$Releases/download/$Version"
    } else {
        "$Releases/download/v$Version"
    }

    $Temp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
    New-Item -ItemType Directory -Path $Temp | Out-Null

    try {
        $Archive = Join-Path $Temp $Asset
        $Sums = Join-Path $Temp "SHA256SUMS"
        Invoke-WebRequest -UseBasicParsing -Uri "$Url/$Asset" -OutFile $Archive
        Invoke-WebRequest -UseBasicParsing -Uri "$Url/SHA256SUMS" -OutFile $Sums

        $Expected = $null
        foreach ($Line in Get-Content $Sums) {
            $Parts = $Line -split "\s+"
            if ($Parts[-1].TrimStart("*") -eq $Asset) {
                $Expected = $Parts[0]
                break
            }
        }
        if (-not $Expected) {
            throw "dekit: SHA256SUMS has no checksum for $Asset"
        }
        if ((Get-FileHash -Algorithm SHA256 $Archive).Hash -ne $Expected) {
            throw "dekit: checksum mismatch for $Asset"
        }

        Expand-Archive -Path $Archive -DestinationPath $Temp -Force
        $New = Join-Path $Temp "dekit.exe"
        $Installed = & $New --version
        if ($LASTEXITCODE -ne 0) {
            throw "dekit: the downloaded binary does not run on this machine"
        }

        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        $Target = Join-Path $InstallDir "dekit.exe"
        $Fresh = -not (Test-Path $Target)

        # A running dekit.exe cannot be overwritten or deleted, but it can be
        # renamed. What is left of it goes away on a later install, once
        # nothing runs it.
        foreach ($File in Get-ChildItem $InstallDir -Filter "dekit.exe.old-*") {
            try { Remove-Item $File.FullName -Force } catch {}
        }
        $Old = "$Target.old-$([System.Guid]::NewGuid().ToString('N'))"
        if (-not $Fresh) {
            Move-Item $Target $Old
        }
        try {
            Move-Item $New $Target
        } catch {
            if (-not $Fresh) {
                Move-Item $Old $Target
            }
            throw
        }
        if (-not $Fresh) {
            try { Remove-Item $Old -Force } catch {}
        }

        Write-Host "$Installed installed to $Target"

        # Only a first install touches PATH.
        if ($Fresh) {
            $Slashes = [char[]]"\/"
            $Dir = $InstallDir.TrimEnd($Slashes)
            $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
            $Entries = @($UserPath -split ";" | ForEach-Object { $_.TrimEnd($Slashes) })
            if ($Entries -notcontains $Dir) {
                $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
                # This also tells running programs that the environment changed.
                [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
            }
            if (($env:Path -split ";") -notcontains $InstallDir) {
                $env:Path = "$InstallDir;$env:Path"
            }
        }
    } finally {
        Remove-Item -Recurse -Force $Temp -ErrorAction SilentlyContinue
    }
}
