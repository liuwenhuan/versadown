// Only called inside the isolated hidden auth verifier; never controls primary tasks.
const path=require('node:path'),fs=require('node:fs');
const {DatabaseSync}=require('node:sqlite');
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
exports.run=async function(invoke,root,data,account){
  if(!path.basename(data).startsWith('versadown-xunlei-auth-live-'))throw Error('isolated_data_required');
  const primary=new DatabaseSync(path.join(process.env.APPDATA,'com.versadown.desktop','tasks.sqlite'),{readOnly:true});
  const reported=primary.prepare('select id,source from tasks where name=? order by updated_at desc limit 1').get('midysj.zip');
  const binding=primary.prepare('select account_id from cloud_download_accounts where id=?').get(reported.id);primary.close();
  if(binding?.account_id!==account.id)throw Error('reported_task_belongs_to_different_account');
  const source=new URL(reported.source);if(source.protocol!=='cloud:'||source.host!=='xunlei')throw Error('invalid_reported_source');
  const file={id:decodeURIComponent(source.pathname.slice(1).replace(/\+/g,'%20')),name:'midysj.zip'};
  const settings=JSON.parse(fs.readFileSync(path.join(data,'settings.json'),'utf8'));
  const rpc=async(method,params)=> {
    const value=await (await fetch(`http://127.0.0.1:${settings.aria2.rpcPort}/jsonrpc`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method,params:['token:'+settings.aria2.rpcToken,...params]})})).json();
    if(value.error)throw Error('isolated_rpc:'+method+':'+value.error.code);return value.result;
  };
  const retryProbe=process.env.VERSADOWN_XUNLEI_RETRY_LIVE==='1';
  let bridge,disconnect=false,bridgeConnections=0;
  const sockets=new Set();
  if(retryProbe){
    if(settings.proxy?.enabled)throw Error('fault_bridge_requires_environment_proxy');
    const upstream=new URL(process.env.HTTPS_PROXY||'');
    if(upstream.protocol!=='http:'||upstream.username||upstream.password)throw Error('unsupported_proxy_for_fault_bridge');
    const net=require('node:net');
    bridge=net.createServer(client=>{
      bridgeConnections++;
      if(disconnect){client.destroy();return;}
      const remote=net.connect(Number(upstream.port)||80,upstream.hostname);
      for(const socket of [client,remote]){sockets.add(socket);socket.on('close',()=>sockets.delete(socket));}
      client.on('error',()=>remote.destroy());remote.on('error',()=>client.destroy());
      client.on('close',()=>remote.destroy());remote.on('close',()=>client.destroy());
      client.pipe(remote).pipe(client);
    });
    await new Promise(resolve=>bridge.listen(0,'127.0.0.1',resolve));
    const route=`http://127.0.0.1:${bridge.address().port}`;
    await rpc('aria2.changeGlobalOption',[{'all-proxy':route,'http-proxy':route,'https-proxy':route}]);
  }
  const log=path.join(data,'download-diagnostic.log');
  await rpc('aria2.changeGlobalOption',[{'log':log,'log-level':'debug'}]);
  const queued=await invoke({name:'cloud_download_url',args:{provider:'xunlei',fileId:file.id,fileName:file.name}});
  if(!queued.ok)throw Error('queue:'+queued.code);
  const id=queued.result.id;
  console.log(JSON.stringify({downloadTest:'queued',id}));
  let pausedAt=0,resumed=false,done=false,retriedAt=0,prefixHash,originalGid;
  const prefixDigest=()=>{
    const fd=fs.openSync(path.join(data,'downloads',file.name),'r');
    try{const bytes=Buffer.alloc(256*1024);if(fs.readSync(fd,bytes,0,bytes.length,0)!==bytes.length)throw Error('prefix_missing');return require('node:crypto').createHash('sha256').update(bytes).digest('hex');}
    finally{fs.closeSync(fd);}
  };
  const started=Date.now();
  try{
    for(let n=0;n<150;n++){
      const all=await invoke({name:'list_tasks',args:{}}),task=all.result?.find(t=>t.id===id);
      if(!task)throw Error('isolated_task_missing');
      if(n%5===0||task.status==='failed')console.log(JSON.stringify({second:Math.round((Date.now()-started)/1000),status:task.status,completed:task.completed_length,speed:task.download_speed,errorCode:task.aria_error_code,emptyReply:task.error?.includes('Empty reply')}));
      const naturalFailure=task.status==='failed'&&retryProbe&&resumed&&!retriedAt&&task.completed_length>=256*1024;
      if(task.status==='failed'&&!naturalFailure)throw Error('isolated_download_failed');
      if(task.status==='completed'){if(retryProbe)throw Error('completed_before_resume_verification');done=true;break;}
      if(!resumed&&task.completed_length>=(retryProbe?256*1024:1024*1024)){
        const pause=await invoke({name:'task_action',args:{taskId:id,action:'pause'}});
        if(!pause.ok)throw Error('pause:'+pause.code);
        await delay(1500);
        const state=await invoke({name:'list_tasks',args:{}});pausedAt=state.result.find(t=>t.id===id).completed_length;
        const resume=await invoke({name:'task_action',args:{taskId:id,action:'resume'}});
        if(!resume.ok)throw Error('resume:'+resume.code);
        resumed=true;console.log(JSON.stringify({downloadTest:'resumed',pausedAt}));
      }
      if(retryProbe&&resumed&&!retriedAt&&(naturalFailure||task.completed_length>=pausedAt+128*1024)){
        // This bridge forwards unchanged bytes through the existing proxy.
        // Only the owned test task is disconnected; retry restores that same route.
        if(!naturalFailure&&!bridgeConnections)throw Error('fault_bridge_not_used');
        console.log(JSON.stringify({downloadTest:'disconnect_probe',naturalFailure,bridgeConnections}));
        disconnect=!naturalFailure;if(disconnect)for(const socket of sockets)socket.destroy();
        try{
          let failed=naturalFailure;
          for(let attempt=0;attempt<50&&!failed;attempt++){
            const state=await invoke({name:'list_tasks',args:{}});failed=state.result.find(t=>t.id===id)?.status==='failed';
            if(!failed)await delay(500);
          }
          if(!failed)throw Error('injected_disconnect_not_observed');
        }finally{disconnect=false;}
        const local=new DatabaseSync(path.join(data,'tasks.sqlite'),{readOnly:true});
        const stopped=local.prepare('select aria_gid,completed_length from tasks where id=?').get(id);local.close();
        originalGid=stopped.aria_gid;retriedAt=stopped.completed_length;prefixHash=prefixDigest();
        const retry=await invoke({name:'task_action',args:{taskId:id,action:'retry'}});
        if(!retry.ok||retry.result?.duplicate!=='resume_available')throw Error('native_resume_not_offered');
        const continuation=await invoke({name:'resolve_duplicate_download',args:{taskId:id,action:'resume'}});
        if(!continuation.ok)throw Error('native_retry:'+continuation.code);
        console.log(JSON.stringify({downloadTest:'native_retry_requested',retriedAt}));
      }
      if(retryProbe&&retriedAt&&task.completed_length>=retriedAt+512*1024){
        if(prefixDigest()!==prefixHash)throw Error('resumed_prefix_changed');
        const local=new DatabaseSync(path.join(data,'tasks.sqlite'),{readOnly:true});
        const current=local.prepare('select aria_gid from tasks where id=?').get(id);local.close();
        if(current.aria_gid===originalGid)throw Error('native_retry_did_not_resubmit');
        const options=await rpc('aria2.getOption',[current.aria_gid]);
        for(const [key,expected] of Object.entries({'max-tries':'5','retry-wait':'3','continue':'false','allow-overwrite':'false','auto-file-renaming':'false'}))if(options[key]!==expected)throw Error('unexpected_resume_option:'+key);
        console.log(JSON.stringify({downloadTest:'native_retry_progress',prefixUnchanged:true,newGid:true,retriedAt,completed:task.completed_length,retryWait:options['retry-wait']}));
        done=true;break;
      }
      if(!retryProbe&&resumed&&task.completed_length>=pausedAt+4*1024*1024){done=true;break;}
      await delay(1000);
    }
    if(!done)throw Error('download_progress_timeout');
    console.log(JSON.stringify({downloadTest:'post_resume_progress',resumed,pausedAt}));
  }finally{
    await invoke({name:'task_action',args:{taskId:id,action:'pause'}}).catch(()=>{});
    if(bridge){for(const socket of sockets)socket.destroy();await new Promise(resolve=>bridge.close(resolve));}
    const db=new DatabaseSync(path.join(data,'tasks.sqlite'),{readOnly:true});
    const task=db.prepare('select aria_gid,completed_length,total_length,status from tasks where id=?').get(id);db.close();
    const stateFile=path.join(data,'aria2-next-state','stream','state.db');
    if(fs.existsSync(stateFile)){
      const state=new DatabaseSync(stateFile,{readOnly:true});
      const record=state.prepare('select completed_length,total_length,completed_ranges from downloads where gid=?').get(task.aria_gid);
      console.log(JSON.stringify({nativeState:record||null,task}));state.close();
    }
    if(fs.existsSync(log)){
      const lines=fs.readFileSync(log,'utf8').split(/\r?\n/).filter(l=>/component=stream.*event=(range_finished|range_retry|response_rejected|download_failed)/.test(l));
      for(const line of lines)console.log(line.replace(/(?:uri|url|gid|remote)=[^\s]+/g,'$FIELD=<redacted>'));
    }
  }
};
