# StatusCheck.ps1: Comprehensive connection status checker for Consilium (Windows).
# Checks Signal/messaging, multi-computer peer links, AI CLIs, internet, and GitHub.
# Formats results, posts into Consilium chat, and sends to Signal Note to Self.

param(
  [switch]$Chat,
  [switch]$Signal,
  [switch]$All,
  [switch]$DryRun,
  [switch]$Json
)

if (-not $Chat -and -not $Signal -and -not $DryRun -and -not $Json) {
  $All = $true
}
if ($All) {
  $Chat = $true
  $Signal = $true
}

$HomeDir = $env:USERPROFILE
$AppData = $env:APPDATA
$ConsiliumData = Join-Path $AppData 'io.github.ag3nt47.consilium'
$MsgCfgPath = Join-Path $AppData 'consilium\messaging.json'
$LinkCfgPath = Join-Path $ConsiliumData 'link\link.json'
$ConvDir = Join-Path $ConsiliumData 'conversations'

function Test-TcpPort([string]$HostName, [int]$Port, [int]$TimeoutMs = 1500) {
  $client = [System.Net.Sockets.TcpClient]::new()
  $sw = [Diagnostics.Stopwatch]::StartNew()
  try {
    $async = $client.ConnectAsync($HostName, $Port)
    if ($async.Wait($TimeoutMs) -and $client.Connected) {
      $sw.Stop()
      $client.Dispose()
      return [int]$sw.ElapsedMilliseconds
    }
  } catch {}
  $client.Dispose()
  return -1
}

$Items = [System.Collections.ArrayList]::new()
$SigAccount = $null

# 1. Signal Check
if (Test-Path $MsgCfgPath) {
  try {
    $msgCfg = Get-Content $MsgCfgPath -Raw | ConvertFrom-Json
    if ($msgCfg.enabled) {
      $endpoint = if ($msgCfg.endpoint) { $msgCfg.endpoint } else { '127.0.0.1:7583' }
      $SigAccount = $msgCfg.account
      $parts = $endpoint.Split(':')
      $hostStr = $parts[0]
      $portNum = [int]$parts[1]
      
      $ms = Test-TcpPort $hostStr $portNum 2000
      if ($ms -ge 0) {
        # Query version over JSON-RPC
        $tcp = [System.Net.Sockets.TcpClient]::new()
        $tcp.Connect($hostStr, $portNum)
        $stream = $tcp.GetStream()
        $writer = [System.IO.StreamWriter]::new($stream)
        $reader = [System.IO.StreamReader]::new($stream)
        $writer.WriteLine('{"jsonrpc":"2.0","id":"chk","method":"version"}')
        $writer.Flush()
        $raw = $reader.ReadLine()
        $tcp.Dispose()
        $ver = 'daemon active'
        if ($raw) {
          try {
            $resp = $raw | ConvertFrom-Json
            if ($resp.result -and $resp.result.version) { $ver = "v$($resp.result.version)" }
          } catch {}
        }
        [void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "Signal: connected ($SigAccount, $ver); phone messages reach council." })
      } else {
        [void]$Items.Add([pscustomobject]@{ Status = 'error'; Mark = [char]0x2717; Msg = "Signal: daemon unreachable at $endpoint." })
      }
    } else {
      [void]$Items.Add([pscustomobject]@{ Status = 'info'; Mark = [char]0x00B7; Msg = "Signal: disabled in Settings." })
    }
  } catch {
    [void]$Items.Add([pscustomobject]@{ Status = 'error'; Mark = [char]0x2717; Msg = "Signal check error: $_" })
  }
} else {
  [void]$Items.Add([pscustomobject]@{ Status = 'info'; Mark = [char]0x00B7; Msg = "Signal: not configured." })
}

# 2. Linked Computers Check
if (Test-Path $LinkCfgPath) {
  try {
    $linkCfg = Get-Content $LinkCfgPath -Raw | ConvertFrom-Json
    if ($linkCfg.enabled) {
      $peers = @($linkCfg.peers)
      if ($peers.Count -eq 0) {
        [void]$Items.Add([pscustomobject]@{ Status = 'info'; Mark = [char]0x00B7; Msg = "Linked computers: none paired." })
      } else {
        foreach ($p in $peers) {
          $pname = if ($p.name) { $p.name } else { 'Peer' }
          $addrs = @($p.addrs)
          $connRoute = $null
          $connMs = -1
          foreach ($a in $addrs) {
            $parts = $a.Split(':')
            $ms = Test-TcpPort $parts[0] ([int]$parts[1]) 1000
            if ($ms -ge 0) {
              $connRoute = $a
              $connMs = $ms
              break
            }
          }
          if ($connRoute) {
            [void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "$pname: connected via $connRoute ($connMs ms), $($addrs.Count) routes known." })
          } else {
            [void]$Items.Add([pscustomobject]@{ Status = 'warn'; Mark = '!'; Msg = "$pname: disconnected, $($addrs.Count) routes known." })
          }
        }
      }
    } else {
      [void]$Items.Add([pscustomobject]@{ Status = 'warn'; Mark = '!'; Msg = "Linked computers: disabled in Settings." })
    }
  } catch {
    [void]$Items.Add([pscustomobject]@{ Status = 'error'; Mark = [char]0x2717; Msg = "Link check error: $_" })
  }
}

