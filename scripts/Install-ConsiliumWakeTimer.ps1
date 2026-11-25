[CmdletBinding(SupportsShouldProcess)]
param(
    [ValidateSet('Install', 'Inspect', 'RunNow', 'Disable')]
    [string]$Action = 'Install'
)

$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'The Consilium wake timer uses Windows Task Scheduler.'
}

$taskName = 'Consilium Watchdog Signal Wake'
$taskPath = '\'
$description = 'Consilium watchdog wake window. Provides a minute for the running Signal listener to reconnect.'
$owner = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$localData = [Environment]::GetFolderPath('LocalApplicationData')
if (!$localData) { throw 'Windows did not provide the current user profile folder.' }
$workDir = Join-Path $localData 'io.github.ag3nt47.consilium\watchdog-timer'
$helper = Join-Path $workDir 'signal-wake-window.js'
$wscript = Join-Path ([Environment]::SystemDirectory) 'wscript.exe'

function Get-OwnWakeTask {
    $task = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath -ErrorAction SilentlyContinue
    if (!$task) { return $null }

    $principal = $task.Principal.UserId
    if ($principal -and $principal -notlike 'S-1-*') {
        $principal = ([Security.Principal.NTAccount]$principal).Translate([Security.Principal.SecurityIdentifier]).Value
    }
    if ($principal -ne $owner -or $task.Description -ne $description) {
        throw "A different task already uses the name '$taskName'."
    }
    return $task
}

function Show-WakeTask {
    $task = Get-OwnWakeTask
    if (!$task) {
        [pscustomobject]@{ Task = $taskName; State = 'Not installed' }
        return
    }
    $info = Get-ScheduledTaskInfo -TaskName $taskName -TaskPath $taskPath
    [pscustomobject]@{
        Task = $taskName
        State = $task.State
        NextRun = $info.NextRunTime
        LastRun = $info.LastRunTime
        LastResult = $info.LastTaskResult
        WakeToRun = $task.Settings.WakeToRun
        ACOnly = ($task.Settings.DisallowStartIfOnBatteries -and $task.Settings.StopIfGoingOnBatteries)
        Repeat = $task.Triggers[0].Repetition.Interval
        WindowSeconds = 60
    }
}

switch ($Action) {
    Inspect {
        Show-WakeTask
        return
    }
    RunNow {
        if (!(Get-OwnWakeTask)) { throw 'Install the wake timer first.' }
        if ($PSCmdlet.ShouldProcess($taskName, 'Start a one-minute test window now')) {
            Start-ScheduledTask -TaskName $taskName -TaskPath $taskPath
            Show-WakeTask
        }
        return
    }
    Disable {
        if ((Get-OwnWakeTask) -and $PSCmdlet.ShouldProcess($taskName, 'Disable the hourly wake timer')) {
            Disable-ScheduledTask -TaskName $taskName -TaskPath $taskPath | Out-Null
        }
        Show-WakeTask
        return
    }
}

$null = Get-OwnWakeTask
if (!(Test-Path -LiteralPath $wscript -PathType Leaf)) {
    throw 'Windows Script Host is unavailable.'
}
$start = (Get-Date).AddHours(1).ToString('yyyy-MM-ddTHH:mm:ss', [Globalization.CultureInfo]::InvariantCulture)
$xmlCommand = [Security.SecurityElement]::Escape($wscript)
$xmlArguments = [Security.SecurityElement]::Escape(('//B //Nologo //T:75 "{0}"' -f $helper))
$xmlDirectory = [Security.SecurityElement]::Escape($workDir)
$xmlDescription = [Security.SecurityElement]::Escape($description)
$taskXml = @"
<?xml version="1.0"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>$xmlDescription</Description></RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition><Interval>PT1H</Interval></Repetition>
      <StartBoundary>$start</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="CurrentUser">
      <UserId>$owner</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>true</StopIfGoingOnBatteries>
    <StartWhenAvailable>false</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <WakeToRun>true</WakeToRun>
    <ExecutionTimeLimit>PT2M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="CurrentUser">
    <Exec>
      <Command>$xmlCommand</Command>
      <Arguments>$xmlArguments</Arguments>
      <WorkingDirectory>$xmlDirectory</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@

if ($PSCmdlet.ShouldProcess($taskName, 'Register an hourly AC-only wake timer')) {
    [void][IO.Directory]::CreateDirectory($workDir)
    # Task Scheduler keeps the computer awake while the action runs. The existing Signal
    # connector handles reconnecting. Windows's unattended sleep policy takes over afterward.
    [IO.File]::WriteAllText($helper, 'WScript.Sleep(60000);', [Text.Encoding]::ASCII)
    Register-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Xml $taskXml -Force | Out-Null
    Show-WakeTask
}
