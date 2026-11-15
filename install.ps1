# Installs the latest Consilium release on Windows (per user, no admin rights needed).
#
#   irm https://raw.githubusercontent.com/Ag3nt47/consilium-ai/main/install.ps1 | iex
#
# Downloads only from this repository's GitHub releases and never asks for credentials.
$ErrorActionPreference = 'Stop'
$Repo = 'Ag3nt47/consilium-ai'

Write-Host '==> Finding the latest Consilium release...' -ForegroundColor Yellow
$release = Invoke-RestMethod -Headers @{ Accept = 'application/vnd.github+json' } "https://api.github.com/repos/$Repo/releases/latest"
$asset = $release.assets |
  Where-Object { $_.name -match '-setup\.exe$' -and $_.browser_download_url -like "https://github.com/$Repo/releases/download/*" } |
  Select-Object -First 1
if (-not $asset) { throw 'This release has no Windows installer.' }

$file = Join-Path $env:TEMP $asset.name
Write-Host "==> Downloading Consilium $($release.tag_name)..." -ForegroundColor Yellow
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $file -UseBasicParsing

Write-Host '==> Installing...' -ForegroundColor Yellow
# The NSIS installer installs per user and installs WebView2 only if it's missing.
Start-Process -FilePath $file -ArgumentList '/S' -Wait
Remove-Item $file -Force

# The terminal command: `consilium chat` and friends (see the README).
$exe = Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA 'Consilium') -Filter '*.exe' -ErrorAction SilentlyContinue |
  Where-Object { $_.Name -notmatch 'uninstall' } | Select-Object -First 1
if ($exe) { Start-Process -FilePath $exe.FullName -ArgumentList 'install-cli' -Wait -NoNewWindow }

Write-Host '==> Installed. Start Consilium from the Start menu, or type consilium chat in a new terminal.' -ForegroundColor Green
Write-Host '    Press "Set up AIs" in the app if ChatGPT or Claude isn''t ready yet.'
