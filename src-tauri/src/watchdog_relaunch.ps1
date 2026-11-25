$ErrorActionPreference = 'Stop'

function Write-RelaunchLog([string]$message) {
    try {
        $stamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        [IO.File]::AppendAllText($logPath, "$stamp watchdog relaunch: $message" + [Environment]::NewLine)
    } catch {
        Write-Error -ErrorRecord $_ -ErrorAction Continue
    }
}

function Get-RelaunchBlock {
    if (Test-Path -LiteralPath $installLockPath) { return 'update in progress' }
    if (Test-Path -LiteralPath $quitFlagPath) { return 'deliberate quit' }
    return $null
}

function Get-RetryBlock {
    $block = Get-RelaunchBlock
    if ($block) { return $block }

    $name = [IO.Path]::GetFileNameWithoutExtension($target)
    foreach ($process in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
        try {
            if (!$process.HasExited -and [String]::Equals($process.Path, $target, [StringComparison]::OrdinalIgnoreCase)) {
                return 'another instance of the same executable is running'
            }
        } catch {
            # A process that exited during enumeration has no live window to protect.
        } finally {
            $process.Dispose()
        }
    }
    return $null
}

function Save-StartupFailureLog([string]$pending, [int]$attempt) {
    if (!(Test-Path -LiteralPath $pending)) { return }
    $failed = [IO.Path]::ChangeExtension($readyPath, "launch$attempt.stderr.log")
    try {
        Move-Item -LiteralPath $pending -Destination $failed -ErrorAction Stop
        Write-RelaunchLog "retained stderr in $([IO.Path]::GetFileName($failed))"
    } catch {
        # A child process may still hold the inherited file. Copy a bounded tail if we can read it.
        try {
            $stream = [IO.File]::Open($pending, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
            try {
                $stream.Position = [Math]::Max(0, $stream.Length - 65536)
                $bytes = New-Object byte[] 65536
                $count = $stream.Read($bytes, 0, $bytes.Length)
                $saved = [IO.File]::Open($failed, [IO.FileMode]::CreateNew)
                try { $saved.Write($bytes, 0, $count) } finally { $saved.Dispose() }
            } finally {
                $stream.Dispose()
            }
            Write-RelaunchLog "retained stderr tail in $([IO.Path]::GetFileName($failed))"
        } catch {
            Write-RelaunchLog "stderr is still in the active file $([IO.Path]::GetFileName($pending))"
        }
    }
}

$logDir = [IO.Path]::GetDirectoryName($logPath)
$startupLogs = @(
    (Join-Path $logDir 'relaunch-attempt-1.stderr.pending.log')
    (Join-Path $logDir 'relaunch-attempt-2.stderr.pending.log')
)
$oldProcess = $null
$replacement = $null
$finished = $false
try {
    $oldProcess = Get-Process -Id $oldPid
    # Keep this process handle open so waiting and stopping cannot target a reused PID.
    $null = $oldProcess.Handle
    if ($oldProcess.StartTime.ToUniversalTime().Ticks -ne $oldStartedTicks -or
        -not [String]::Equals($oldProcess.Path, $target, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'original process identity changed'
    }

    $block = Get-RelaunchBlock
    if ($block) {
        Write-RelaunchLog "canceled before arming ($block)"
        $finished = $true
        return
    }
    [IO.File]::WriteAllText($readyPath, [string]$PID)
    Write-RelaunchLog "helper $PID ready for process $oldPid"

    $armWait = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $armPath)) {
        if ($armWait.Elapsed.TotalSeconds -ge 45) {
            Write-RelaunchLog "helper $PID was not armed"
            $finished = $true
            return
        }
        Start-Sleep -Milliseconds 100
    }

    if ($closeWindow -and -not $oldProcess.HasExited) {
        $block = Get-RelaunchBlock
        if ($block) {
            Write-RelaunchLog "canceled before closing process $oldPid ($block)"
            $finished = $true
            return
        }
        try {
            if ($oldProcess.CloseMainWindow()) {
                Write-RelaunchLog "asked process $oldPid to close its window"
            } else {
                Write-RelaunchLog "process $oldPid has no closeable main window"
            }
        } catch {
            Write-RelaunchLog "could not request a window close for process ${oldPid}: $($_.Exception.Message)"
        }
    }

    if (-not $oldProcess.WaitForExit(15000)) {
        $block = Get-RelaunchBlock
        if ($block) {
            Write-RelaunchLog "canceled before stopping process $oldPid ($block)"
            $finished = $true
            return
        }
        if (-not $forceStalled) { throw "process $oldPid did not exit" }
        Write-RelaunchLog "stopping stalled process $oldPid after waiting for its exit"
        try {
            $oldProcess.Kill()
        } catch {
            if (-not $oldProcess.HasExited) { throw }
        }
        if (-not $oldProcess.WaitForExit(5000)) { throw "process $oldPid did not stop" }
    }

    $block = Get-RelaunchBlock
    if ($block) {
        Write-RelaunchLog "canceled before launching a replacement ($block)"
        $finished = $true
        return
    }
    $env:CONSILIUM_DATA = $profile
    $workDir = [IO.Path]::GetDirectoryName($target)

    for ($attempt = 1; $attempt -le 2; $attempt++) {
        $stderrPath = $startupLogs[$attempt - 1]
        # Windows keeps redirected stderr open for the process lifetime. Reuse a slot only
        # after its previous owner releases the handle.
        Remove-Item -LiteralPath $stderrPath -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $stderrPath) {
            Write-RelaunchLog "startup stderr slot $attempt is busy; launching without capture"
            $replacement = Start-Process -FilePath $target -WorkingDirectory $workDir -WindowStyle Normal -PassThru
            $stderrPath = $null
        } else {
            $replacement = Start-Process -FilePath $target -WorkingDirectory $workDir -WindowStyle Normal -PassThru -RedirectStandardError $stderrPath
        }
        # PowerShell 5.1 can otherwise lose the exit code when a process exits quickly.
        try { $null = $replacement.Handle } catch { }
        Write-RelaunchLog "started replacement process $($replacement.Id) on attempt $attempt"
        $earlyExit = $replacement.WaitForExit(5000)
        $code = if ($earlyExit) { $replacement.ExitCode } else { $null }
        $replacement.Dispose()
        $replacement = $null

        if ($earlyExit -and $null -ne $code) {
            Write-RelaunchLog "replacement on attempt $attempt exited within 5 s with code $code"
        } elseif ($earlyExit) {
            Write-RelaunchLog "replacement on attempt $attempt exited early with no available exit code"
        }
        if ($null -eq $code -or $code -eq 0) {
            if ($stderrPath) { Remove-Item -LiteralPath $stderrPath -ErrorAction SilentlyContinue }
            break
        }
        if ($stderrPath) { Save-StartupFailureLog $stderrPath $attempt }
        if ($attempt -eq 2) {
            $finished = $true
            exit 1
        }
        Start-Sleep -Seconds 4
        $block = Get-RetryBlock
        if ($block) {
            Write-RelaunchLog "canceled the replacement retry ($block)"
            break
        }
    }
    $finished = $true
} catch {
    Write-RelaunchLog "failed for process ${oldPid}: $($_.Exception.Message)"
    exit 1
} finally {
    if ($null -ne $replacement) { $replacement.Dispose() }
    if ($null -ne $oldProcess) { $oldProcess.Dispose() }
    if ($finished) {
        Remove-Item -LiteralPath $readyPath, $armPath, $PSCommandPath -ErrorAction SilentlyContinue
        # Remove any unlocked capture from a nonfailing process. A live child's fixed slot
        # is eligible for cleanup once it exits and releases the operating-system handle.
        Remove-Item -LiteralPath $startupLogs -ErrorAction SilentlyContinue
    }
}
