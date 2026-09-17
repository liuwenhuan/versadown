# Diagnostic spike only: inspect our isolated session's official response shape.
# Never print credentials, signed URLs, acceleration tokens, or decoded values.
param([Parameter(Mandatory=$true)][string]$SessionDirectory)
$ErrorActionPreference='Stop'
$root=[IO.Path]::GetFullPath($SessionDirectory)
$temporaryRoot=[IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')+'\'
if(-not $root.StartsWith($temporaryRoot,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path $root -Leaf) -notlike 'alldownloader-xunlei-auth-live-*'){throw 'isolated_profile_required'}
Add-Type -AssemblyName System.Security
$plain=$null
try {
  $plain=[Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes((Join-Path $root 'cloud-sessions/xunlei.bin')),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
  $session=[Text.Encoding]::UTF8.GetString($plain)|ConvertFrom-Json
  $fileId=node -e "const {DatabaseSync}=require('node:sqlite');const p=require('node:path');const d=new DatabaseSync(p.join(process.env.APPDATA,'com.alldownloader.desktop','tasks.sqlite'),{readOnly:true});console.log(d.prepare('select source from tasks where name=? order by updated_at desc limit 1').get('midysj.zip').source.split('/').pop());"
  if($LASTEXITCODE -ne 0 -or -not $fileId){throw 'test_file_missing'}
  $headers=@{Authorization='Bearer '+$session.access_token;'X-Device-Id'=$session.profile.device_id;'X-Client-Id'=$session.profile.client_id;'X-Client-Version'=$session.profile.client_version;Origin='https://pan.xunlei.com';Referer='https://pan.xunlei.com/';'X-Captcha-Token'=$session.web_security.captcha_token}
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
  $report|ConvertTo-Json -Depth 4 -Compress
}catch{
  $status=0
  try{$status=[int]$_.Exception.Response.StatusCode}catch{}
  @{probe='acceleration_shape';status=$status;error='probe_failed'}|ConvertTo-Json -Compress
  exit 1
}finally{
  if($plain){[Array]::Clear($plain,0,$plain.Length)}
  if($decoded){[Array]::Clear($decoded,0,$decoded.Length)}
  $session=$null;$headers=$null;$token=$null;$decodedText=$null;$object=$null;$file=$null;$link=$null
}
