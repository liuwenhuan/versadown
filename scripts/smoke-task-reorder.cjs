const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const esbuild = require('esbuild');
const fs = require('node:fs/promises');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function connectCdp(browser) {
  const input = browser.stdio[3];
  const output = browser.stdio[4];
  let nextId = 0;
  let buffer = Buffer.alloc(0);
  const pending = new Map();
  output.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    let separator;
    while ((separator = buffer.indexOf(0)) >= 0) {
      const raw = buffer.subarray(0, separator).toString('utf8');
      buffer = buffer.subarray(separator + 1);
      if (!raw) continue;
      const message = JSON.parse(raw);
      const request = pending.get(message.id);
      if (!request) continue;
      pending.delete(message.id);
      clearTimeout(request.timer);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    }
  });
  return {
    close: () => input.end(),
    send(method, params = {}, sessionId) {
      return new Promise((resolve, reject) => {
        const id = ++nextId;
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`CDP command timed out: ${method}`));
        }, 5_000);
        pending.set(id, { resolve, reject, timer });
        input.write(`${JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })}\0`);
      });
    },
  };
}

async function main() {
  const root = path.resolve(__dirname, '..');
  const chrome = process.argv[2] || 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe';
  const profile = await fs.mkdtemp(path.join(os.tmpdir(), 'versadown-task-reorder-'));
  const bundle = await esbuild.build({
    absWorkingDir: root,
    entryPoints: ['scripts/task-reorder-fixture.tsx'],
    bundle: true,
    write: false,
    outdir: 'task-reorder-smoke-out',
    platform: 'browser',
    format: 'iife',
    jsx: 'automatic',
  });
  const script = bundle.outputFiles.find((file) => file.path.endsWith('.js')).text;
  const styles = bundle.outputFiles.find((file) => file.path.endsWith('.css'))?.text ?? '';
  const server = http.createServer((request, response) => {
    if (request.url === '/fixture.js') {
      response.setHeader('Content-Type', 'text/javascript');
      response.end(script);
      return;
    }
    if (request.url === '/fixture.css') {
      response.setHeader('Content-Type', 'text/css');
      response.end(styles);
      return;
    }
    response.setHeader('Content-Type', 'text/html; charset=utf-8');
    response.end('<!doctype html><html><head><link rel="stylesheet" href="/fixture.css"></head><body><div id="root"></div><script src="/fixture.js"></script></body></html>');
  });
  let browser;
  let cdp;
  try {
    process.stdout.write('Starting isolated task fixture...\n');
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
    const fixtureUrl = `http://127.0.0.1:${server.address().port}/`;
    process.stdout.write('Launching hidden Chrome...\n');
    browser = spawn(chrome, [
      '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--remote-debugging-pipe',
      `--user-data-dir=${profile}`, 'about:blank',
    ], { windowsHide: true, stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe'] });
    browser.stderr.on('data', (chunk) => process.stderr.write(chunk));
    cdp = connectCdp(browser);
    await cdp.send('Browser.getVersion');
    const { targetId } = await cdp.send('Target.createTarget', { url: fixtureUrl });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const send = (method, params = {}) => cdp.send(method, params, sessionId);
    process.stdout.write('Sending real mouse input...\n');
    await send('Runtime.enable');
    await send('Emulation.setDeviceMetricsOverride', { width: 960, height: 640, deviceScaleFactor: 1, mobile: false });
    process.stdout.write('Renderer connected.\n');

    await delay(1_000);
    const rowResult = await send('Runtime.evaluate', {
      expression: `Array.from(document.querySelectorAll('.task-row')).map(row => { const rect=row.getBoundingClientRect(); return {id:row.dataset.taskId,name:row.querySelector('.task-copy b')?.textContent,x:rect.x,y:rect.y,width:rect.width,height:rect.height}; })`,
      returnByValue: true,
    });
    const rows = rowResult.result.value;
    assert.equal(rows.length, 3, 'fixture must render three task rows');
    process.stdout.write('Three task rows rendered.\n');
    const byId = Object.fromEntries(rows.map((row) => [row.id, row]));
    const click = async (row, modifiers = 0) => {
      const x = row.x + 120;
      const y = row.y + row.height / 2;
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y, modifiers });
      await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', buttons: 1, clickCount: 1, modifiers });
      await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', buttons: 0, clickCount: 1, modifiers });
    };
    await click(byId.one);
    await click(byId.two, 2);
    process.stdout.write('Two task rows selected.\n');

    const fromX = byId.one.x + 120;
    const fromY = byId.one.y + byId.one.height / 2;
    const toX = byId.three.x + 120;
    const toY = byId.three.y + 5;
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: fromX, y: fromY });
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: fromX, y: fromY, button: 'left', buttons: 1, clickCount: 1 });
    for (let step = 1; step <= 8; step += 1) {
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: fromX + (toX - fromX) * step / 8, y: fromY + (toY - fromY) * step / 8, button: 'left', buttons: 1 });
    }
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: toX, y: toY, button: 'left', buttons: 0, clickCount: 1 });
    process.stdout.write('Mouse drag dispatched.\n');

    let order = [];
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const result = await send('Runtime.evaluate', { expression: `Array.from(document.querySelectorAll('.task-copy > b')).map(node => node.textContent)`, returnByValue: true });
      order = result.result.value;
      if (order.join(',') === 'one.zip,two.zip,three.zip') break;
      await delay(50);
    }
    assert.deepEqual(order, ['one.zip', 'two.zip', 'three.zip']);
    process.stdout.write(`Background pointer drag passed: ${order.join(' -> ')}\n`);
  } finally {
    process.stdout.write('Cleaning up background test...\n');
    cdp?.close();
    browser?.kill();
    await Promise.race([new Promise((resolve) => server.close(resolve)), delay(2_000)]);
    await Promise.race([fs.rm(profile, { recursive: true, force: true, maxRetries: 1 }).catch(() => undefined), delay(2_000)]);
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
