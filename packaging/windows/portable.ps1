param([Parameter(Mandatory=$true)][string]$Payload, [Parameter(Mandatory=$true)][string]$Output)
$ErrorActionPreference = 'Stop'
Compress-Archive -Path "$Payload\*" -DestinationPath $Output -Force
