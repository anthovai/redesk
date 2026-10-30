# Installs the Windows agent silently, checks what it installed, then uninstalls it.
# Used by CI (.github/workflows/ci.yml); also works on a spare Windows machine or VM:
#
#   powershell -ExecutionPolicy Bypass -File deploy\windows\smoke-test.ps1 dist\Aegiron-Agent-Setup-<version>.exe
#
# Changes the current user's Aegiron install: do not run it on a machine you use.
#
# Checks: the files, the identity created by /SERVER /KEY, the registry (aegiron:// and
# redesk:// URL schemes, start at logon, Add/Remove Programs), `aegiron-agent info`, the
# single-instance hand-over (a second launch and an aegiron:// link opened through the shell
# reach the running app over the named pipe and exit), and a silent uninstall that keeps the
# identity.
param([Parameter(Mandatory = $true)][string]$Installer)

$ErrorActionPreference = 'Stop'
$failures = 0
function Check([bool]$ok, [string]$what) {
    if ($ok) { Write-Host "  ok    $what" } else { Write-Host "  FAIL  $what"; $script:failures++ }
}
function Wait-Until([scriptblock]$cond, [int]$seconds) {
    $end = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $end) {
        if (& $cond) { return $true }
        Start-Sleep -Milliseconds 250
    }
    return [bool](& $cond)
}
function RegValue([string]$key, [string]$name) {
    $p = Get-ItemProperty -Path $key -ErrorAction SilentlyContinue
    if ($null -eq $p) { return $null }
    if ($name -eq '') { return $p.'(default)' }
    return $p.$name
}

$dir = Join-Path $env:LOCALAPPDATA 'Programs\Aegiron'
$app = Join-Path $dir 'aegiron.exe'
$cli = Join-Path $dir 'aegiron-agent.exe'
$cfg = Join-Path $env:APPDATA 'Aegiron\agent.toml'
$log = Join-Path $env:LOCALAPPDATA 'Aegiron\aegiron.log'
$server = 'https://aegiron.example.com'
# Contains "/" and "+" (base64 of 0xFF 0xFC ...), like many real server keys.
$key = [Convert]::ToBase64String((0..31 | ForEach-Object { [byte](255 - 3 * $_) }))
$token = "rdk_smoketest"

if (Test-Path $dir) { throw "$dir exists: refusing to run on a machine with Aegiron installed" }
Remove-Item -Force -ErrorAction SilentlyContinue $log

Write-Host "==> Silent install: $Installer"
# Not -Wait: that also waits for the app the silent install starts.
$p = Start-Process -FilePath $Installer -ArgumentList '/S', "/SERVER=$server", "/KEY=$key", "/TOKEN=$token" -PassThru
if (-not $p.WaitForExit(120000)) { throw 'installer did not finish within 2 minutes' }
Check ($p.ExitCode -eq 0) "installer exit code 0 (got $($p.ExitCode))"
foreach ($f in 'aegiron.exe', 'aegiron-agent.exe', 'aegiron.ico', 'uninstall.exe') {
    Check (Test-Path (Join-Path $dir $f)) "installed $f"
}
foreach ($f in $app, $cli) {
    $v = (Get-Item $f).VersionInfo
    Check ($v.ProductName -eq 'Aegiron') "$(Split-Path -Leaf $f) has version information (icon and resources embedded)"
}
# Fails with 0xC0000135 (STATUS_DLL_NOT_FOUND) if a mingw runtime DLL is needed.
$ver = & $cli --version 2>&1 | Out-String
Check ($LASTEXITCODE -eq 0 -and $ver -match 'aegiron-agent') "aegiron-agent --version runs (exit $LASTEXITCODE): $($ver.Trim())"
Check (Test-Path $cfg) "identity created ($cfg)"
Check ((Test-Path $cfg) -and (Get-Content -Raw $cfg) -match [regex]::Escape($server)) 'agent.toml has the server from /SERVER'
Check ((Test-Path $cfg) -and (Get-Content -Raw $cfg) -match [regex]::Escape($key)) "agent.toml has the whole key from /KEY ($key)"
Check ((Test-Path $cfg) -and (Get-Content -Raw $cfg) -match $token) "agent.toml has the token from /TOKEN"
Check (Test-Path (Join-Path ([Environment]::GetFolderPath('Programs')) 'Aegiron.lnk')) 'Start menu shortcut'

