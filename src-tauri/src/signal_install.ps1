# One-click signal-cli install for Consilium on Windows, with no admin rights and no Terminal.
# Mirrors signal_install.sh: a pinned Java runtime (Eclipse Temurin) and a pinned signal-cli,
# both checked against fixed SHA-256 sums, unpacked into %USERPROFILE%\.local\share\consilium-signal,
# with a signal-cli.bat launcher in %USERPROFILE%\.local\bin. Same marker files as the shell script.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Root = Join-Path $HOME '.local\share\consilium-signal'
$Bin = Join-Path $HOME '.local\bin'
$SignalVer = '0.14.10'
$SignalUrl = "https://github.com/AsamK/signal-cli/releases/download/v$SignalVer/signal-cli-$SignalVer.tar.gz"
$SignalSha = '706a57d9faae7525cf732936efdabf210146e3c83f9e8ae7e4e62f5946121c33'
$JreUrl = 'https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.5%2B8/OpenJDK25U-jre_x64_windows_hotspot_25.0.5_8.zip'
$JreSha = 'da78d54a2fcdbba1e92d5e3f07dd30236f96297891c4a21a781c3f38f5c0beae'
New-Item -ItemType Directory -Force -Path $Root, $Bin | Out-Null
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $Root 'failed')
New-Item -ItemType File -Force -Path (Join-Path $Root 'installing') | Out-Null
$Log = Join-Path $Root 'install.log'
$Work = Join-Path $Root ("tmp." + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $Work | Out-Null
function Fail($why) {
  Set-Content -Path (Join-Path $Root 'failed') -Value $why
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Work, (Join-Path $Root 'installing')
  exit 1
}
function Fetch($url, $sha, $dest, $label, $file) {
  Add-Content $Log "Downloading $label..."
  $pkg = Join-Path $Work $file
  try { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $pkg } catch { Fail "Couldn't download $label. Check the internet connection and try again." }
  if ((Get-FileHash -Algorithm SHA256 $pkg).Hash.ToLower() -ne $sha) { Fail "The $label download didn't match its expected checksum, so it wasn't installed." }
  $x = Join-Path $Work 'x'
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $x
  New-Item -ItemType Directory -Path $x | Out-Null
  & tar.exe -xf $pkg -C $x
  if ($LASTEXITCODE -ne 0) { Fail "Couldn't unpack $label." }
  $top = Get-ChildItem -Directory $x | Select-Object -First 1
  if (-not $top) { Fail "The $label download was empty." }
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $dest
  Move-Item $top.FullName $dest
  Remove-Item -Force $pkg
}
try {
  $Jre = Join-Path $Root 'jre'
  $Cli = Join-Path $Root "signal-cli-$SignalVer"
  if (-not (Test-Path (Join-Path $Jre 'bin\java.exe'))) { Fetch $JreUrl $JreSha $Jre 'Java' 'jre.zip' }
  if (-not (Test-Path (Join-Path $Cli 'bin\signal-cli.bat'))) { Fetch $SignalUrl $SignalSha $Cli "signal-cli $SignalVer" 'cli.tgz' }
  $Launcher = Join-Path $Bin 'signal-cli.bat'
  Set-Content -Encoding ASCII -Path $Launcher -Value "@echo off`r`nset `"JAVA_HOME=$Jre`"`r`ncall `"$Cli\bin\signal-cli.bat`" %*"
  & $Launcher --version *>> $Log
  if ($LASTEXITCODE -ne 0) { Fail "signal-cli was installed but didn't start." }
} catch { Fail "The install stopped unexpectedly. See $Log" }
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Work, (Join-Path $Root 'installing')
Add-Content $Log 'Installed.'
