# Windows entry point for the existing cleaner.
# Preview: powershell.exe -NoProfile -File scripts\clean.ps1 --dry-run
$ErrorActionPreference = 'Stop'
$cleanerPath = Join-Path $PSScriptRoot 'clean.mjs'
$nodeCommand = Get-Command node.exe -CommandType Application -ErrorAction Stop
& $nodeCommand.Source $cleanerPath @args
exit $LASTEXITCODE
