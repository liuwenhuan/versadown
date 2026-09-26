// Opt-in local app diagnostic. It never prints credentials or directory names.
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const WebSocket = require('ws');

async function evaluate(target, expression, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    const timer = setTimeout(() => { socket.close(); reject(new Error('evaluation_timeout')); }, timeoutMs);
    socket.once('error', () => { clearTimeout(timer); reject(new Error('debug_connection_failed')); });
    socket.once('open', () => socket.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } })));
    socket.on('message', data => {
      const message = JSON.parse(data);
      if (message.id !== 1) return;
      clearTimeout(timer); socket.close();
      if (message.error || message.result?.exceptionDetails) reject(new Error('page_evaluation_failed'));
      else resolve(message.result.result.value);
    });
  });
}

async function targets(port) {
  return (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
}

async function start() {
  const binary = process.env.VERSADOWN_TEST_BINARY || path.resolve(__dirname, '../artifacts/VersaDown.exe');
  if (!fs.existsSync(binary)) throw new Error('binary_not_found');
  const port = await new Promise(resolve => {
    const server = net.createServer(); server.listen(0, '127.0.0.1', () => { const port = server.address().port; server.close(() => resolve(port)); });
  });
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'versadown-xunlei-live-'));
  const child = spawn(binary, [], { cwd: path.dirname(binary), windowsHide: true, stdio: 'ignore', detached: true, env: {
    ...process.env, VERSADOWN_BACKGROUND_UI_TEST: '1', VERSADOWN_TEST_DATA_DIR: dataDir,
    VERSADOWN_TEST_DOWNLOAD_DIR: path.join(dataDir, 'downloads'),
    WEBVIEW2_USER_DATA_FOLDER: path.join(dataDir, 'webview2'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  }});
  child.unref();
  for (let attempt = 0; attempt < 80; attempt++) {
    const pages = await targets(port).catch(() => []);
    for (const manager of pages.filter(t => t.type === 'page' && /tauri\.localhost|^tauri:/.test(t.url))) {
      // Exercise the actual React authorization flow, including polling and
      // creation of the provider tab, rather than bypassing it with an invoke.
      const result = await evaluate(manager, `(()=>{
        const area=document.querySelector('.cloud-provider-xunlei');
        const button=area&&Array.from(area.querySelectorAll('button')).find(b=>b.textContent==='使用网页授权');
        if(!button)return {ok:false};
        Array.from(document.querySelectorAll('button')).find(b=>b.textContent.includes('我的网盘'))?.click();
        button.click();return {ok:true};
      })()`);
      if (result?.ok) {
        console.log(JSON.stringify({ ...result, port, pid: child.pid, dataDir }));
        return;
      }
    }
    await new Promise(r => setTimeout(r, 250));
  }
  throw new Error('app_start_timeout');
}

if (require.main === module) start().catch(e => { console.error(e.message); process.exitCode = 1; });
module.exports = { evaluate, targets };
