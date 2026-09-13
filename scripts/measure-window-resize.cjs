// Isolated WebView2 resize benchmark. Never opens the user's task database.
const { spawn, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const WebSocket = require('ws');
const assert = require('node:assert/strict');
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function connect(url) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const pending = new Map();
    let id = 0;
    socket.on('error', reject);
    socket.on('message', (data) => {
      const result = JSON.parse(data);
      const waiter = pending.get(result.id);
      if (!waiter) return;
      pending.delete(result.id);
      clearTimeout(waiter.timer);
      if (result.error) waiter.reject(new Error(JSON.stringify(result.error)));
      else waiter.resolve(result.result);
    });
    socket.on('open', () => resolve({
      close: () => socket.close(),
      send: (method, params = {}) => new Promise((resolve, reject) => {
        const requestId = ++id;
        const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`Timed out: ${method}`)); }, 20000);
        pending.set(requestId, { resolve, reject, timer });
        socket.send(JSON.stringify({ id: requestId, method, params }));
      }),
    }));
  });
}

async function run() {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'alldownloader-resize-'));
  const executable = path.resolve(process.argv[2] || 'artifacts/window-resize-test/alldownloader.exe');
  const child = spawn(executable, [], { windowsHide: true, stdio: 'ignore', env: {
    ...process.env, ALLDOWNLOADER_BACKGROUND_UI_TEST: '1',
    ALLDOWNLOADER_TEST_DATA_DIR: root, ALLDOWNLOADER_TEST_DOWNLOAD_DIR: path.join(root, 'downloads'),
    WEBVIEW2_USER_DATA_FOLDER: path.join(root, 'webview2'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  } });
  let cdp;
  let evaluate;
  try {
    for (let attempt = 0; attempt < 80; attempt++) {
      if (child.exitCode !== null) throw new Error(`Test app exited: ${child.exitCode}`);
      try {
        const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        for (const target of targets.filter((t) => t.type === 'page')) {
          const candidate = await connect(target.webSocketDebuggerUrl);
          const state = await candidate.send('Runtime.evaluate', { expression: 'Boolean(document.querySelector(".app-shell"))', returnByValue: true });
          if (state.result.value) { cdp = candidate; break; }
          candidate.close();
        }
        if (cdp) break;
      } catch {}
      await delay(200);
    }
    if (!cdp) throw new Error('No main WebView2 target');
    evaluate = async (expression) => {
      const response = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
      return response.result.value;
    };
    await delay(1000);
    console.log(JSON.stringify({ executable, initial: await evaluate('({width:innerWidth,height:innerHeight,dpi:devicePixelRatio})') }));
    const browserInfo = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    if (browserInfo.webSocketDebuggerUrl) {
      const browser = await connect(browserInfo.webSocketDebuggerUrl);
      try {
        const info = await browser.send('SystemInfo.getInfo');
        console.log(JSON.stringify({gpuFeatures:info.gpu.featureStatus}));
      } catch (error) {
        console.log(JSON.stringify({gpuDiagnosticsUnavailable:error.message}));
      } finally { browser.close(); }
    }
    if (process.argv.includes('--verify-minimum')) {
      // SetWindowPos/set_size can bypass interactive limits on Windows.
      // WM_GETMINMAXINFO is the limit actually used by native mouse resizing.
      const minimum = JSON.parse(execFileSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(__dirname, 'read-window-minimum.ps1'), '-AppProcessId', String(child.pid)], { windowsHide:true, encoding:'utf8' }));
      const trackWidth = minimum.minTrackWidth / minimum.dpiScale;
      const trackHeight = minimum.minTrackHeight / minimum.dpiScale;
      if (Math.abs(trackWidth - 960) > 1 || Math.abs(trackHeight - 640) > 1) throw new Error(`Expected 960x640 native minimum track size: ${JSON.stringify(minimum)}`);
      console.log(JSON.stringify({minimumSizeVerified:minimum}));
    }
    await evaluate(`window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', {action:'launcher.hide_manager'})`);
    if (process.argv.includes('--interaction-smoke')) {
      const initialInteraction = await evaluate(`window.__TAURI_INTERNALS__.invoke('main_window_interaction_state')`);
      assert.deepEqual(initialInteraction, {active:false,nativeDrag:true});
      const chrome = await evaluate(`(() => {
        const header=document.querySelector('.app-header');
        return { native:header.hasAttribute('data-native-window-drag'),
          jsRegions:header.querySelectorAll('[data-tauri-drag-region]').length,
          captions:['.brand','.brand img','.brand h1','.app-header-drag-space'].map(s=>getComputedStyle(header.querySelector(s)).getPropertyValue('app-region')),
          controls:[...header.querySelectorAll('textarea,.window-controls button')].map(e=>getComputedStyle(e).getPropertyValue('app-region')) };
      })()`);
      assert.equal(chrome.native, true);
      assert.equal(chrome.jsRegions, 0);
      assert.deepEqual(chrome.captions, ['drag','drag','drag','drag']);
      assert.ok(chrome.controls.length >= 4 && chrome.controls.every(value => value === 'no-drag'));
      const jsDragCalls = await evaluate(`(() => {
        const internals=window.__TAURI_INTERNALS__, original=internals.invoke;
        let calls=0;
        internals.invoke=function(command,...args) {
          if(command==='plugin:window|start_dragging') { calls++; return Promise.resolve(); }
          return original.call(this,command,...args);
        };
        try {
          for(const selector of ['.brand','.brand img','.brand h1','.app-header-drag-space']) {
            document.querySelector(selector).dispatchEvent(new MouseEvent('mousedown',{bubbles:true,button:0,buttons:1,detail:1}));
          }
          return calls;
        } finally { internals.invoke=original; }
      })()`);
      assert.equal(jsDragCalls, 0);
      const phase = (value) => execFileSync('powershell.exe', ['-NoProfile','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'probe-window-interaction.ps1'),'-AppProcessId',String(child.pid),'-Phase',value], {windowsHide:true,encoding:'utf8'});
      phase('enter');
      try {
        await delay(100);
        assert.equal((await evaluate(`window.__TAURI_INTERNALS__.invoke('main_window_interaction_state')`)).active, true);
        await evaluate(`window.__TAURI_INTERNALS__.invoke('plugin:window|set_size',{label:'main',value:{Logical:{width:1100,height:740}}})`);
        await delay(600);
        assert.equal(await evaluate(`document.documentElement.classList.contains('window-resizing')`), true, 'Native hold must outlive the 120ms fallback');
      } finally { phase('exit'); }
      await delay(200);
      assert.equal((await evaluate(`window.__TAURI_INTERNALS__.invoke('main_window_interaction_state')`)).active, false);
      assert.equal(await evaluate(`document.documentElement.classList.contains('window-resizing')`), false, 'Native release must restore effects');
      console.log(JSON.stringify({interactionSmoke:'passed',...chrome,jsDragCalls,nativeLifecycle:'enter / hold / exit verified; not a mouse-FPS benchmark'}));
      return;
    }
    await cdp.send('Performance.enable');
    // Synthetic, credential-free folder rows use the real production stylesheet.
    for (const rows of [7, 1000]) {
      await evaluate(`(() => {
        document.querySelector('#root').innerHTML = '<main class="app-shell"><header class="app-header"></header><div class="workspace"><nav></nav><section class="view-panel"><section class="cloud-workspace"><div class="cloud-workspace-tabs workspace-tab-list">Files</div><section class="cloud-file-manager"><div class="cloud-filebar">Root directory</div><div class="cloud-list"><div class="cloud-row cloud-columns"><span></span><span>Name</span><span>Size</span><span>Modified</span></div>' + Array.from({length:${rows}}, (_, i) => '<div class="cloud-row cloud-file-row"><span class="cloud-check-cell"><input type="checkbox"></span><span class="cloud-file-name"><i><svg viewBox="0 0 24 24"><path d="M2 4h8l2 2h10v15H2z"/></svg></i><b>Folder with a long filename '+i+'</b></span><span>—</span><span>2026-09-12 12:00:00</span></div>').join('') + '</div></section></section></section></div></main>';
      })()`);
      for (const optimized of [false, true, false, true]) {
        await evaluate(`(() => {
          document.getElementById('resize-experiment')?.remove();
          const style = document.createElement('style'); style.id='resize-experiment';
          style.textContent=${optimized} ? '.cloud-file-row{content-visibility:auto;contain-intrinsic-block-size:auto 53px}' : '.cloud-file-row{content-visibility:visible;contain-intrinsic-block-size:none}';
          document.head.appendChild(style);
        })()`);
        const before = await cdp.send('Performance.getMetrics');
        const result = await evaluate(`(async () => {
          const samples=[]; let resizeEvents=0; const onResize=()=>resizeEvents++; window.addEventListener('resize',onResize);
          for(let i=0;i<80;i++) {
            const start=performance.now();
            await window.__TAURI_INTERNALS__.invoke('plugin:window|set_size',{label:'main',value:{Logical:{width:960+(i%40)*6,height:660+(i%20)*3}}});
            document.querySelector('.cloud-list').getBoundingClientRect();
            samples.push(performance.now()-start);
            await new Promise(resolve=>setTimeout(resolve,8));
          }
          window.removeEventListener('resize',onResize); samples.sort((a,b)=>a-b);
          return {medianMs:samples[40],p95Ms:samples[76],resizeEvents,scrollHeight:document.querySelector('.cloud-list').scrollHeight};
        })()`);
        const after = await cdp.send('Performance.getMetrics');
        const baseline = Object.fromEntries(before.metrics.map((m)=>[m.name,m.value]));
        const delta = Object.fromEntries(after.metrics.filter((m)=>['LayoutDuration','RecalcStyleDuration','TaskDuration','LayoutCount','RecalcStyleCount'].includes(m.name)).map((m)=>[m.name,+(m.value-baseline[m.name]).toFixed(5)]));
        console.log(JSON.stringify({ rows, optimized, ...result, ...delta }));
      }
    }
    await evaluate(`window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test',{action:'launcher.quit'})`);
    cdp.close(); cdp = null;
    for(let i=0;i<60 && child.exitCode===null;i++) await delay(100);
  } finally {
    if (cdp && child.exitCode === null) {
      try {
        await evaluate(`window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test',{action:'launcher.quit'})`);
        for(let i=0;i<60 && child.exitCode===null;i++) await delay(100);
      } catch {}
    }
    cdp?.close();
    if(child.exitCode===null) child.kill();
  }
}
run().catch((error)=>{ console.error(error); process.exitCode=1; });
