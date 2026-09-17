const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../src-tauri/src/cloud/driver/native/xunlei_auth/observer.js'),'utf8');
const token=sub=>'header.'+Buffer.from(JSON.stringify({sub,exp:4000000000})).toString('base64url')+'.sig';
const h=(sub='a')=>({'Authorization':'Bearer '+token(sub),'X-Client-Id':'Xqp0kJBXWhwaTpB6','X-Device-Id':'device','X-Captcha-Token':'ticket'});
function fixture(origin='https://pan.xunlei.com') {
  const replies=[];const calls=[];
  class Xhr {open(){} setRequestHeader(){} send(){} addEventListener(type,callback){this.callback=callback;} }
  const window={PAN_VERSION:'future-version',addEventListener(type,callback){this.message=callback;},fetch:(...args)=>{calls.push(args);return Promise.resolve(replies.shift());}};
  const context=vm.createContext({window,location:{origin,href:origin+'/'},navigator:{userAgent:'Browser'},Headers,URL,XMLHttpRequest:Xhr,atob});
  vm.runInContext(source,context);
  return {window,calls,replies,Xhr,snapshot:()=>JSON.parse(JSON.stringify(window.__alldownloaderXunleiAuth.snapshot())),
    async request(url,headers=h(),status=200,value={files:[]},method='GET',body){
      const response={status,clone:()=>({json:()=>Promise.resolve(value)})};replies.push(response);
      assert.equal(await window.fetch(url,{headers,method,body}),response);
      await new Promise(resolve=>setImmediate(resolve));
    }};
}
const root='https://api-pan.xunlei.com/drive/v1/files?parent_id=';
test('observes successful official root without generating any extra request',async()=>{
  const f=fixture();await f.request(root);
  assert.equal(f.calls.length,1);assert.equal(f.snapshot().access_token,token('a'));
  assert.equal(f.snapshot().user_id,'a');assert.equal(f.snapshot().web_security.captcha_token,'ticket');
});
test('rejected, malformed, foreign and non-root responses cannot authorize',async()=>{
  for(const [url,status,value] of [[root,401,{files:[]}],[root,200,{}],[root+'nested',200,{files:[]}],['https://api-pan.xunlei.com.evil.test/drive/v1/files',200,{files:[]}]]){
    const f=fixture();await f.request(url,h(),status,value);assert.equal(f.snapshot().pending,true);
  }
});
test('account change invalidates the earlier candidate until new root succeeds',async()=>{
  const f=fixture();await f.request(root);await f.request('https://xluser-ssl.xunlei.com/v1/user/me',h('b'),200,{sub:'b'});
  assert.equal(f.snapshot().pending,true);await f.request(root,h('b'));assert.equal(f.snapshot().user_id,'b');
});
test('captures only matching official captcha metadata',async()=>{
  const f=fixture();await f.request('https://xluser-ssl.xunlei.com/v1/user/me',h(),200,{sub:'a'});
  await f.request('https://xluser-ssl.xunlei.com/v1/shield/captcha/init',{},200,{captcha_token:'ticket',expires_in:600},'POST',JSON.stringify({client_id:'Xqp0kJBXWhwaTpB6',device_id:'device',meta:{user_id:'a',package_name:'pan.xunlei.com',client_version:'future-version',captcha_sign:'official-sign',timestamp:String(Date.now())}}));
  await f.request(root);assert.equal(f.snapshot().web_security.captcha_sign,'official-sign');
  assert.equal(f.calls.length,3);
});
test('does not install on a foreign or non-HTTPS origin',()=>{
  for(const origin of ['http://pan.xunlei.com','https://i.xunlei.com','https://pan.xunlei.com.evil.test'])assert.equal(fixture(origin).window.__alldownloaderXunleiAuth,undefined);
});
test('observes XMLHttpRequest root responses used by the official page',()=>{
  const f=fixture(),xhr=new f.Xhr();xhr.open('GET',root);
  for(const [key,value] of Object.entries(h()))xhr.setRequestHeader(key,value);
  xhr.send();xhr.status=200;xhr.responseText=JSON.stringify({files:[]});xhr.callback();
  assert.equal(f.snapshot().user_id,'a');assert.equal(f.calls.length,0);
});
test('retains a matching OAuth refresh token captured before the first authenticated request',async()=>{
  const f=fixture();
  await f.request('https://xluser-ssl.xunlei.com/v1/auth/token',{},200,{access_token:token('a'),refresh_token:'refresh-a'},'POST');
  await f.request(root);assert.equal(f.snapshot().refresh_token,'refresh-a');
});
test('reports incompatible password form only from the trusted official iframe',()=>{
  const f=fixture();
  f.window.message({origin:'https://i.xunlei.com.evil.test',data:{type:'alldownloader-xunlei-password',error:'plugin_update_required'}});
  assert.equal(f.snapshot().pending,true);
  f.window.message({origin:'https://i.xunlei.com',data:{type:'alldownloader-xunlei-password',error:'plugin_update_required'}});
  assert.equal(f.snapshot().authorization_error,'plugin_update_required');
});
