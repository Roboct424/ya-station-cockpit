$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Start-Process -Verb RunAs -FilePath (Join-Path $root 'target_gui\release\cockpit.exe') `
  -ArgumentList 'ui','--port','8780' -WorkingDirectory $root
