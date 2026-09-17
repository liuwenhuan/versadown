// Read-only diagnosis of the user-reported task. Never print signed URLs or RPC secrets.
const fs=require('node:fs'),path=require('node:path');
const {DatabaseSync}=require('node:sqlite');
const {spawnSync}=require('node:child_process');
async function main(){
  const root=path.join(process.env.APPDATA,'com.alldownloader.desktop');
  const settings=JSON.parse(fs.readFileSync(path.join(root,'settings.json'),'utf8'));
  const db=new DatabaseSync(path.join(root,'tasks.sqlite'),{readOnly:true});
  const tasks=db.prepare('select id,source,status,completed_length,total_length,aria_gid,aria_error_code,failure_code,error,updated_at from tasks where name=? order by updated_at desc limit 3').all('midysj.zip');
  console.log(JSON.stringify({settings:Object.fromEntries(Object.entries(settings.aria2).filter(([k])=>!/token|secret/i.test(k))),proxyEnabled:settings.proxy.enabled}));
  const rpc=async(method,params)=>{
    const result=await (await fetch(`http://127.0.0.1:${settings.aria2.rpcPort}/jsonrpc`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method,params:['token:'+settings.aria2.rpcToken,...params]})})).json();
    return result.result||{rpcError:result.error?.code};
  };
  for(const task of tasks){
    const {source,aria_gid,error,...safe}=task;
    console.log(JSON.stringify({...safe,sourceScheme:source.split(':')[0],emptyReply:error?.includes('Empty reply'),sourceKeys:source.startsWith('{')?Object.keys(JSON.parse(source)):[]}));
    if(!aria_gid)continue;
    const status=await rpc('aria2.tellStatus',[aria_gid,['status','errorCode','errorMessage','completedLength','totalLength','connections','downloadSpeed','files']]);
    const files=(status.files||[]).map(f=>({length:f.length,completedLength:f.completedLength,uris:(f.uris||[]).map(item=>{const u=new URL(item.uri);return {host:u.host,scheme:u.protocol,queryKeys:[...u.searchParams.keys()],status:item.status};})}));
    const {errorMessage,...rest}=status;
    console.log(JSON.stringify({engine:{...rest,files,emptyReply:errorMessage?.includes('Empty reply')}}));
    const options=await rpc('aria2.getOption',[aria_gid]);
    const headers=Array.isArray(options.header)?options.header:String(options.header||'').split('\n');
    console.log(JSON.stringify({options:Object.fromEntries(Object.entries(options).filter(([k])=>['split','max-connection-per-server','max-download-limit','min-split-size','continue','max-tries','retry-wait','connect-timeout','timeout'].includes(k))),headerNames:headers.map(x=>x.split(':')[0]),proxyConfigured:!!options['all-proxy']}));
    if(process.env.XUNLEI_RANGE_PROBE==='1'){
      const uri=status.files?.[0]?.uris?.[0]?.uri;if(!uri)throw Error('uri_missing');
      const url=new URL(uri);
      console.log(JSON.stringify({linkPolicy:Object.fromEntries(['vip','threshold','rl','e','pr'].map(k=>[k,/^[0-9.,-]*$/.test(url.searchParams.get(k)||'')?url.searchParams.get(k):'encoded']))}));
      const temp=fs.mkdtempSync(path.join(require('node:os').tmpdir(),'alldownloader-xunlei-range-'));
      if(process.env.XUNLEI_VALIDATOR_PROBE==='1'){
        const state=new DatabaseSync(path.join(root,'aria2-next-state','stream','state.db'),{readOnly:true});
        const saved=state.prepare('select etag,last_modified from downloads where gid=?').get(aria_gid);state.close();
        const validator=saved?.etag&&!saved.etag.startsWith('W/')?saved.etag:saved?.last_modified;
        if(validator)headers.push('If-Range: '+validator);
        console.log(JSON.stringify({validatorPresent:!!validator}));
      }
      for(const start of (process.env.XUNLEI_HEAD_PROBE==='1'?['head']:process.env.XUNLEI_VALIDATOR_PROBE==='1'?[task.completed_length]:[0,task.completed_length])){
        const output=path.join(temp,String(start)+'.part'),head=path.join(temp,String(start)+'.headers');
        const config=[`url = ${JSON.stringify(uri)}`,...headers.filter(Boolean).map(h=>`header = ${JSON.stringify(h.trim())}`)].join('\n');
        const mode=start==='head'?['--head']:['--range',`${start}-${start+4*1024*1024-1}`];
        const result=spawnSync('curl.exe',['--config','-','--location','--silent','--show-error',...mode,'--max-time','30','--output',output,'--dump-header',head,'--write-out','%{http_code} %{size_download} %{speed_download} %{time_starttransfer}'],{input:config,encoding:'utf8',windowsHide:true,timeout:35000});
        const fields=fs.existsSync(head)?fs.readFileSync(head,'utf8').split(/\r?\n/).filter(l=>/^(HTTP\/|content-range:|content-length:|accept-ranges:)/i.test(l)):[];
        console.log(JSON.stringify({rangeStart:start,curlExit:result.status,metrics:result.stdout?.trim(),headers:fields,emptyReply:result.stderr?.includes('Empty reply')}));
      }
      console.log(JSON.stringify({rangeProbeDirectory:temp}));
      break;
    }
  }
  db.close();
}
main().catch(()=>{console.error('diagnostic_failed');process.exitCode=1;});