Write-Host '==> Registry'
foreach ($scheme in 'aegiron', 'redesk') {
    $k = "HKCU:\Software\Classes\$scheme"
    Check ((RegValue $k '') -like 'URL:Aegiron*') "$scheme`: URL class name"
    Check ($null -ne (RegValue $k 'URL Protocol')) "$scheme`: URL Protocol value"
    Check ((RegValue "$k\shell\open\command" '') -eq "`"$app`" `"%1`"") "$scheme`: open command"
    Check ((RegValue "$k\DefaultIcon" '') -eq "`"$app`",0") "$scheme`: icon"
}
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Check ((RegValue $run 'Aegiron') -eq "`"$app`" --background") 'start at logon (Run\Aegiron)'
$un = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Aegiron'
Check ((RegValue $un 'QuietUninstallString') -eq "`"$dir\uninstall.exe`" /S") 'Add/Remove Programs entry'

Write-Host '==> aegiron-agent info'
$info = & $cli info 2>&1 | Out-String
Write-Host $info
Check ($LASTEXITCODE -eq 0) "info exit code 0 (got $LASTEXITCODE)"
Check ($info -match 'ID:\s+\d{9}') 'info shows a 9-digit ID'
Check ($info -match [regex]::Escape("Server:       $server")) 'info shows the server'
Check ($info -match 'starts at logon') 'info sees the Run entry'

Write-Host '==> Single instance'
# A silent install starts the app in the tray.
Check (Wait-Until { $null -ne (Get-Process aegiron -ErrorAction SilentlyContinue) } 15) 'silent install started aegiron.exe'
Check (Wait-Until { (Test-Path $log) -and ((Get-Item $log).Length -gt 0) } 15) "app writes $log"
Start-Sleep -Seconds 3
$first = @(Get-Process aegiron -ErrorAction SilentlyContinue)
Check ($first.Count -eq 1) "one aegiron.exe running (got $($first.Count))"

# Second launch with a link on the command line: hands it over and exits.
$second = Start-Process -FilePath $app -ArgumentList 'aegiron://connect/123456789' -PassThru
Check ($second.WaitForExit(15000)) 'second launch exits'
Check (Wait-Until { (Get-Content -Raw $log) -match 'request from another launch: Open\("123456789"\)' } 10) 'running app received Open("123456789") through the pipe'

# Through the shell, the way a browser opens the link (uses the registered URL scheme).
Start-Process 'aegiron://connect/987654321'
Check (Wait-Until { (Get-Content -Raw $log) -match 'request from another launch: Open\("987654321"\)' } 15) 'aegiron:// link opened by the shell reached the running app'
Start-Process 'redesk://connect/111222333'
Check (Wait-Until { (Get-Content -Raw $log) -match 'request from another launch: Open\("111222333"\)' } 15) 'legacy redesk:// link reached the running app'

# Plain second launch: brings the window forward.
$third = Start-Process -FilePath $app -PassThru
Check ($third.WaitForExit(15000)) 'plain second launch exits'
Check (Wait-Until { (Get-Content -Raw $log) -match 'request from another launch: Show' } 10) 'running app received Show'
Check (Wait-Until { @(Get-Process aegiron -ErrorAction SilentlyContinue).Count -eq 1 } 10) 'still exactly one aegiron.exe'
Check ($first[0].HasExited -eq $false) 'first instance still running'

Write-Host '==> Silent uninstall'
# The uninstaller copies itself to %TEMP% and returns at once: wait for the files to go.
$u = Start-Process -FilePath (Join-Path $dir 'uninstall.exe') -ArgumentList '/S' -PassThru
$null = $u.WaitForExit(30000)
Check (Wait-Until { -not (Test-Path $dir) } 30) "removed $dir"
Check ($null -eq (Get-Process aegiron -ErrorAction SilentlyContinue)) 'aegiron.exe stopped'
foreach ($k in 'HKCU:\Software\Classes\aegiron', 'HKCU:\Software\Classes\redesk', $un) {
    Check (-not (Test-Path $k)) "removed $k"
}
Check ($null -eq (RegValue $run 'Aegiron')) 'removed Run\Aegiron'
Check (Test-Path $cfg) 'kept the identity (silent uninstall)'

if (Test-Path $log) {
    Write-Host '==> aegiron.log'
    Get-Content $log | ForEach-Object { Write-Host "  $_" }
}
if ($failures -gt 0) {
    Write-Host "$failures check(s) failed"
    exit 1
}
Write-Host 'all checks passed'
