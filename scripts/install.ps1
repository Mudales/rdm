<#
.SYNOPSIS
  Installs rdm on Windows from the project's GitHub releases.

.DESCRIPTION
  Downloads the installer of the latest (or the given) release, checks it
  against the release's SHA256SUMS, installs it for the current user (no
  administrator rights needed) and registers the browser helper.

    irm https://raw.githubusercontent.com/Mudales/rdm/main/scripts/install.ps1 | iex

  With options:

    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Mudales/rdm/main/scripts/install.ps1))) -Version v0.2.0

.PARAMETER Version
  Release tag to install, e.g. v0.2.0. Default: the latest release.
.PARAMETER NoBrowser
  Do not register rdm with the browsers found.
.PARAMETER Uninstall
  Remove rdm. Downloads, settings and the download list are kept.
.PARAMETER From
  Use release files from this folder instead of downloading them.
#>
param(
  [string]$Version = '',
  [switch]$NoBrowser,
  [switch]$Uninstall,
  [string]$From = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is slow with it
$Repo = 'Mudales/rdm'
if ($PSVersionTable.PSVersion.Major -lt 6) {
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

function Find-InstallDir {
  $default = Join-Path $env:LOCALAPPDATA 'rdm'
  if (Test-Path (Join-Path $default 'rdm-host.exe')) { return $default }
  $keys = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
  foreach ($key in Get-ItemProperty $keys -ErrorAction SilentlyContinue) {
    if ($key.DisplayName -eq 'rdm' -and $key.InstallLocation) {
      return $key.InstallLocation.Trim('"')
    }
  }
  return $null
}

function Stop-Rdm {
  $running = Get-Process -Name 'rdm-gui', 'rdm' -ErrorAction SilentlyContinue
  if ($running) {
    Write-Host 'Closing the running rdm (downloads resume where they stopped)...'
    $running | ForEach-Object { $_.CloseMainWindow() | Out-Null }
    Start-Sleep -Seconds 3
    Get-Process -Name 'rdm-gui', 'rdm' -ErrorAction SilentlyContinue | Stop-Process -Force
  }
}

# --- Uninstall ------------------------------------------------------------------

if ($Uninstall) {
  $dir = Find-InstallDir
  if (-not $dir) { Write-Host 'rdm is not installed.'; return }
  Stop-Rdm
  $hostExe = Join-Path $dir 'rdm-host.exe'
  if (Test-Path $hostExe) { & $hostExe uninstall | Out-Null }
  $uninstaller = Join-Path $dir 'uninstall.exe'
  if (Test-Path $uninstaller) {
    $p = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "the uninstaller failed (exit code $($p.ExitCode))" }
  }
  Write-Host 'rdm was removed. Your downloads and settings were left in place.'
  return
}

# --- Which release --------------------------------------------------------------

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("rdm-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  if ($From) {
    $sums = Join-Path $From 'SHA256SUMS'
    if (-not (Test-Path $sums)) { throw "$sums not found" }
    $line = Select-String -Path $sums -Pattern 'rdm_(\S+)_x64-setup\.exe$' | Select-Object -First 1
    if (-not $line) { throw "no Windows installer listed in $sums" }
    $Version = 'v' + $line.Matches[0].Groups[1].Value
  } elseif (-not $Version) {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ 'User-Agent' = 'rdm-installer' }
    $Version = $release.tag_name
  }
  if (-not $Version.StartsWith('v')) { $Version = "v$Version" }
  $ver = $Version.TrimStart('v')

  function Get-ReleaseFile([string]$name) {
    $target = Join-Path $tmp $name
    if ($From) {
      Copy-Item -Path (Join-Path $From $name) -Destination $target
    } else {
      $url = "https://github.com/$Repo/releases/download/$Version/$name"
      Invoke-WebRequest -Uri $url -OutFile $target -UseBasicParsing
    }
    return $target
  }

  Write-Host "Installing rdm $ver..."
  $sumsFile = Get-ReleaseFile 'SHA256SUMS'
  $asset = "rdm_${ver}_x64-setup.exe"
  $installer = Get-ReleaseFile $asset

  $expected = $null
  foreach ($l in Get-Content $sumsFile) {
    $parts = $l -split '\s+', 2
    if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) { $expected = $parts[0].ToLowerInvariant() }
  }
  if (-not $expected) { throw "$asset is not listed in SHA256SUMS" }
  $actual = (Get-FileHash -Algorithm SHA256 -Path $installer).Hash.ToLowerInvariant()
  if ($actual -ne $expected) { throw "checksum mismatch for ${asset}: the download is damaged or was altered" }

  Stop-Rdm
  # /S: silent. The installer is per-user, so no elevation prompt.
  $p = Start-Process -FilePath $installer -ArgumentList '/S' -Wait -PassThru
  if ($p.ExitCode -ne 0) { throw "the installer failed (exit code $($p.ExitCode))" }

  $dir = Find-InstallDir
  if (-not $dir) { throw 'the installer finished, but rdm was not found where expected' }

  if (-not $NoBrowser) {
    $hostExe = Join-Path $dir 'rdm-host.exe'
    & $hostExe install | Out-Null
    if ($LASTEXITCODE -eq 0) {
      Write-Host 'Registered the browser helper with the browsers found (install the rdm extension to use it).'
    } else {
      Write-Host 'Could not register the browser helper; use Settings > Browser in rdm.'
    }
  }
  Write-Host "rdm $ver is installed in $dir. Start it from the Start menu."
} finally {
  Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
}
