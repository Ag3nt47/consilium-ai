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
    $replacement = Start-Process -FilePath $target -WorkingDirectory $workDir -WindowStyle Normal -PassThru
    Write-RelaunchLog "started replacement process $($replacement.Id)"
    if ($replacement.WaitForExit(5000)) {
        $code = $replacement.ExitCode
        Write-RelaunchLog "replacement process $($replacement.Id) exited within 5 s with code $code"
        if ($code -ne 0) { exit 1 }
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
    }
}
