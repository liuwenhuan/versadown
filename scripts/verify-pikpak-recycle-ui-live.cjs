// Interactive, opt-in verification of the real packaged React UI while every
// native window remains hidden. Never invokes a cloud mutation command directly.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const net = require('node:net');
const { spawn } = require('node:child_process');
const readline = require('node:readline');
const { evaluate, targets } = require('./xunlei-live-probe.cjs');
const targetName = 'VersaDown-IT-pikpak-cache-1788958358211-root';
const binary = process.env.VERSADOWN_TEST_BINARY || path.resolve(__dirname, '../artifacts/versadown.exe');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function run() {
  if (process.env.VERSADOWN_LIVE_CLOUD !== '1') throw new Error('Explicit live test opt-in required');
  const root = path.resolve(__dirname, '../.tools');
  const data = fs.mkdtempSync(path.join(root, 'pikpak-ui-'));
  // MSIX can redirect APPDATA to a different credential store. Never silently
  // accept that default for a live-session regression test.
  const sessions = process.env.VERSADOWN_TEST_CLOUD_SESSIONS;
  if (!sessions) throw new Error('Explicit physical cloud session directory required');
  // Keep task/download databases separate but use the same encrypted session
  // store as the application, so token renewal cannot strand the user's login.
  fs.symlinkSync(sessions, path.join(data, 'cloud-sessions'), sessions.startsWith('\\\\') ? 'dir' : 'junction');
  const port = await new Promise(resolve => {
    const server = net.createServer();
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port; server.close(() => resolve(port));
    });
  });
  const child = spawn(binary, [], { cwd: path.dirname(binary), windowsHide: true, stdio: 'ignore', env: {
    ...process.env, VERSADOWN_BACKGROUND_UI_TEST: '1', VERSADOWN_TEST_DATA_DIR: data,
    VERSADOWN_TEST_DOWNLOAD_DIR: path.join(data, 'downloads'),
    WEBVIEW2_USER_DATA_FOLDER: path.join(data, 'webview2'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  }});
  console.log(JSON.stringify({ binary, pid: child.pid, port, modified: fs.statSync(binary).mtime.toISOString(), sha256: crypto.createHash('sha256').update(fs.readFileSync(binary)).digest('hex'), sessionSource: sessions, encryptedSessionSha256: crypto.createHash('sha256').update(fs.readFileSync(path.join(sessions, 'pikpak.bin'))).digest('hex') }));
  let page;
  try {
    for (let i = 0; i < 100 && !page; i++) {
      if (child.exitCode !== null) throw new Error('Exact executable exited before UI was ready');
      for (const candidate of await targets(port).catch(() => [])) {
        if (candidate.type === 'page' && await evaluate(candidate, 'Boolean(document.querySelector(".app-shell"))').catch(() => false)) { page = candidate; break; }
      }
      if (!page) await delay(200);
    }
    if (!page) throw new Error('Hidden application UI unavailable');
    async function hidden() {
      const state = await evaluate(page, `window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')`);
      if (state.visible) throw new Error('Background test unexpectedly visible; stopping');
      return state;
    }
    async function observe() {
      await hidden();
      return evaluate(page, `(() => {
        const manager = document.querySelector('[aria-label="PikPak文件管理器"]');
        const rows = manager ? [...manager.querySelectorAll('[data-cloud-file-row]')] : [];
        return {
          hidden: true,
          navigation: [...document.querySelectorAll('.cloud-provider-navigation button')].map(b => b.textContent.trim()),
          errors: [...document.querySelectorAll('[role="alert"]')].map(e => e.textContent.trim()).filter(Boolean),
          breadcrumb: manager?.querySelector('.cloud-breadcrumbs')?.textContent.trim(),
          rowCount: rows.length,
          targetPresent: rows.some(r => r.querySelector('b[title]')?.title === ${JSON.stringify(targetName)}),
          selected: rows.filter(r => r.getAttribute('aria-selected') === 'true').map(r => r.querySelector('b[title]')?.title),
          dialogs: [...document.querySelectorAll('[role="alertdialog"]')].map(e => ({label:e.getAttribute('aria-label'),text:e.textContent})),
          buttons: manager ? [...manager.querySelectorAll('button')].map(b => ({text:b.textContent.trim(),label:b.getAttribute('aria-label'),disabled:b.disabled})).filter(b=>b.text||b.label) : []
        };
      })()`);
    }
    console.log(JSON.stringify(await observe()));
    const lines = readline.createInterface({ input: process.stdin, terminal: false });
    for await (const line of lines) {
      const command = line.startsWith('confirm-delete ') ? 'confirm-delete' : line.trim();
      if (command === 'quit') break;
      try {
        await hidden();
        if (command === 'management' || command === 'pikpak') {
          const text = command === 'management' ? '我的网盘' : 'PikPak网盘';
          await evaluate(page, `(() => { const b=[...document.querySelectorAll('button')].filter(b=>b.textContent.trim()===${JSON.stringify(text)});if(b.length!==1)throw Error('Navigation not unique');b[0].click(); })()`);
        } else if (command === 'recycle') {
          await evaluate(page, `(() => {const r=[...document.querySelectorAll('[aria-label="PikPak文件管理器"] [data-cloud-file-row]')].find(r=>r.querySelector('b[title]')?.title==='回收站');if(!r)throw Error('Recycle folder not found');r.dispatchEvent(new MouseEvent('dblclick',{bubbles:true}));})()`);
        } else if (command === 'select') {
          await evaluate(page, `(() => {const rows=[...document.querySelectorAll('[aria-label="PikPak文件管理器"] [data-cloud-file-row]')];if(rows.some(r=>r.getAttribute('aria-selected')==='true'))throw Error('Unexpected existing selection');const r=rows.find(r=>r.querySelector('b[title]')?.title===${JSON.stringify(targetName)});if(!r)throw Error('Exact fixture missing');r.querySelector('input[type=checkbox]').click();})()`);
        } else if (command === 'prepare-delete' || command === 'confirm-delete') {
          if (command === 'confirm-delete' && line.trim() !== `confirm-delete ${targetName}`) throw new Error('Exact fixture deletion not confirmed');
          const selector = command === 'confirm-delete' ? '[role="alertdialog"][aria-label="确认永久删除"]' : '[aria-label="PikPak文件管理器"] .cloud-selection-actions';
          await evaluate(page, `(() => {const rows=[...document.querySelectorAll('[aria-label="PikPak文件管理器"] [data-cloud-file-row][aria-selected=true]')];if(rows.length!==1||rows[0].querySelector('b[title]')?.title!==${JSON.stringify(targetName)})throw Error('Exact single fixture selection required');const scope=document.querySelector(${JSON.stringify(selector)});const b=scope&&[...scope.querySelectorAll('button')].find(b=>b.textContent.trim()==='永久删除');if(!b||b.disabled)throw Error('Delete button unavailable');b.click();})()`);
        } else if (command === 'refresh') {
          await evaluate(page, `(() => {const b=document.querySelector('[aria-label="PikPak文件管理器"] button[aria-label="刷新"]');if(!b)throw Error('Refresh unavailable');b.click();})()`);
        } else if (command !== 'observe') throw new Error('Unknown test action');
        await delay(400);
        console.log(JSON.stringify(await observe()));
      } catch (error) { console.log(JSON.stringify({ error: error.message })); }
    }
    lines.close();
    process.stdin.pause();
  } finally {
    if (page) await evaluate(page, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test',{action:'launcher.quit'})`).catch(() => {});
    await delay(500);
    if (child.exitCode === null) child.kill();
    // Remove only the junction itself, never recurse into the user's sessions.
    fs.unlinkSync(path.join(data, 'cloud-sessions'));
  }
}
run().catch(error => { console.error(error.message); process.exitCode = 1; });
