param([Parameter(Mandatory=$true)][string]$SessionDirectory,[switch]$Measure)
$ErrorActionPreference='Stop'
if (-not ([IO.Path]::GetFullPath($SessionDirectory).StartsWith([IO.Path]::GetFullPath($env:TEMP),[StringComparison]::OrdinalIgnoreCase)) -or (Split-Path $SessionDirectory -Leaf) -notlike 'alldownloader-xunlei-auth-live-*') { throw 'isolated_profile_required' }
Add-Type -AssemblyName System.Security
$bytes=[IO.File]::ReadAllBytes((Join-Path $SessionDirectory 'cloud-sessions/xunlei.bin'))
$plain=[Security.Cryptography.ProtectedData]::Unprotect($bytes,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
try {
  $session=[Text.Encoding]::UTF8.GetString($plain)|ConvertFrom-Json
  $fileId=node -e "const {DatabaseSync}=require('node:sqlite');const p=require('node:path');const d=new DatabaseSync(p.join(process.env.APPDATA,'com.alldownloader.desktop','tasks.sqlite'),{readOnly:true});console.log(d.prepare('select source from tasks where name=? order by updated_at desc limit 1').get('midysj.zip').source.split('/').pop());"
  $headers=@{Authorization='Bearer '+$session.access_token;'X-Device-Id'=$session.profile.device_id;'X-Client-Id'=$session.profile.client_id;'X-Client-Version'=$session.profile.client_version;Origin='https://pan.xunlei.com';Referer='https://pan.xunlei.com/';'X-Captcha-Token'=$session.web_security.captcha_token}
  $transportProbe=$env:ALLDOWNLOADER_XUNLEI_TRANSPORT_PROBE -eq '1'
  $protocolProbe=$env:ALLDOWNLOADER_XUNLEI_PROTOCOL_PROBE -eq '1'
  $usages=if($transportProbe -or $protocolProbe){@('FETCH')}else{@('','DISPLAY','FETCH')}
  foreach($usage in $usages) {
    try {
      $endpoint='https://api-pan.xunlei.com/drive/v1/files/'+$fileId
      if($usage){$endpoint+='?space=&usage='+$usage}
      $reply=Invoke-WebRequest -UseBasicParsing -Uri $endpoint -Headers $headers -UserAgent $session.profile.user_agent -Proxy $env:HTTPS_PROXY -TimeoutSec 30
      $value=$reply.Content|ConvertFrom-Json
      foreach($entry in $value.links.PSObject.Properties) {
        $candidate=$entry.Value
        $candidateUrl=$candidate.url
        $safeLink=$null
        if($candidateUrl){try{$safeLink=[Uri]$candidateUrl}catch{}}
        $knownTokenType=if($candidate.token_type -match '^[A-Za-z_ -]{1,48}$'){$candidate.token_type}else{'other'}
        @{usage=$usage;linkType=$entry.Name;fields=@($candidate.PSObject.Properties.Name);host=if($safeLink){$safeLink.Host}else{''};sameAsWeb=($candidateUrl -eq $value.web_content_link);hasToken=[bool]$candidate.token;tokenType=$knownTokenType;tokenIsAccount=($candidate.token -eq $session.access_token);tokenInUrl=([bool]$candidate.token -and $candidateUrl.Contains($candidate.token))}|ConvertTo-Json -Compress
      }
      if(-not $value.web_content_link) { @{usage=$usage;status=[int]$reply.StatusCode;hasLink=$false;phase=$value.phase}|ConvertTo-Json -Compress;continue }
      $link=[Uri]$value.web_content_link
      $query=@{}; foreach($pair in $link.Query.TrimStart('?').Split('&')) { $part=$pair.Split('=',2); if($part.Length -eq 2){$query[$part[0]]=$part[1]} }
      @{usage=$usage;status=[int]$reply.StatusCode;webHost=$link.Host;scheme=$link.Scheme;alternateLink=[bool]$value.download_url;threshold=$query.threshold;pr=$query.pr;vip=$query.vip;linkFields=@($value.links.PSObject.Properties.Name);paramFields=@($value.params.PSObject.Properties.Name)} | ConvertTo-Json -Compress
      if($protocolProbe) {
        $expiresIn=if($query.e -match '^\d+$'){[long]$query.e-[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()}else{$null}
        @{expiresInSeconds=$expiresIn;queryKeys=@($query.Keys);urlHasHtmlEntities=$value.web_content_link.Contains('&amp;');apiDate=[string]$reply.Headers.Date}|ConvertTo-Json -Compress
        @{url=$value.web_content_link;userAgent=$session.profile.user_agent}|ConvertTo-Json -Compress|node (Join-Path $PSScriptRoot 'probe-xunlei-transport.cjs')
        if($LASTEXITCODE -ne 0){throw 'protocol_probe_failed'}
        continue
      }
      if($transportProbe) {
        $base='url = "'+$value.web_content_link+'"'
        $agent='user-agent = "'+$session.profile.user_agent+'"'
        $referer='referer = "https://pan.xunlei.com/"'
        foreach($probe in @(
          @{name='current-zero';headers=@($agent,$referer);range='0-1048575'},
          @{name='current-resume';headers=@($agent,$referer);range='3414400-4462975'},
          @{name='no-referer';headers=@($agent);range='0-1048575'},
          @{name='curl-default-agent';headers=@($referer);range='0-1048575'},
          @{name='current-unbounded';headers=@($agent,$referer);range='0-'}
        )) {
          Write-Output ('probe='+$probe.name)
          ($base+"`n"+($probe.headers -join "`n")) | & curl.exe --config - --silent --range $probe.range --max-time 15 --output NUL --write-out 'http=%{http_code} connect=%{http_connect} proxy=%{proxy_used} version=%{http_version} bytes=%{size_download} speed=%{speed_download} tcp=%{time_connect} tls=%{time_appconnect} start=%{time_starttransfer}'
          Write-Output " curlExit=$LASTEXITCODE"
        }
        continue
      }
      if($Measure -and $usage -ne 'DISPLAY') {
        $config='url = "'+$value.web_content_link+'"'+"`n"+'user-agent = "'+$session.profile.user_agent+'"'+"`n"+'referer = "https://pan.xunlei.com/"'
        $config | & curl.exe --config - --silent --range 0-1048575 --max-time 20 --output NUL --write-out 'http=%{http_code} bytes=%{size_download} speed=%{speed_download} start=%{time_starttransfer}'
        Write-Output "curlExit=$LASTEXITCODE"
      }
    } catch {
      $err=$null;try{$err=$_.ErrorDetails.Message|ConvertFrom-Json}catch{}
      @{usage=$usage;status=[int]$_.Exception.Response.StatusCode;error=$err.error;code=$err.error_code} | ConvertTo-Json -Compress
    }
  }
} finally { [Array]::Clear($plain,0,$plain.Length);$session=$null;$headers=$null }
