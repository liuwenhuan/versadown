// Real authentication against the official page in an isolated hidden app.
// Credentials stay in DPAPI storage and are never returned to this process.
const fs=require('node:fs'),os=require('node:os'),path=require('node:path'),net=require('node:net');
const {spawn}=require('node:child_process');
const {evaluate,targets}=require('./xunlei-live-probe.cjs');
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function availablePort(){
  return await new Promise((resolve,reject)=>{
    const server=net.createServer();
    server.once('error',reject);
    server.listen(0,'127.0.0.1',()=>{
      const port=server.address().port;
      server.close(error=>error?reject(error):resolve(port));
    });
  });
}
function removeIsolatedData(data){
  fs.rmSync(data,{recursive:true,force:true,maxRetries:240,retryDelay:250});
}
async function invoke(page,command,args={}){
  return evaluate(page,`(async()=>{try{return {ok:true,result:await window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})};}catch(e){return {ok:false,code:e?.code,message:e?.message};}})()`,210000);
}
async function main(){
  const password=process.env.VERSADOWN_XUNLEI_PASSWORD_LIVE==='1';
  const primary=path.join(process.env.APPDATA,'com.versadown.desktop');
  const data=fs.mkdtempSync(path.join(os.tmpdir(),'versadown-xunlei-auth-live-'));
  const retain=process.env.VERSADOWN_XUNLEI_KEEP_TEST_DATA==='1';
  let child,manager,spawnError,diagnostic='';
  try{
    const reuse=process.env.VERSADOWN_XUNLEI_REUSE_TEST_DATA;
    if(reuse&&(!path.resolve(reuse).startsWith(path.resolve(os.tmpdir())+path.sep)||!path.basename(reuse).startsWith('versadown-xunlei-auth-live-')))throw Error('invalid_test_profile');
    const filter=file=>!['LOCK','SingletonLock','SingletonCookie','SingletonSocket'].includes(path.basename(file));
    if(!password){
      const source=reuse?path.join(reuse,'webview2'):path.join(primary,'web-auth','xunlei');
      fs.cpSync(source,path.join(data,'webview2'),{recursive:true,filter});
    }else{
      fs.mkdirSync(path.join(data,'cloud-sessions'),{recursive:true});
      for(const name of ['cloud-password-xunlei.bin','cloud-password-draft-xunlei.bin']){
        const source=path.join(primary,'cloud-sessions',name);
        if(fs.existsSync(source))fs.copyFileSync(source,path.join(data,'cloud-sessions',name));
      }
    }
    const port=await availablePort();
    if(process.env.VERSADOWN_XUNLEI_DOWNLOAD_LIVE==='1'){
      const enginePort=await availablePort();
      const original=JSON.parse(fs.readFileSync(path.join(primary,'settings.json'),'utf8'));
      fs.writeFileSync(path.join(data,'settings.json'),JSON.stringify({downloadDirectory:path.join(data,'downloads'),proxy:original.proxy,aria2:{...original.aria2,rpcPort:enginePort,rpcToken:require('node:crypto').randomBytes(32).toString('hex')}}));
    }
    const quickBinary=path.resolve(__dirname,'..','artifacts','quick','versadown.exe');
    const releaseBinary=path.resolve(__dirname,'..','artifacts','versadown.exe');
    const binary=process.env.VERSADOWN_TEST_BINARY
      ? path.resolve(process.env.VERSADOWN_TEST_BINARY)
      : (fs.existsSync(quickBinary)?quickBinary:releaseBinary);
    if(!fs.existsSync(binary))throw Error(`test_binary_missing:${binary}`);
    child=spawn(binary,[],{cwd:path.dirname(binary),windowsHide:true,stdio:['ignore','pipe','pipe'],env:{...process.env,
      VERSADOWN_BACKGROUND_UI_TEST:'1',VERSADOWN_TEST_DATA_DIR:data,VERSADOWN_TEST_DOWNLOAD_DIR:path.join(data,'downloads'),
      WEBVIEW2_USER_DATA_FOLDER:path.join(data,'webview2'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port} --disable-background-timer-throttling`}});
    child.once('error',error=>{spawnError=error;});
    for(const stream of [child.stdout,child.stderr])stream?.on('data',chunk=>{diagnostic=(diagnostic+String(chunk)).slice(-4096);});
    for(let n=0;n<120&&!manager;n++){
      if(spawnError)throw spawnError;
      for(const page of (await targets(port).catch(()=>[])).filter(t=>t.type==='page'&&/tauri.localhost/.test(t.url))){
        if(await evaluate(page,"Array.from(document.querySelectorAll('button')).some(b=>b.textContent.includes('我的网盘'))").catch(()=>false)){manager=page;break;}
      }
      if(!manager)await delay(250);
    }
    if(!manager){
      const pages=await targets(port).catch(()=>[]);
      throw Error(`app_start_timeout:exit=${child.exitCode}:signal=${child.signalCode}:pages=${JSON.stringify(pages.map(p=>({type:p.type,url:p.url,title:p.title})))}:diagnostic=${diagnostic.replace(/https?:\/\/\S+/g,'<url>').replace(/[A-Za-z0-9_-]{48,}/g,'<redacted>')}`);
    }
    let clicked=false;
    for(let n=0;n<80&&!clicked;n++){
      clicked=await evaluate(manager,`(()=>{
        Array.from(document.querySelectorAll('button')).find(b=>b.textContent.includes('我的网盘'))?.click();
        const card=document.querySelector('.cloud-provider-xunlei');
        if(!card)return false;
        const passwordMode=${password};
        if(passwordMode&&(!card.querySelector('input[aria-label="账号"]')?.value||card.querySelector('input[aria-label="密码"]')?.placeholder!=='***'))return false;
        const button=Array.from(card.querySelectorAll('button')).find(b=>b.textContent===(passwordMode?'登录':'使用网页授权'));
        if(!button||button.disabled)return false;button.click();return true;
      })()`);
      if(!clicked)await delay(250);
    }
    if(!clicked){
      console.log(JSON.stringify({ui:await evaluate(manager,"({buttons:Array.from(document.querySelectorAll('button')).map(b=>b.textContent),card:Boolean(document.querySelector('.cloud-provider-xunlei')),savedAccount:Boolean(document.querySelector('.cloud-provider-xunlei input[aria-label=账号]')?.value),savedPassword:document.querySelector('.cloud-provider-xunlei input[aria-label=密码]')?.placeholder==='***'})")}));
      throw Error('authorization_card_or_saved_password_missing');
    }
    let ready=false;
    for(let n=0;n<195&&!ready;n++){
      const result=await invoke(manager,'cloud_completed_authorization',{provider:'xunlei'});
      ready=result.ok&&Boolean(result.result);if(!ready)await delay(1000);
    }
    if(!ready){
      const official=(await targets(port)).find(p=>/^https:\/\/pan.xunlei.com/.test(p.url));
      const status=official?await evaluate(official,"window.__versadownXunleiAuth?.status() || {phase:'missing_observer'}"):{};
      throw Error('authorization_timeout:'+JSON.stringify(status));
    }
    const root=await invoke(manager,'cloud_list_directory',{provider:'xunlei',parentId:'0'});
    if(!root.ok)throw Error('root:'+root.code+':'+root.message);
    const account=await invoke(manager,'cloud_account',{provider:'xunlei'});
    if(!account.ok)throw Error('restore:'+account.code+':'+account.message);
    const capacity=await invoke(manager,'cloud_account_capacity',{provider:'xunlei'});
    if(!capacity.ok)throw Error('capacity:'+capacity.code+':'+capacity.message);
    if(!(Number(capacity.result?.capacity?.totalBytes)>0))throw Error('capacity_missing_total');
    await delay(1000);
    const closed=!(await targets(port)).some(p=>/^https:\/\/(pan|i).xunlei.com/.test(p.url));
    if(!closed)throw Error('authorization_window_not_closed');
    const menu=await evaluate(manager,"Array.from(document.querySelectorAll('.cloud-provider-navigation button')).some(t=>t.textContent.includes('迅雷'))");
    if(!menu)throw Error('workspace_menu_missing');
    let capacityVisible=false;
    for(let n=0;n<20&&!capacityVisible;n++){
      capacityVisible=await evaluate(manager,"(()=>{const text=document.querySelector('.cloud-provider-xunlei .cloud-account-capacity')?.textContent||'';return /[0-9]/.test(text)&&!text.includes('更新失败');})()");
      if(!capacityVisible)await delay(250);
    }
    if(!capacityVisible)throw Error('capacity_not_visible_on_card');
    if(process.env.VERSADOWN_XUNLEI_ACCELERATION_PROBE==='1'){
      const result=require('node:child_process').spawnSync('powershell.exe',['-ExecutionPolicy','Bypass','-File',path.join(__dirname,'probe-xunlei-acceleration.ps1'),'-SessionDirectory',data],{encoding:'utf8',windowsHide:true,timeout:45000});
      process.stdout.write(result.stdout||'');if(result.status!==0)throw Error('acceleration_probe_failed');
    }
    if(process.env.VERSADOWN_XUNLEI_USAGE_PROBE==='1'){
      const result=require('node:child_process').spawnSync('powershell.exe',['-ExecutionPolicy','Bypass','-File',path.join(__dirname,'probe-xunlei-file-usage.ps1'),'-SessionDirectory',data,'-Measure'],{encoding:'utf8',windowsHide:true,timeout:120000});
      process.stdout.write(result.stdout||'');if(result.status!==0)throw Error('usage_probe_failed');
    }
    if(process.env.VERSADOWN_XUNLEI_DOWNLOAD_LIVE==='1'){
      await require('./verify-xunlei-download-live.cjs').run(command=>invoke(manager,command.name,command.args),root.result,data,account.result);
    }
    const summary={method:password?'password':'web',rootReadable:true,accountRestorable:true,capacityReadable:true,capacityVisible,authorizationWindowClosed:closed,workspaceMenu:menu};
    if(retain)summary.isolatedData=data;
    console.log(JSON.stringify(summary));
  }finally{
    if(manager&&child)await invoke(manager,'activate_launcher_menu_for_test',{action:'launcher.quit'}).catch(()=>{});
    if(child&&!spawnError){
      for(let n=0;n<40&&child.exitCode===null;n++)await delay(200);
    }
    if(child&&child.exitCode===null)child.kill();
    if(retain){
      console.log(JSON.stringify({testData:data,retained:true}));
    }else{
      try{
        removeIsolatedData(data);
      }catch(error){
        console.error(`cleanup_failed:${error.code||error.message}:testData=${data}`);
        process.exitCode=1;
      }
    }
  }
}
main().catch(e=>{console.error(e.message);process.exitCode=1;});
