// Diagnostic only: one supplied signed link, explicit existing proxy, no credentials printed.
const net=require('node:net'),tls=require('node:tls'),http=require('node:http'),http2=require('node:http2');
const {performance}=require('node:perf_hooks');
async function probe(config,protocol){
  const uri=new URL(config.url),proxy=new URL(process.env.HTTPS_PROXY||process.env.https_proxy||'');
  if(uri.protocol!=='https:'||proxy.protocol!=='http:')throw Error('https_target_and_existing_http_proxy_required');
  const began=performance.now();let raw,secure,request,session,timer;
  const result={probe:protocol,proxy:true,status:0,bytes:0,range:'',alpn:'',tlsMs:0,firstByteMs:0,error:''};
  try{
    await new Promise((resolve,reject)=>{
      raw=net.connect(Number(proxy.port)||80,proxy.hostname);
      raw.setTimeout(12000,()=>reject(Error('proxy_timeout')));raw.once('error',reject);
      raw.once('connect',()=>{
        const auth=proxy.username?'Proxy-Authorization: Basic '+Buffer.from(decodeURIComponent(proxy.username)+':'+decodeURIComponent(proxy.password)).toString('base64')+'\r\n':'';
        raw.write(`CONNECT ${uri.hostname}:443 HTTP/1.1\r\nHost: ${uri.hostname}:443\r\n${auth}\r\n`);
      });
      let incoming=Buffer.alloc(0);
      const read=chunk=>{
        incoming=Buffer.concat([incoming,chunk]);const end=incoming.indexOf('\r\n\r\n');if(end<0)return;
        raw.removeListener('data',read);
        if(!/^HTTP\/1\.[01] 200 /.test(incoming.toString('latin1',0,end)))return reject(Error('proxy_connect_rejected'));
        if(incoming.length>end+4)raw.unshift(incoming.subarray(end+4));resolve();
      };
      raw.on('data',read);
    });
    raw.setTimeout(0);
    secure=tls.connect({socket:raw,servername:uri.hostname,ALPNProtocols:protocol==='h2'?['h2','http/1.1']:['http/1.1']});
    await new Promise((resolve,reject)=>{secure.once('secureConnect',resolve);secure.once('error',reject);secure.setTimeout(12000,()=>reject(Error('tls_timeout')));});
    result.tlsMs=Math.round(performance.now()-began);result.alpn=secure.alpnProtocol||'none';secure.setTimeout(0);
    await new Promise(resolve=>{
      let finished=false;
      const finish=error=>{if(finished)return;finished=true;result.error=error||'';resolve();};
      timer=setTimeout(()=>finish('sample_timeout'),15000);
      const onData=chunk=>{if(!result.firstByteMs)result.firstByteMs=Math.round(performance.now()-began);result.bytes+=chunk.length;if(result.bytes>=1048576)finish();};
      const headers={'user-agent':config.userAgent,referer:'https://pan.xunlei.com/'};
      if(protocol!=='no-range')headers.range='bytes=3414400-4462975';
      if(result.alpn==='h2'){
        session=http2.connect(uri.origin,{createConnection:()=>secure});session.on('error',e=>finish(e.code||'h2_error'));
        request=session.request({':path':uri.pathname+uri.search,...headers});
        request.on('response',h=>{result.status=h[':status'];result.range=h['content-range']||'';});
      }else{
        // http.request owns parsing only; createConnection returns the already proxied TLS socket.
        request=http.request({hostname:uri.hostname,port:443,path:uri.pathname+uri.search,method:'GET',headers,createConnection:()=>secure},response=>{
          result.status=response.statusCode;result.range=response.headers['content-range']||'';
          response.on('data',onData);response.on('end',()=>finish());response.on('error',e=>finish(e.code||'response_error'));
        });
      }
      request.on('data',onData);request.on('end',()=>finish());request.on('error',e=>finish(e.code||'request_error'));request.end();
      secure.on('close',()=>{if(!result.status)finish('closed_before_http_response');});
    });
  }catch(e){result.error=e.code||(/^[a-z_]+$/.test(e.message)?e.message:'transport_error');}
  finally{clearTimeout(timer);request?.destroy();session?.destroy();secure?.destroy();raw?.destroy();}
  result.elapsedMs=Math.round(performance.now()-began);result.bytesPerSecond=Math.round(result.bytes/(result.elapsedMs/1000));
  console.log(JSON.stringify(result));
}
let input='';process.stdin.setEncoding('utf8');process.stdin.on('data',part=>input+=part);
process.stdin.on('end',async()=>{try{const config=JSON.parse(input.replace(/^\uFEFF/,''));for(const p of ['no-range','h1','h2'])await probe(config,p);}catch{console.error('transport_probe_failed');process.exitCode=1;}});
