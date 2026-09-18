# Diagnostic spike only: inspect our isolated session's official response shape.
# Never print credentials, signed URLs, acceleration tokens, or decoded values.
param([Parameter(Mandatory=$true)][string]$SessionDirectory)
$ErrorActionPreference='Stop'
$stage='start'
$root=[IO.Path]::GetFullPath($SessionDirectory)
$temporaryRoot=[IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')+'\'
if(-not $root.StartsWith($temporaryRoot,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path $root -Leaf) -notlike 'alldownloader-xunlei-auth-live-*'){throw 'isolated_profile_required'}
Add-Type -AssemblyName System.Security
$plain=$null
try {
  $stage='decrypt'
  $plain=[Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes((Join-Path $root 'cloud-sessions/xunlei.bin')),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
  $session=[Text.Encoding]::UTF8.GetString($plain)|ConvertFrom-Json
  $stage='refresh'
  if($session.refresh_token -and $session.profile.client_id -and $session.profile.device_id){
    $refreshHeaders=@{'X-Device-Id'=$session.profile.device_id;'X-Client-Id'=$session.profile.client_id;'X-Client-Version'=$session.profile.client_version;Accept='application/json;charset=UTF-8';'X-Sdk-Version'='3.4.20';'X-Protocol-Version'='301';'X-Action'='401'}
    $refreshBody=@{grant_type='refresh_token';refresh_token=$session.refresh_token;client_id=$session.profile.client_id}|ConvertTo-Json -Compress
    try{
      $refreshed=Invoke-RestMethod -Method Post -Uri 'https://xluser-ssl.xunlei.com/v1/auth/token' -Headers $refreshHeaders -UserAgent $session.profile.user_agent -ContentType 'application/json' -Body $refreshBody -Proxy $env:HTTPS_PROXY -TimeoutSec 30
      if($refreshed.access_token){$session.access_token=[string]$refreshed.access_token}
      if($refreshed.refresh_token){$session.refresh_token=[string]$refreshed.refresh_token}
    }catch{}
  }
  $stage='find_file'
  $fileId=node -e "const {DatabaseSync}=require('node:sqlite');const p=require('node:path');const d=new DatabaseSync(p.join(process.env.APPDATA,'com.alldownloader.desktop','tasks.sqlite'),{readOnly:true});console.log(d.prepare('select source from tasks where name=? order by updated_at desc limit 1').get('midysj.zip').source.split('/').pop());"
  if($LASTEXITCODE -ne 0 -or -not $fileId){throw 'test_file_missing'}
  $headers=@{Authorization='Bearer '+$session.access_token;'X-Device-Id'=$session.profile.device_id;'X-Client-Id'=$session.profile.client_id;'X-Client-Version'=$session.profile.client_version;Origin='https://pan.xunlei.com';Referer='https://pan.xunlei.com/';'X-Captcha-Token'=$session.web_security.captcha_token}
  $stage='fetch_file'
  $response=Invoke-WebRequest -UseBasicParsing -Uri ('https://api-pan.xunlei.com/drive/v1/files/'+$fileId+'?space=&usage=FETCH') -Headers $headers -UserAgent $session.profile.user_agent -Proxy $env:HTTPS_PROXY -TimeoutSec 30
  $file=$response.Content|ConvertFrom-Json
  $link=$file.links.'application/octet-stream'
  if(-not $link.token){throw 'acceleration_token_missing'}
  $token=[string]$link.token
  $parts=$token.Split('.')
  $report=[ordered]@{status=[int]$response.StatusCode;fileFields=@($file.PSObject.Properties.Name);linkFields=@($link.PSObject.Properties.Name);tokenType=if($link.token_type -match '^[A-Z_]{1,64}$'){$link.token_type}else{'unknown'};tokenLength=$token.Length;dotSegments=$parts.Length;sameAsWeb=($link.url -eq $file.web_content_link);decodedShape='opaque';decodedKeys=@()}
  if($token.Length -le 131072){
    try{
      $encoded=if($parts.Length -eq 3){$parts[1]}else{$token}
      $encoded=$encoded.Replace('-','+').Replace('_','/')
      $encoded+='='*((4-$encoded.Length%4)%4)
      $decoded=[Convert]::FromBase64String($encoded)
      $decodedText=[Text.Encoding]::UTF8.GetString($decoded)
      $object=$decodedText|ConvertFrom-Json
      if($object -is [System.Management.Automation.PSCustomObject]){
        $report.decodedShape='json'
        $report.decodedKeys=@($object.PSObject.Properties.Name | Where-Object {$_ -match '^[A-Za-z_][A-Za-z0-9_]{0,63}$'})
        $report.expiresInSeconds=if($object.exp -is [ValueType]){[long]$object.exp-[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()}else{$null}
        $report.boundToSessionUser=([bool]$object.user_id -and [string]$object.user_id -eq [string]$session.user_id)
        $report.resourceMatchesFileHash=([bool]$object.res_value -and [string]$object.res_value -eq [string]$file.hash)
        $report.resourceMatchesWebUrl=([bool]$object.res_value -and [string]$object.res_value -eq [string]$file.web_content_link)
        $report.resourceIsHttpUrl=([string]$object.res_value -match '^https?://')
        $report.resourceValueLength=([string]$object.res_value).Length
        $report.userDataKeys=@()
        try{
          $userData=$object.user_data
          if($userData -is [string]){$userData=$userData|ConvertFrom-Json}
          $report.userDataKeys=@($userData.PSObject.Properties.Name | Where-Object {$_ -match '^[A-Za-z_][A-Za-z0-9_]{0,63}$'})
        }catch{}
        $report.channelClaims=@{}
        foreach($claim in @('dcdnhub_level','phub_level','vip_type','res_type','product_type','version')){
          if([string]$object.$claim -match '^[A-Za-z0-9_-]{1,24}$'){$report.channelClaims[$claim]=[string]$object.$claim}
        }
      }
    }catch{}
  }
  $stage='speed_rule'
  $nativeClientId='XW-G4v1H72tgfJym'
  $nativeVersion=(Get-Item 'C:\Program Files\Thunder Network\Thunder\Program\Thunder.exe').VersionInfo.FileVersion
  if(-not $nativeVersion){throw 'native_version_missing'}
  $peerId=[string]$session.web_context.peer_id
  if(-not $peerId){$peerId=[string]$session.web_context.device_id}
  if(-not $peerId){$peerId=[string]$session.profile.device_id}
  if(-not $peerId){throw 'peer_id_missing'}
  $rt=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString()
  $nonce=[Guid]::NewGuid().ToString()
  $resource=[Uri]::EscapeDataString([string]$link.url)
  foreach($pair in @(@('!','%21'),@("'",'%27'),@('(','%28'),@(')','%29'),@('*','%2A'))){$resource=$resource.Replace($pair[0],$pair[1])}
  $securitySeed=$nativeClientId+$nativeVersion+[string]$session.user_id+$rt
  $md5=[Security.Cryptography.MD5]::Create()
  $securityKey=([BitConverter]::ToString($md5.ComputeHash([Text.Encoding]::UTF8.GetBytes($securitySeed)))).Replace('-','').Substring(0,16)
  $signatureText='nonce='+$nonce+'&resource_type=url&resource_value='+$resource+'&rt='+$rt+'&scene=download&trial_cost=0&trial_id='
  $hmac=[Security.Cryptography.HMACSHA256]::new([Text.Encoding]::UTF8.GetBytes($securityKey))
  $signature=([BitConverter]::ToString($hmac.ComputeHash([Text.Encoding]::UTF8.GetBytes($signatureText)))).Replace('-','').ToLowerInvariant()
  $ruleHeaders=@{'Content-Type'='application/json';'X-Guid'=$peerId;'X-Peer-Id'=$peerId;'X-Client-Id'=$nativeClientId;'X-Client-Version'=$nativeVersion;'X-Client-Version-Code'=$nativeVersion;'X-Signature'=$signature;'X-Signature-Time'=$rt;'X-Signature-Nonce'=$nonce;Authorization='Bearer '+$session.access_token}
  $query='trace_id='+[Uri]::EscapeDataString([Guid]::NewGuid().ToString())+'&resource_type=url&resource_value='+$resource+'&scene=download&trial_id=&trial_cost=0&rt='+$rt+'&nonce='+$nonce+'&sign='+$signature+'&user_id='+[Uri]::EscapeDataString([string]$session.user_id)
  $ruleResponse=Invoke-WebRequest -UseBasicParsing -Uri ('https://api-gateway-pan.xunlei.com/speed-center/v2/rule?'+$query) -Headers $ruleHeaders -Proxy $env:HTTPS_PROXY -TimeoutSec 30
  $ruleEnvelope=$ruleResponse.Content|ConvertFrom-Json
  $ruleData=$null
  if([int]$ruleEnvelope.error_code -eq 0 -and $ruleEnvelope.data){
    $aes=[Security.Cryptography.Aes]::Create()
    $aes.Mode=[Security.Cryptography.CipherMode]::CBC
    $aes.Padding=[Security.Cryptography.PaddingMode]::PKCS7
    $aes.Key=[Text.Encoding]::UTF8.GetBytes($securityKey)
    $aes.IV=[Text.Encoding]::UTF8.GetBytes($securityKey)
    $encrypted=[Convert]::FromBase64String([string]$ruleEnvelope.data)
    $decrypted=$aes.CreateDecryptor().TransformFinalBlock($encrypted,0,$encrypted.Length)
    $ruleData=([Text.Encoding]::UTF8.GetString($decrypted))|ConvertFrom-Json
  }
  $rule=$ruleData.rule | Select-Object -First 1
  $report.speedRule=[ordered]@{status=[int]$ruleResponse.StatusCode;errorCode=[int]$ruleEnvelope.error_code;decrypted=[bool]$ruleData;result=[int]$ruleData.result;present=[bool]$rule.id;idLength=([string]$rule.id).Length;expireDuration=[int64]$rule.expire_duration;duration=[int64]$rule.speed_info.duration}
  $report|ConvertTo-Json -Depth 4 -Compress
}catch{
  $status=0
  try{$status=[int]$_.Exception.Response.StatusCode}catch{}
  @{probe='acceleration_shape';status=$status;stage=$stage;error='probe_failed'}|ConvertTo-Json -Compress
  exit 1
}finally{
  if($plain){[Array]::Clear($plain,0,$plain.Length)}
  if($decoded){[Array]::Clear($decoded,0,$decoded.Length)}
  if($decrypted){[Array]::Clear($decrypted,0,$decrypted.Length)}
  if($encrypted){[Array]::Clear($encrypted,0,$encrypted.Length)}
  if($aes){$aes.Dispose()}
  if($hmac){$hmac.Dispose()}
  if($md5){$md5.Dispose()}
  $session=$null;$headers=$null;$token=$null;$decodedText=$null;$object=$null;$file=$null;$link=$null
}
