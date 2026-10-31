param([Parameter(Mandatory=$true)][string]$Installer, [Parameter(Mandatory=$true)][string]$Fixture)
$ErrorActionPreference = 'Stop'
$installPath = Join-Path $env:RUNNER_TEMP 'Planyrith-installed-smoke'
$process = Start-Process -FilePath $Installer -ArgumentList @('/S', "/D=$installPath") -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Installer failed: $($process.ExitCode)" }
foreach ($name in @('planyrith.exe', 'planyrith-gui.exe', 'planyrith-skp.exe', 'Uninstall.exe')) {
  if (-not (Test-Path (Join-Path $installPath $name))) { throw "Missing installed $name" }
}
$cli = Join-Path $installPath 'planyrith.exe'
$result = & $cli --json -f $Fixture info | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $result.ok -or $result.output.problems -ne 0 -or $result.output.counts.model.faces -ne 6) {
  throw 'Installed CLI/converter could not open the historical model'
}
$native = Join-Path $env:RUNNER_TEMP 'installed-smoke.plyr'
& $cli -f $Fixture -o $native tool select --query all
if ($LASTEXITCODE -ne 0) { throw 'Installed native save failed' }
& $cli -f $native validate
if ($LASTEXITCODE -ne 0) { throw 'Installed native validation failed' }
$uninstaller = Join-Path $installPath 'Uninstall.exe'
$process = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
if ($process.ExitCode -ne 0) { throw 'Uninstall failed' }
for ($attempt = 0; $attempt -lt 20 -and (Test-Path $cli); $attempt++) { Start-Sleep -Seconds 1 }
if (Test-Path $cli) { throw 'Uninstaller left application binaries installed' }
