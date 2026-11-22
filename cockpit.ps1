$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root 'bin\cockpit.exe'
if (-not (Test-Path $exe)) {
    # Плоская раскладка релизного zip: cockpit.exe рядом с лаунчером.
    $exe = Join-Path $root 'cockpit.exe'
}
$port = 8790
$url = "http://127.0.0.1:$port/"
try {
    $r = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 2
    if ($r.StatusCode -eq 200) { Start-Process $url; exit 0 }
} catch {}
if (-not (Test-Path $exe)) { Write-Host "нет $exe"; exit 1 }
Start-Process -Verb RunAs -FilePath $exe -ArgumentList 'ui','--port',"$port" -WorkingDirectory $root
Start-Sleep -Seconds 2
Start-Process $url
