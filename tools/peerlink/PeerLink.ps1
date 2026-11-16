# PeerLink: find every working network path to a peer and connect over the best one.
param(
  [Parameter(Position=0)][ValidateSet('add','status','connect','list')][string]$Action = 'list',
  [Parameter(Position=1)][string]$Peer,
  [string[]]$Address = @(), [string[]]$Mac = @(), [string]$User, [string]$Tailscale,
  [string]$Direct, [string]$Share, [string]$Drive, [int]$Port = 445
)
$Root = $PSScriptRoot; $CfgPath = Join-Path $Root 'peers.json'
function Get-Cfg { $h = @{}; if (Test-Path $CfgPath) { $o = Get-Content $CfgPath -Raw | ConvertFrom-Json; foreach ($q in $o.PSObject.Properties) { $h[$q.Name] = $q.Value } }; $h }
function Save-Cfg($h) { $h | ConvertTo-Json -Depth 5 | Set-Content $CfgPath -Encoding UTF8 }
function Merge($old, $new) { return ,@(@(@($new) + @($old)) | Where-Object { $_ } | Select-Object -Unique -First 6) }
function NormMac($m) { $m.ToUpper() -replace ':','-' }
function UncHost($ip) { if ($ip -match ':') { ($ip -replace ':','-' -replace '%','s') + '.ipv6-literal.net' } else { $ip } }
function Test-Tcp([string]$ip, [int]$port, [int]$timeout = 1500) {
  $fam = if ($ip -match ':') { [System.Net.Sockets.AddressFamily]::InterNetworkV6 } else { [System.Net.Sockets.AddressFamily]::InterNetwork }
  $t = [System.Net.Sockets.TcpClient]::new($fam); $sw = [Diagnostics.Stopwatch]::StartNew()
  try { [void]$t.ConnectAsync([ipaddress]::Parse($ip), $port).Wait($timeout) } catch {}
  $ok = $t.Connected; $sw.Stop(); $t.Dispose(); if ($ok) { [int]$sw.ElapsedMilliseconds } else { -1 }
}
function Update-Neighbors($p) {
  foreach ($ip in @($p.IPs)) { if ($ip) { ping -n 1 -w 300 $ip | Out-Null } }
  foreach ($a in @(Get-NetAdapter -Physical -ErrorAction SilentlyContinue | Where-Object Status -eq 'Up')) { ping -n 1 -w 300 "ff02::1%$($a.ifIndex)" | Out-Null }
}
function Get-Candidates($p) {
  $list = [System.Collections.ArrayList]::new()
  $add = { param($via, $ip) if ($ip -and -not ($list | Where-Object { $_.Ip -eq "$ip" })) { [void]$list.Add([pscustomobject]@{ Via = $via; Ip = "$ip" }) } }
  foreach ($h in @($p.Hosts)) { if ($h) { try { foreach ($a in [System.Net.Dns]::GetHostAddresses($h)) { if ($a.AddressFamily -eq 'InterNetwork' -or $a.IsIPv6LinkLocal) { & $add "name $h" $a.ToString() } } } catch {} } }
  $macs = @(@($p.Macs) | Where-Object { $_ } | ForEach-Object { NormMac $_ })
  if ($macs.Count) {
    Get-NetNeighbor -ErrorAction SilentlyContinue | Where-Object { $macs -contains $_.LinkLayerAddress -and $_.State -ne 'Unreachable' } | ForEach-Object {
      if ($_.IPAddress -notmatch ':') { & $add "hardware address ($($_.InterfaceAlias))" $_.IPAddress }
      elseif ($_.IPAddress -like 'fe80*') { & $add "router-free link-local ($($_.InterfaceAlias))" "$($_.IPAddress)%$($_.InterfaceIndex)" }
    }
  }
  foreach ($ip in @($p.IPs)) { & $add 'last known address' $ip }
  if ($p.Direct) { & $add 'direct static link' $p.Direct }
  if ($p.Tailscale -and (Get-Command tailscale -ErrorAction SilentlyContinue)) { try { & $add 'tailscale' (& tailscale ip -4 $p.Tailscale 2>$null | Select-Object -First 1) } catch {} }
  $list
}
$cfg = Get-Cfg
if ($Action -eq 'list') { $cfg.Keys | ForEach-Object { $q = $cfg[$_]; [pscustomobject]@{ Peer = $_; User = $q.User; IPs = (@($q.IPs) -join ', '); Macs = (@($q.Macs) -join ', ') } } | Format-Table -AutoSize | Out-String; return }
if (-not $Peer) { throw 'Give a peer name, e.g.: PeerLink status Lukes-MBP' }
if ($Action -eq 'add') {
  $p = if ($cfg.ContainsKey($Peer)) { $cfg[$Peer] } else { [pscustomobject]@{ Hosts = @("$Peer.local", $Peer); IPs = @(); Macs = @(); User = ''; Share = ''; Tailscale = ''; Direct = '' } }
  $p.IPs = Merge $p.IPs $Address; $p.Macs = Merge $p.Macs @($Mac | ForEach-Object { NormMac $_ })
  if ($User) { $p.User = $User }; if ($Share) { $p.Share = $Share }; if ($Tailscale) { $p.Tailscale = $Tailscale }; if ($Direct) { $p.Direct = $Direct }
  $cfg[$Peer] = $p; Save-Cfg $cfg; "Saved peer $Peer"; $Action = 'status'
}
if (-not $cfg.ContainsKey($Peer)) { throw "Unknown peer $Peer. Add it first: PeerLink add $Peer -Address <ip> -User <name>" }
$p = $cfg[$Peer]; Update-Neighbors $p
$res = @(foreach ($c in (Get-Candidates $p)) { $ms = Test-Tcp $c.Ip $Port; [pscustomobject]@{ Via = $c.Via; Address = $c.Ip; Open = ($ms -ge 0); Ms = $(if ($ms -ge 0) { $ms } else { $null }) } })
$res = @($res | Sort-Object @{ e = { -not $_.Open } }, Ms)
$v4 = @($res | Where-Object { $_.Open -and $_.Address -notmatch ':' } | ForEach-Object Address)
$p.IPs = Merge $p.IPs $v4
$p.Macs = Merge $p.Macs @(foreach ($ip in $v4) { Get-NetNeighbor -IPAddress $ip -ErrorAction SilentlyContinue | Where-Object { $_.LinkLayerAddress -and $_.LinkLayerAddress -ne '00-00-00-00-00-00' } | ForEach-Object LinkLayerAddress })
$cfg[$Peer] = $p; Save-Cfg $cfg
"Paths to $Peer (port $Port):"; $res | Format-Table Via, Address, Open, Ms -AutoSize | Out-String
if ($Action -eq 'connect') {
  $best = $res | Where-Object Open | Select-Object -First 1
  if (-not $best) { "No working path to $Peer."; exit 1 }
  $credFile = Join-Path $Root "$Peer.cred.xml"
  if (Test-Path $credFile) { $cred = Import-Clixml $credFile } else {
    $ga = @{ Message = "Sign in to $Peer (saved encrypted, readable only by this Windows user)" }; if ($p.User) { $ga.UserName = $p.User }
    $cred = Get-Credential @ga; if (-not $cred) { exit 1 }; $cred | Export-Clixml $credFile
  }
  $u = $cred.UserName -replace '^.*\\',''; $h = UncHost $best.Address
  $sh = if ($Share) { $Share } elseif ($p.Share) { $p.Share } else { 'IPC$' }; $target = "\\$h\$sh"
  net use $target /delete /y 2>&1 | Out-Null
  $na = @(); if ($Drive) { $na += "$($Drive.TrimEnd(':')):" }; $na += @($target, "/user:$Peer\$u", $cred.GetNetworkCredential().Password, '/persistent:no')
  $out = & net use @na 2>&1
  if ($LASTEXITCODE -eq 0) { "Connected to $Peer via $($best.Via) [$($best.Address)]"; Start-Process explorer.exe "\\$h" }
  else { "Sign-in failed: $out"; "If the password changed, delete $credFile and run again." }
}
