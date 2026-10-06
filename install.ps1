# Installs the latest ritobin-tools release for Windows (user scope)

param(
    [string]$Owner = "LeagueToolkit",
    [string]$Repo  = "ritobin-tools",
    [string]$Channel = "windows-x64",
    [string]$InstallDir = "$env:LOCALAPPDATA\LeagueToolkit\ritobin-tools",
    # Add the Explorer right-click menu without asking.
    [switch]$ShellIntegration,
    # Do not add the Explorer right-click menu, and do not ask.
    [switch]$NoShellIntegration
)

$ErrorActionPreference = 'Stop'

# Whether to add the Explorer right-click menu. The switches decide when one is given. Otherwise
# the user is asked, and an install nobody can answer adds it.
function Confirm-ShellIntegration {
    if ($NoShellIntegration) { return $false }
    if ($ShellIntegration) { return $true }

    if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
        $answer = Read-Host 'Add ritobin-tools to the Windows Explorer right-click menu? [Y/n]'
        return ($answer -notmatch '^\s*(n|no)\s*$')
    }
    return $true
}

Write-Host "Installing ritobin-tools..." -ForegroundColor Cyan

if (!(Test-Path -LiteralPath $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

# Get latest release metadata
$releaseApi = "https://api.github.com/repos/$Owner/$Repo/releases/latest"
try {
    $release = Invoke-RestMethod -Uri $releaseApi -Headers @{ 'User-Agent' = 'ritobin-tools-installer' }
} catch {
    throw "Failed to query GitHub releases: $($_.Exception.Message)"
}

$tag = $release.tag_name
# Extract the first semantic version (handles tags like "v0.1.1" or "ritobin-tools-v0.1.1")
$match = [regex]::Match($tag, '\d+\.\d+\.\d+([\-\+][A-Za-z0-9\.-]+)?')
$version = if ($match.Success) { $match.Value } else { $tag.TrimStart('v') }

$assetName = "ritobin-tools-$version-$Channel.zip"
$asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
if (-not $asset) {
    # Fallback: find any ritobin-tools asset matching the channel
    $pattern = "^ritobin-tools-.*-" + [regex]::Escape($Channel) + "\.zip$"
    $asset = $release.assets | Where-Object { $_.name -match $pattern } | Select-Object -First 1
}
if (-not $asset) {
    throw "Could not find asset matching '$assetName' (channel $Channel) in the latest release."
}
if ($asset.name -ne $assetName) { $assetName = $asset.name }

$zipPath = Join-Path $env:TEMP $assetName
Write-Host "Downloading $assetName ($version)..." -ForegroundColor Yellow
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zipPath -UseBasicParsing

Write-Host "Extracting to $InstallDir" -ForegroundColor Yellow
Expand-Archive -Path $zipPath -DestinationPath $InstallDir -Force

# Create a shim directory so PATH is simple and stable
$binDir = Join-Path $InstallDir 'bin'
if (!(Test-Path -LiteralPath $binDir)) { New-Item -ItemType Directory -Path $binDir | Out-Null }

# Ensure the executable exists
$exePath = Join-Path $InstallDir 'ritobin-tools.exe'
if (!(Test-Path -LiteralPath $exePath)) {
    throw "ritobin-tools.exe not found after extraction: $exePath"
}

# Place a thin cmd shim in bin to avoid spaces in paths and simplify PATH updates
$shimCmd = @"
@echo off
"$exePath" %*
"@
Set-Content -LiteralPath (Join-Path $binDir 'ritobin-tools.cmd') -Value $shimCmd -Encoding Ascii -Force

# Add to user PATH if missing
$currentPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($currentPath -split ';' | Where-Object { $_ -eq $binDir })) {
    $newPath = if ([string]::IsNullOrEmpty($currentPath)) { $binDir } else { "$currentPath;$binDir" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added to PATH (User): $binDir" -ForegroundColor Green
} else {
    Write-Host "PATH already contains: $binDir" -ForegroundColor Green
}

# The menu is for the current user and needs no administrator rights. Failing to add it does not
# fail the install.
if (Confirm-ShellIntegration) {
    # The tool says what it added. Its output is not redirected: Windows PowerShell turns the
    # redirected error stream of a program into errors, which would stop this script.
    & $exePath shell install
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Remove the right-click menu with: ritobin-tools shell uninstall" -ForegroundColor Green
    } else {
        Write-Host "Could not add the Explorer right-click menu. Try: ritobin-tools shell install" -ForegroundColor Yellow
    }
} else {
    Write-Host "Skipped the Explorer right-click menu (add it later with: ritobin-tools shell install)" -ForegroundColor Yellow
}

Write-Host "Installed ritobin-tools $version to $InstallDir" -ForegroundColor Green
Write-Host "Open a new terminal and run: ritobin-tools --help" -ForegroundColor Cyan

