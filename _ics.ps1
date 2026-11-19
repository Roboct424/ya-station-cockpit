$log = 'C:\Users\Admin\Documents\Проект по умолчанию\ya-station-cockpit\ics.log'
try {
  $uplink = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias
  "uplink: $uplink" | Out-File $log -Encoding utf8
  $share = New-Object -ComObject HNetCfg.HNetShare
  $wifi = $null; $eth = $null
  foreach ($c in $share.EnumEveryConnection) {
      $name = $share.NetConnectionProps($c).Name
      if ($name -eq $uplink) { $wifi = $c }
      if ($name -eq 'Ethernet') { $eth = $c }
  }
  "conns wifi=$([bool]$wifi) eth=$([bool]$eth)" | Out-File $log -Append -Encoding utf8
  if (-not $wifi -or -not $eth) { throw 'не нашёл подключения' }
  $pub  = $share.INetSharingConfigurationForINetConnection($wifi)
  $priv = $share.INetSharingConfigurationForINetConnection($eth)
  $priv.DisableSharing(); $pub.DisableSharing()
  $pub.EnableSharing(0); $priv.EnableSharing(1)
  Start-Sleep -Seconds 6
  "public=$($pub.SharingEnabled)/$($pub.SharingConnectionType) private=$($priv.SharingEnabled)/$($priv.SharingConnectionType)" | Out-File $log -Append -Encoding utf8
} catch {
  "ОШИБКА: $_" | Out-File $log -Append -Encoding utf8
}
