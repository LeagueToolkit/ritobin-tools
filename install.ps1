# Installs the latest ritobin-tools release for the current Windows user.

param(
    [string]$Owner = "LeagueToolkit",
    [string]$Repo  = "ritobin-tools",
    [string]$Channel = "windows-x64",
    [string]$InstallDir = "$env:LOCALAPPDATA\LeagueToolkit\ritobin-tools",
    # Add the Explorer context menu without prompting.
    [switch]$ShellIntegration,
    # Do not add the Explorer context menu, and do not prompt.
    [switch]$NoShellIntegration
)

$ErrorActionPreference = 'Stop'

# Returns $true if the Explorer context menu should be added. A switch decides if one is set.
# Otherwise the user is prompted. A non-interactive install adds the menu without a prompt.
function Confirm-ShellIntegration {
    if ($NoShellIntegration) { return $false }
    if ($ShellIntegration) { return $true }

    if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
        $answer = Read-Host 'Add ritobin-tools to the Windows Explorer context menu? [Y/n]'
        return ($answer -notmatch '^\s*(n|no)\s*$')
    }
    return $true
}

Write-Host "Installing ritobin-tools..." -ForegroundColor Cyan

if (!(Test-Path -LiteralPath $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

# Get the metadata of the latest release.
$releaseApi = "https://api.github.com/repos/$Owner/$Repo/releases/latest"
try {
    $release = Invoke-RestMethod -Uri $releaseApi -Headers @{ 'User-Agent' = 'ritobin-tools-installer' }
} catch {
    throw "Failed to query GitHub releases: $($_.Exception.Message)"
}

$tag = $release.tag_name
# Extract the first semantic version from the tag. Tags look like "v0.1.1" or
# "ritobin-tools-v0.1.1".
$match = [regex]::Match($tag, '\d+\.\d+\.\d+([\-\+][A-Za-z0-9\.-]+)?')
$version = if ($match.Success) { $match.Value } else { $tag.TrimStart('v') }

$assetName = "ritobin-tools-$version-$Channel.zip"
$asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
if (-not $asset) {
    # Fall back to any ritobin-tools asset for the channel.
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

# Create the bin directory. PATH gets this directory, which contains only the shim.
$binDir = Join-Path $InstallDir 'bin'
if (!(Test-Path -LiteralPath $binDir)) { New-Item -ItemType Directory -Path $binDir | Out-Null }

# Fail if the archive did not contain the executable.
$exePath = Join-Path $InstallDir 'ritobin-tools.exe'
if (!(Test-Path -LiteralPath $exePath)) {
    throw "ritobin-tools.exe not found after extraction: $exePath"
}

# Write a cmd shim to the bin directory. The shim forwards all arguments to the executable.
$shimCmd = @"
@echo off
"$exePath" %*
"@
Set-Content -LiteralPath (Join-Path $binDir 'ritobin-tools.cmd') -Value $shimCmd -Encoding Ascii -Force

# Add the bin directory to the user PATH if it is missing.
$currentPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($currentPath -split ';' | Where-Object { $_ -eq $binDir })) {
    $newPath = if ([string]::IsNullOrEmpty($currentPath)) { $binDir } else { "$currentPath;$binDir" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added to PATH (User): $binDir" -ForegroundColor Green
} else {
    Write-Host "PATH already contains: $binDir" -ForegroundColor Green
}

# The menu is installed for the current user and does not require administrator rights. If
# adding it fails, the install still succeeds.
if (Confirm-ShellIntegration) {
    # The tool logs the result. Its output is not redirected, because Windows PowerShell converts
    # the redirected error stream of a native program into error records, and those would stop
    # this script.
    & $exePath shell install
    if ($LASTEXITCODE -eq 0) {
        Write-Host "To remove the context menu, run: ritobin-tools shell uninstall" -ForegroundColor Green
    } else {
        Write-Host "Failed to add the Explorer context menu. To retry, run: ritobin-tools shell install" -ForegroundColor Yellow
    }
} else {
    Write-Host "Skipped the Explorer context menu. To add it later, run: ritobin-tools shell install" -ForegroundColor Yellow
}

Write-Host "Installed ritobin-tools $version to $InstallDir" -ForegroundColor Green
Write-Host "Open a new terminal and run: ritobin-tools --help" -ForegroundColor Cyan

