const { spawn } = require('node:child_process');
const net = require('node:net');
const path = require('node:path');
const WebSocket = require('ws');

const projectRoot = path.resolve(__dirname, '..');
const executable = path.join(projectRoot, 'src-tauri', 'target', 'release', 'alldownloader.exe');

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function availablePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}

async function targetsAt(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`);
  if (!response.ok) throw new Error(`WebView2 target request failed: ${response.status}`);
  return await response.json();
}

async function waitForTargets(port) {
  let lastError;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    try {
      const targets = await targetsAt(port);
      if (targets.length >= 2) return targets;
    } catch (error) {
      lastError = error;
    }
    await delay(200);
  }
  throw lastError ?? new Error('Timed out waiting for both Tauri WebView2 targets.');
}

async function evaluate(target, expression) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    socket.on('open', () => socket.send(JSON.stringify({
      id: 1,
      method: 'Runtime.evaluate',
      params: { expression, awaitPromise: true, returnByValue: true }
    })));
    socket.on('error', reject);
    socket.on('message', (data) => {
      const message = JSON.parse(data.toString());
      if (message.id !== 1) return;
      socket.close();
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else if (message.result.exceptionDetails) reject(new Error(message.result.exceptionDetails.text));
      else resolve(message.result.result.value);
    });
  });
}

async function waitForEvaluation(target, expression, predicate) {
  let value;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    value = await evaluate(target, expression);
    if (predicate(value)) return value;
    await delay(100);
  }
  throw new Error(`Timed out waiting for renderer state. Last value: ${JSON.stringify(value)}`);
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function run() {
  const port = await availablePort();
  const child = spawn(executable, [], {
    cwd: path.dirname(executable),
    windowsHide: true,
    stdio: 'ignore',
    env: {
      ...process.env,
      ALLDOWNLOADER_BACKGROUND_UI_TEST: '1',
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`
    }
  });

  try {
    const targets = await waitForTargets(port);
    let launcher;
    let manager;
    for (const target of targets) {
      const role = await waitForEvaluation(
        target,
        'document.readyState === "complete" ? (document.querySelector(".launcher-shell") ? "launcher" : document.querySelector(".app-shell") ? "manager" : "unknown") : "loading"',
        (value) => value === 'launcher' || value === 'manager'
      );
      if (role === 'launcher') launcher = target;
      if (role === 'manager') manager = target;
    }
    assert(launcher, 'Launcher WebView2 target did not render.');
    assert(manager, 'Download manager WebView2 target did not render.');

    const compact = JSON.parse(await evaluate(launcher, `JSON.stringify((() => {
      const button = document.querySelector('.launcher-trigger');
      const rect = button.getBoundingClientRect();
      const style = getComputedStyle(button);
      return { width: innerWidth, height: innerHeight, left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, display: style.display, visibility: style.visibility, opacity: style.opacity, background: style.background };
    })())`));
    assert(compact.display !== 'none' && compact.visibility === 'visible' && compact.opacity === '1', 'Compact launcher button is not visible.');
    assert(compact.left >= 0 && compact.top >= 0 && compact.right <= compact.width + 0.5 && compact.bottom <= compact.height + 0.5, `Compact launcher is outside its viewport: ${JSON.stringify(compact)}`);
    assert(compact.background.includes('linear-gradient'), 'Compact launcher lost its colored background.');

    const nativeCompact = JSON.parse(await evaluate(launcher, `(async () => JSON.stringify({
      position: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_position', { label: 'launcher' }),
      size: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_size', { label: 'launcher' }),
      monitor: await window.__TAURI_INTERNALS__.invoke('plugin:window|current_monitor')
    }))()`));
    const workArea = nativeCompact.monitor.workArea;
    assert(nativeCompact.position.x >= workArea.position.x && nativeCompact.position.y >= workArea.position.y, `Launcher starts before the monitor work area: ${JSON.stringify(nativeCompact)}`);
    assert(nativeCompact.position.x + nativeCompact.size.width <= workArea.position.x + workArea.size.width, `Launcher extends beyond the right edge: ${JSON.stringify(nativeCompact)}`);
    assert(nativeCompact.position.y + nativeCompact.size.height <= workArea.position.y + workArea.size.height, `Launcher extends beyond the bottom edge: ${JSON.stringify(nativeCompact)}`);

    const nativeRegion = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('set_launcher_compact_shape', { compact: true })`);
    assert(nativeRegion && nativeRegion.width === nativeRegion.height && nativeRegion.width > 0, `Windows launcher region is not circular: ${JSON.stringify(nativeRegion)}`);
    assert(nativeRegion.x >= 0 && nativeRegion.y >= 0, `Windows launcher region starts outside its native window: ${JSON.stringify(nativeRegion)}`);

    await evaluate(launcher, `(() => {
      window.__launcherSmokeEvents = [];
      const button = document.querySelector('.launcher-trigger');
      for (const type of ['mousedown', 'mousemove', 'mouseup', 'click', 'contextmenu']) {
        button.addEventListener(type, (event) => window.__launcherSmokeEvents.push({ type, button: event.button, buttons: event.buttons, clientX: event.clientX, clientY: event.clientY }), { capture: true, once: true });
      }
      button.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, clientX: 10, clientY: 10 }));
      button.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, button: 0, buttons: 1, clientX: 20, clientY: 18 }));
      button.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, buttons: 0, clientX: 20, clientY: 18 }));
      button.dispatchEvent(new MouseEvent('click', { bubbles: true, button: 0 }));
      button.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, button: 2 }));
    })()`);
    const launcherInteractions = JSON.parse(await waitForEvaluation(
      launcher,
      `(async () => JSON.stringify({ state: await window.__TAURI_INTERNALS__.invoke('launcher_interaction_snapshot'), events: window.__launcherSmokeEvents, compact: Boolean(document.querySelector('.launcher-trigger')), expanded: Boolean(document.querySelector('.launcher-panel')) }))()`,
      (value) => {
        const state = JSON.parse(value).state;
        return state.dragRequests === 1 && state.menuRequests === 1;
      }
    ));
    assert(launcherInteractions.state.dragRequests === 1, `Launcher drag did not invoke native window dragging: ${JSON.stringify(launcherInteractions)}`);
    assert(launcherInteractions.state.menuRequests === 1, `Launcher right click did not request its native menu: ${JSON.stringify(launcherInteractions)}`);
    assert(launcherInteractions.compact && !launcherInteractions.expanded, `Drag or right click unexpectedly expanded the launcher: ${JSON.stringify(launcherInteractions)}`);

    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('plugin:event|emit_to', { target: { kind: 'AnyLabel', label: 'main' }, event: 'launcher-navigate', payload: 'settings' })`);
    const settingsVisible = await waitForEvaluation(
      manager,
      `Boolean(document.querySelector('.settings-panel'))`,
      Boolean
    );
    assert(settingsVisible, 'Launcher settings action did not navigate the manager to Settings.');

    await evaluate(launcher, 'document.querySelector("button[aria-label=\\"打开下载输入\\"]").click()');
    const expanded = JSON.parse(await waitForEvaluation(
      launcher,
      'JSON.stringify({ width: innerWidth, height: innerHeight, panel: Boolean(document.querySelector(".launcher-panel")), action: document.querySelector(".launcher-panel .primary")?.textContent })',
      (value) => {
        const state = JSON.parse(value);
        return state.panel && state.width >= 469 && state.height >= 299;
      }
    ));
    assert(expanded.width >= 469 && expanded.height >= 299, `Launcher did not expand to the input panel: ${JSON.stringify(expanded)}`);
    assert(expanded.action === '开始解析下载', 'Expanded launcher action label is incorrect.');

    await evaluate(launcher, 'document.querySelector("button[aria-label=\\"收起下载输入\\"]").click()');
    const collapsed = await waitForEvaluation(
      launcher,
      'Boolean(document.querySelector(".launcher-trigger")) && !document.querySelector(".launcher-panel") && innerWidth < 200 && innerHeight < 100',
      Boolean
    );
    assert(collapsed, 'Launcher did not collapse back to the floating button.');

    const restoredRegion = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('set_launcher_compact_shape', { compact: true })`);
    assert(restoredRegion && restoredRegion.width === restoredRegion.height, `Compact region was not restored after collapse: ${JSON.stringify(restoredRegion)}`);

    process.stdout.write(`Launcher background smoke passed: compact ${compact.width}x${compact.height}, native circular region ${nativeRegion.width}px, drag/context menu/settings verified, expanded ${expanded.width}x${expanded.height}.\n`);
  } finally {
    child.kill();
  }
}

run().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
