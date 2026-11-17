# SessionSafety.ps1: Session change protection and automatic merge script for Consilium (Windows).
# Protects in-progress session edits and untracked files across app restarts and updates,
# automatically merging them back into the updated codebase.

param(
  [Parameter(Position=0)][ValidateSet('save','merge','status','snapshot','restore','check')][string]$Action = 'status',
  [Parameter(Position=1)][string]$RepoPath
)

$HomeDir = $env:USERPROFILE
$AppData = $env:APPDATA
$SafetyDir = Join-Path $AppData 'io.github.ag3nt47.consilium\safety'
if (-not (Test-Path $SafetyDir)) { New-Item -ItemType Directory -Path $SafetyDir -Force | Out-Null }
$LatestFile = Join-Path $SafetyDir 'latest.json'

function Get-RepoDir($customPath) {
  if ($customPath -and (Test-Path $customPath)) { return (Resolve-Path $customPath).Path }
  $top = git rev-parse --show-toplevel 2>$null
  if ($top) { return $top.Trim() }
  $def = Join-Path $HomeDir 'Projects\consilium-ai'
  if (Test-Path $def) { return $def }
  return (Get-Location).Path
}

$repo = Get-RepoDir $RepoPath

if ($Action -in @('save', 'snapshot')) {
  Write-Output "Checking repository at $repo..."
  $statusLines = @(git -C $repo status --porcelain 2>$null)
  if (-not $statusLines -or $statusLines.Count -eq 0) {
    Write-Output "Working tree clean; no uncommitted session changes to save."
    return
  }

  $ts = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  $sessionDir = Join-Path $SafetyDir "session-$ts"
  $untrackedDir = Join-Path $sessionDir "untracked"
  New-Item -ItemType Directory -Path $untrackedDir -Force | Out-Null

  $untrackedFiles = [System.Collections.ArrayList]::new()
  $trackedChanges = [System.Collections.ArrayList]::new()

  foreach ($l in $statusLines) {
    if (-not $l.Trim()) { continue }
    $code = $l.Substring(0, 2)
    $filePath = $l.Substring(3).Trim()
    if ($code -eq '??') {
      [void]$untrackedFiles.Add($filePath)
      $src = Join-Path $repo $filePath
      $dst = Join-Path $untrackedDir $filePath
      $dstParent = Split-Path $dst
      if (-not (Test-Path $dstParent)) { New-Item -ItemType Directory -Path $dstParent -Force | Out-Null }
      if (Test-Path $src -PathType Leaf) { Copy-Item -Path $src -Destination $dst -Force }
      elseif (Test-Path $src -PathType Container) { Copy-Item -Path $src -Destination $dst -Recurse -Force }
    } else {
      [void]$trackedChanges.Add($filePath)
    }
  }

  $stashHash = ''
  if ($trackedChanges.Count -gt 0) {
    $stashHash = (git -C $repo stash create "consilium-session-safety-$ts" 2>$null).Trim()
    if ($stashHash) {
      git -C $repo branch -f consilium/session-safety $stashHash 2>$null | Out-Null
    }
  }

  $meta = [pscustomobject]@{
    timestamp = $ts
    repo_dir = $repo
    stash_hash = $stashHash
    tracked_changes = $trackedChanges
    untracked_files = $untrackedFiles
    session_dir = $sessionDir
  }

  $meta | ConvertTo-Json -Depth 5 | Set-Content -Path $LatestFile -Encoding UTF8

  Write-Output "✓ Session safety snapshot saved (timestamp $ts):"
  if ($trackedChanges.Count -gt 0) {
    Write-Output "  - $($trackedChanges.Count) tracked file(s) snapshot commit: $($stashHash.Substring(0, [Math]::Min(10, $stashHash.Length)))"
  }
  if ($untrackedFiles.Count -gt 0) {
    Write-Output "  - $($untrackedFiles.Count) untracked file(s) backed up to $untrackedDir"
  }
  Write-Output "Your work is safely protected against updates and restarts."
  return
}

if ($Action -in @('merge', 'restore')) {
  if (-not (Test-Path $LatestFile)) {
    Write-Output "No saved session snapshot found to merge."
    return
  }
  $meta = Get-Content $LatestFile -Raw | ConvertFrom-Json
  $targetRepo = if ($meta.repo_dir) { $meta.repo_dir } else { $repo }
  $stashHash = $meta.stash_hash
  $untrackedFiles = @($meta.untracked_files)
  $untrackedDir = Join-Path $meta.session_dir 'untracked'

  Write-Output "Restoring & merging session snapshot from $($meta.timestamp) into $targetRepo..."

  # 1. Restore untracked files
  if ((Test-Path $untrackedDir) -and $untrackedFiles.Count -gt 0) {
    foreach ($f in $untrackedFiles) {
      $src = Join-Path $untrackedDir $f
      $dst = Join-Path $targetRepo $f
      if (Test-Path $src) {
        $dstParent = Split-Path $dst
        if (-not (Test-Path $dstParent)) { New-Item -ItemType Directory -Path $dstParent -Force | Out-Null }
        if (Test-Path $src -PathType Leaf) { Copy-Item -Path $src -Destination $dst -Force }
        elseif (Test-Path $src -PathType Container) { Copy-Item -Path $src -Destination $dst -Recurse -Force }
      }
    }
    Write-Output "✓ Restored $($untrackedFiles.Count) untracked file(s)."
  }

  # 2. 3-way merge tracked changes
  if ($stashHash) {
    Write-Output "Merging tracked changes ($($stashHash.Substring(0, [Math]::Min(10, $stashHash.Length)))) with 3-way merge..."
    $out = git -C $targetRepo stash apply $stashHash 2>&1
    if ($LASTEXITCODE -eq 0) {
      Write-Output "✓ All session changes successfully merged into working tree with 0 conflicts!"
    } else {
      Write-Output "! Automatic merge encountered conflicts in some files:"
      Write-Output $out
      Write-Output "Conflict markers (<<<<<<< / ======= / >>>>>>>) have been placed in the affected files."
      Write-Output "Your original backup is safely preserved in consilium/session-safety."
    }
  } else {
    Write-Output "✓ All untracked session files restored; no tracked changes were pending."
  }
  return
}

if ($Action -in @('status', 'check')) {
  Write-Output "Repository: $repo"
  $st = git -C $repo status -s 2>$null
  if ($st) {
    Write-Output "Current uncommitted changes:`n$st"
  } else {
    Write-Output "Current working tree: clean."
  }

  if (Test-Path $LatestFile) {
    $meta = Get-Content $LatestFile -Raw | ConvertFrom-Json
    Write-Output "`nLast saved safety snapshot: $($meta.timestamp)"
    Write-Output "  Tracked changes: $(@($meta.tracked_changes).Count) file(s)"
    Write-Output "  Untracked files: $(@($meta.untracked_files).Count) file(s)"
    Write-Output "  Stash commit: $($meta.stash_hash)"
  } else {
    Write-Output "`nNo saved safety snapshots."
  }
}
