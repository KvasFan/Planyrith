param([Parameter(Mandatory=$true)][string]$Package, [Parameter(Mandatory=$true)][string]$Fixture)
# Installs the per-user MSI silently, exercises the installed programs and
# association, then uninstalls it and checks nothing is left (docs/16).
$ErrorActionPreference = 'Stop'
# msiexec refuses paths that mix / and \ (error 1619).
$Package = (Resolve-Path -LiteralPath $Package).Path
$log = Join-Path $env:RUNNER_TEMP 'planyrith-msi.log'
$process = Start-Process -FilePath msiexec.exe -ArgumentList @('/i', "`"$Package`"", '/qn', '/l*v', "`"$log`"") -Wait -PassThru
if ($process.ExitCode -ne 0) { Get-Content $log -Tail 60; throw "MSI install failed: $($process.ExitCode)" }
$installPath = Join-Path $env:LOCALAPPDATA 'Programs\Planyrith'
foreach ($name in @('planyrith.exe', 'planyrith-gui.exe', 'planyrith-skp.exe', 'Planyrith.ico', 'licenses')) {
  if (-not (Test-Path (Join-Path $installPath $name))) { throw "Missing installed $name" }
}
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Planyrith\Planyrith.lnk'
if (-not (Test-Path $shortcut)) { throw 'Missing Start menu shortcut' }
$command = (Get-ItemProperty 'HKCU:\Software\Classes\Planyrith.Model\shell\open\command').'(default)'
if ($command -notlike "*$installPath\planyrith-gui.exe*") { throw "Unexpected open command: $command" }
$cli = Join-Path $installPath 'planyrith.exe'
$result = & $cli --json -f $Fixture info | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $result.ok -or $result.output.problems -ne 0 -or $result.output.counts.model.faces -ne 6) {
  throw 'Installed CLI/converter could not open the historical model'
}
$native = Join-Path $env:RUNNER_TEMP 'msi-smoke.plyr'
& $cli -f $Fixture -o $native tool select --query all
if ($LASTEXITCODE -ne 0) { throw 'Installed native save failed' }
& $cli -f $native validate
if ($LASTEXITCODE -ne 0) { throw 'Installed native validation failed' }
$process = Start-Process -FilePath msiexec.exe -ArgumentList @('/x', "`"$Package`"", '/qn') -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "MSI uninstall failed: $($process.ExitCode)" }
if (Test-Path $installPath) { throw 'Uninstall left the installation folder' }
if (Test-Path 'HKCU:\Software\Classes\Planyrith.Model') { throw 'Uninstall left the file type' }
if (Test-Path $shortcut) { throw 'Uninstall left the Start menu shortcut' }
