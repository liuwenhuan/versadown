// Isolated, read-only protocol observation. Never print token/header values.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const { spawn } = require('node:child_process');
const { evaluate, targets } = require('./xunlei-live-probe.cjs');
const WebSocket = require('ws');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function main() {
  const source = process.env.VERSADOWN_XUNLEI_REUSE_TEST_DATA;
  if (!source || !path.resolve(source).startsWith(path.resolve(os.tmpdir()) + path.sep)
    || !path.basename(source).startsWith('versadown-xunlei-auth-live-')) throw Error('isolated_profile_required');
  const data = fs.mkdtempSync(path.join(os.tmpdir(), 'versadown-xunlei-protocol-'));
  fs.cpSync(path.join(source, 'webview2'), path.join(data, 'webview2'), {recursive:true,
    filter: source => !['LOCK','SingletonLock','SingletonCookie','SingletonSocket'].includes(path.basename(source))});
  const port = await new Promise(resolve => {const server = net.createServer();server.listen(0,'127.0.0.1',()=>{const p=server.address().port;server.close(()=>resolve(p));});});
  const binary = path.resolve('artifacts/versadown.exe');
  const child = spawn(binary, [], {cwd:path.dirname(binary),windowsHide:true,stdio:'ignore',env:{...process.env,
    VERSADOWN_BACKGROUND_UI_TEST:'1',VERSADOWN_TEST_DATA_DIR:data,VERSADOWN_TEST_DOWNLOAD_DIR:path.join(data,'downloads'),
    WEBVIEW2_USER_DATA_FOLDER:path.join(data,'webview2'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`}});
  let manager, socket;
  try {
    for(let n=0;n<120&&!manager;n++) {manager=(await targets(port).catch(()=>[])).find(t=>/tauri.localhost/.test(t.url));if(!manager)await delay(250);}
    if(!manager)throw Error('startup_timeout');
    await evaluate(manager,"window.__TAURI_INTERNALS__.invoke('cloud_begin_authorization',{provider:'xunlei'})");
    let page;
    for(let n=0;n<80&&!page;n++){page=(await targets(port)).find(t=>/^https:\/\/pan.xunlei.com/.test(t.url));if(!page)await delay(250);}
    if(!page)throw Error('no_official_page');
    socket=new WebSocket(page.webSocketDebuggerUrl);
    const pending=new Map();let seq=0;const requests=new Map();
    const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params}));});
    socket.on('message', async raw=>{const m=JSON.parse(raw);if(m.id){const p=pending.get(m.id);if(p){pending.delete(m.id);m.error?p.reject(Error('cdp')):p.resolve(m.result);}return;}
      if(m.method==='Network.requestWillBeSent'){
        const r=m.params.request;const u=new URL(r.url);if(!['api-pan.xunlei.com','xluser-ssl.xunlei.com'].includes(u.hostname))return;
        requests.set(m.params.requestId,{url:u,headers:r.headers});
        const h=Object.fromEntries(Object.entries(r.headers).map(([k,v])=>[k.toLowerCase(),v]));
        let claims={};try{claims=JSON.parse(Buffer.from((h.authorization||'').replace(/^Bearer /,'').split('.')[1]||'','base64url').toString());}catch{}
        let body={};try{body=JSON.parse(r.postData||'{}');}catch{}
        console.log(JSON.stringify({event:'request',host:u.hostname,path:u.pathname,method:r.method,headers:Object.keys(h),jwtKeys:Object.keys(claims),sub:!!claims.sub,userId:!!claims.user_id,bodyKeys:Object.keys(body),metaKeys:Object.keys(body.meta||{})}));
      }
      if(m.method==='Network.responseReceived'&&requests.has(m.params.requestId)){
        const {url}=requests.get(m.params.requestId);console.log(JSON.stringify({event:'response',path:url.pathname,status:m.params.response.status}));
      }
    });
    await new Promise((resolve,reject)=>{socket.once('open',resolve);socket.once('error',reject);});
    await call('Network.enable');
    await call('Page.enable');
    if(process.env.VERSADOWN_XUNLEI_OBSERVER_SCRIPT){
      await call('Page.addScriptToEvaluateOnNewDocument',{source:fs.readFileSync(process.env.VERSADOWN_XUNLEI_OBSERVER_SCRIPT,'utf8')});
    }
    await call('Page.navigate',{url:'https://pan.xunlei.com/'});
    await delay(20000);
    const safe=await evaluate(page,`(()=>({path:location.pathname,version:window.PAN_VERSION||'',observer:window.__versadownXunleiAuth?.status?.(),snapshotKeys:Object.keys(window.__versadownXunleiAuth?.snapshot?.()||{})}))()`);
    console.log(JSON.stringify(safe));
  } finally {
    socket?.close();
    if(manager)await evaluate(manager,"window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test',{action:'launcher.quit'})").catch(()=>{});
    for(let n=0;n<40&&child.exitCode===null;n++)await delay(200);
    if(child.exitCode===null)child.kill();
    console.log(JSON.stringify({temporaryProfile:data}));
  }
}
main().catch(e=>{console.error(e.message);process.exitCode=1;});