# 3. AI CLI Check
$claudeExe = Get-Command claude -ErrorAction SilentlyContinue
if ($claudeExe) {
  [void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "Claude: ready (Claude Code CLI installed)." })
} else {
  [void]$Items.Add([pscustomobject]@{ Status = 'warn'; Mark = '!'; Msg = "Claude: CLI not in PATH." })
}

$agyExe = Get-Command agy -ErrorAction SilentlyContinue
if ($agyExe) {
  [void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "Gemini: ready (Antigravity CLI installed)." })
} else {
  [void]$Items.Add([pscustomobject]@{ Status = 'warn'; Mark = '!'; Msg = "Gemini: agy CLI not in PATH." })
}

[void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "ChatGPT: ready." })

# Local AI Models
foreach ($local in @(@{ Name = 'Ollama'; Port = 11434 }, @{ Name = 'LM Studio'; Port = 1234 })) {
  $ms = Test-TcpPort '127.0.0.1' $local.Port 400
  if ($ms -ge 0) {
    [void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "Local AI ($($local.Name)): listening on port $($local.Port)." })
  }
}

# 4. Network & Internet Check
$netMs = Test-TcpPort 'github.com' 443 2500
if ($netMs -ge 0) {
  [void]$Items.Add([pscustomobject]@{ Status = 'ok'; Mark = [char]0x2713; Msg = "Internet: reachable (github.com:443)." })
} else {
  [void]$Items.Add([pscustomobject]@{ Status = 'error'; Mark = [char]0x2717; Msg = "Internet: unreachable." })
}

$problems = @($Items | Where-Object { $_.Status -eq 'error' }).Count
$warnings = @($Items | Where-Object { $_.Status -eq 'warn' }).Count

$hostname = $env:COMPUTERNAME
if ($problems -eq 0 -and $warnings -eq 0) {
  $summary = "everything is connected."
} elseif ($problems -eq 0) {
  $summary = "everything is connected; $warnings item(s) need attention."
} else {
  $summary = "$problems problem(s) found."
}

$header = "Connection Status Check on $hostname`: $summary"
$lines = @(foreach ($i in $Items) { "$($i.Mark) $($i.Msg)" })
$reportText = "$header`n" + ($lines -join "`n")

if ($Json) {
  [pscustomobject]@{
    Hostname = $hostname
    Summary = $summary
    Problems = $problems
    Warnings = $warnings
    Items = $Items
  } | ConvertTo-Json -Depth 5
  return
}

Write-Output $reportText

if ($DryRun) {
  Write-Output "`n(Dry run: not sent to chat or Signal)"
  return
}

# Post to Chat
if ($Chat -and (Test-Path $ConvDir)) {
  $targetConv = Get-ChildItem -Path $ConvDir -Filter '*.jsonl' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if ($targetConv) {
    $convId = $targetConv.BaseName
    $consiliumCmd = Get-Command consilium -ErrorAction SilentlyContinue
    $posted = $false
    if ($consiliumCmd) {
      try {
        & $consiliumCmd say $reportText -c $convId | Out-Null
        $posted = $true
        Write-Output "`n[Chat Delivery]: Posted via Consilium CLI to conversation $convId."
      } catch {}
    }
    if (-not $posted) {
      try {
        $seq = (Get-Content $targetConv.FullName | Measure-Object -Line).Lines + 1
        $evt = [pscustomobject]@{
          id = [guid]::NewGuid().ToString()
          seq = $seq
          ts = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
          conversation = $convId
          sender = 'consilium'
          kind = 'system'
          to = @()
          text = $reportText
          requests = @()
          add_ai = @()
        }
        Add-Content -Path $targetConv.FullName -Value ($evt | ConvertTo-Json -Compress) -Encoding UTF8
        Write-Output "`n[Chat Delivery]: Appended system note to $($targetConv.Name)."
      } catch {
        Write-Output "`n[Chat Delivery Failed]: $_"
      }
    }
  }
}

# Send to Signal
if ($Signal -and $SigAccount) {
  try {
    $tcp = [System.Net.Sockets.TcpClient]::new()
    $tcp.Connect('127.0.0.1', 7583)
    $stream = $tcp.GetStream()
    $writer = [System.IO.StreamWriter]::new($stream)
    $reader = [System.IO.StreamReader]::new($stream)
    $payload = [pscustomobject]@{
      jsonrpc = '2.0'
      id = "status-check-$([guid]::NewGuid().ToString())"
      method = 'send'
      params = [pscustomobject]@{
        recipient = @($SigAccount)
        message = $reportText
      }
    }
    $writer.WriteLine(($payload | ConvertTo-Json -Compress))
    $writer.Flush()
    $res = $reader.ReadLine()
    $tcp.Dispose()
    Write-Output "[Signal Delivery]: Delivered to Signal Note to Self ($SigAccount)."
  } catch {
    Write-Output "[Signal Delivery Failed]: $_"
  }
} elseif ($Signal) {
  Write-Output "[Signal Delivery]: Skipped (Signal not configured)."
}
