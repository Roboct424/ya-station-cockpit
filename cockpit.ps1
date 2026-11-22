$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root 'bin\cockpit.exe'
if (-not (Test-Path $exe)) {
    # Плоская раскладка релизного zip: cockpit.exe рядом с лаунчером.
    $exe = Join-Path $root 'cockpit.exe'
}
if (-not (Test-Path $exe)) { Write-Host "нет $exe"; exit 1 }
# Без аргументов: cockpit сам проверит 8790, при старой панели возьмёт
# следующий свободный порт и откроет браузер.
Start-Process -Verb RunAs -FilePath $exe -WorkingDirectory $root
