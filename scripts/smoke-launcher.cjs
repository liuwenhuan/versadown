const { spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const WebSocket = require('ws');

const projectRoot = path.resolve(__dirname, '..');
const targetRoot = process.env.CARGO_TARGET_DIR
  ? path.resolve(projectRoot, process.env.CARGO_TARGET_DIR)
  : null;
const executable = targetRoot
  ? path.join(targetRoot, 'release', 'alldownloader.exe')
  : path.join(projectRoot, 'artifacts', 'alldownloader.exe');

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

async function waitForTargets(port, child) {
  let lastError;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    if (child?.exitCode !== null) {
      throw new Error(`Application exited before exposing its WebView2 targets (exit code ${child.exitCode}).`);
    }
    try {
      const targets = (await targetsAt(port)).filter((target) => target.type === 'page' && target.url !== 'about:blank');
      if (targets.length >= 2) return targets;
    } catch (error) {
      lastError = error;
    }
    await delay(200);
  }
  const failure = lastError?.message ?? 'Timed out waiting for both Tauri WebView2 targets.';
  throw new Error(`${failure} (application exit code: ${child?.exitCode ?? 'still running'}).`);
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
      else if (message.result.exceptionDetails) {
        const details = message.result.exceptionDetails;
        const description = details.exception?.description ?? details.exception?.value ?? JSON.stringify(details);
        reject(new Error(`${details.text}: ${description}`));
      }
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

async function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null) return child.exitCode;
  return await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`Application did not exit within ${timeoutMs}ms.`)), timeoutMs);
    child.once('exit', (code) => {
      clearTimeout(timeout);
      resolve(code);
    });
  });
}

function windowsChildProcessIds(parentPid) {
  const script = `$items = @(Get-CimInstance Win32_Process -Filter \"ParentProcessId = ${Number(parentPid)}\" | Where-Object { $_.Name -like 'aria2-next*' } | Select-Object -ExpandProperty ProcessId); ConvertTo-Json -Compress -InputObject $items`;
  const result = require('node:child_process').execFileSync(
    'powershell.exe',
    ['-NoProfile', '-NonInteractive', '-Command', script],
    { encoding: 'utf8', windowsHide: true }
  ).trim();
  if (!result) return [];
  const parsed = JSON.parse(result);
  return Array.isArray(parsed) ? parsed.map(Number) : [Number(parsed)];
}

function processExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error && error.code === 'EPERM';
  }
}

async function waitForOwnedSidecar(parentPid) {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const pids = windowsChildProcessIds(parentPid);
    if (pids.length === 1) return pids;
    if (pids.length > 1) throw new Error(`Expected one owned aria2 sidecar, found ${pids.join(', ')}.`);
    await delay(100);
  }
  throw new Error('Timed out waiting for the app-owned aria2 sidecar.');
}

async function waitForProcessesToStop(pids, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pids.every((pid) => !processExists(pid))) return;
    await delay(50);
  }
  throw new Error(`Owned aria2 sidecar did not stop: ${pids.filter(processExists).join(', ')}.`);
}

async function startSlowDownloadFixture() {
  const payloadSize = 64 * 1024 * 1024;
  const chunk = Buffer.alloc(64 * 1024, 0x5a);
  const server = http.createServer((request, response) => {
    response.writeHead(200, {
      'Accept-Ranges': 'bytes',
      'Content-Length': String(payloadSize),
      'Content-Type': 'application/octet-stream'
    });
    let sent = 0;
    const timer = setInterval(() => {
      if (sent >= payloadSize) {
        clearInterval(timer);
        response.end();
        return;
      }
      response.write(chunk);
      sent += chunk.length;
    }, 100);
    const stop = () => clearInterval(timer);
    request.once('close', stop);
    response.once('close', stop);
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  return {
    server,
    url: `http://127.0.0.1:${server.address().port}/launcher-quit-payload.bin`
  };
}

async function run() {
  const port = await availablePort();
  const testDataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alldownloader-ui-smoke-'));
  const downloadFixture = await startSlowDownloadFixture();
  let ownedSidecarPids = [];
  let completedQuitFlow = false;
  const child = spawn(executable, [], {
    cwd: path.dirname(executable),
    windowsHide: true,
    stdio: 'ignore',
    env: {
      ...process.env,
      ALLDOWNLOADER_BACKGROUND_UI_TEST: '1',
      ALLDOWNLOADER_TEST_DATA_DIR: testDataDir,
      ALLDOWNLOADER_TEST_DOWNLOAD_DIR: path.join(testDataDir, 'downloads'),
      WEBVIEW2_USER_DATA_FOLDER: path.join(testDataDir, 'webview2'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`
    }
  });

  try {
    const targets = await waitForTargets(port, child);
    let launcher;
    let manager;
    for (const target of targets) {
      const role = await waitForEvaluation(
        target,
        'document.readyState === "complete" ? (document.querySelector(".launcher-shell") ? "launcher" : document.querySelector(".app-shell") ? "manager" : `unknown:${location.href}:${document.body.innerText.slice(0, 120)}`) : "loading"',
        (value) => value === 'launcher' || value === 'manager'
      );
      if (role === 'launcher') launcher = target;
      if (role === 'manager') manager = target;
    }
    assert(launcher, 'Launcher WebView2 target did not render.');
    assert(manager, 'Download manager WebView2 target did not render.');
    ownedSidecarPids = await waitForOwnedSidecar(child.pid);

    const managerViewport = JSON.parse(await evaluate(manager, 'JSON.stringify({ width: innerWidth, height: innerHeight })'));
    assert(
      Math.abs(managerViewport.width - 960) <= 1 && Math.abs(managerViewport.height - 640) <= 1,
      `Main window did not use the requested 960x640 layout: ${JSON.stringify(managerViewport)}`
    );

    const titlebarLayout = JSON.parse(await evaluate(manager, `JSON.stringify((() => {
      const controls = document.querySelector('.window-controls').getBoundingClientRect();
      return { viewportWidth: innerWidth, controlsLeft: controls.left, controlsRight: controls.right };
    })())`));
    assert(
      Math.abs(titlebarLayout.controlsRight - (titlebarLayout.viewportWidth - 8)) <= 1,
      `Compact titlebar controls are not pinned to the trailing edge: ${JSON.stringify(titlebarLayout)}`
    );

    await evaluate(manager, `window.__TAURI_INTERNALS__.invoke('plugin:window|toggle_maximize', { label: 'main' })`);
    const maximizedTitlebarLayout = JSON.parse(await waitForEvaluation(
      manager,
      `JSON.stringify((() => {
        const controls = document.querySelector('.window-controls').getBoundingClientRect();
        return { viewportWidth: innerWidth, controlsRight: controls.right };
      })())`,
      (value) => {
        const layout = JSON.parse(value);
        return layout.viewportWidth > managerViewport.width && Math.abs(layout.controlsRight - (layout.viewportWidth - 8)) <= 1;
      }
    ));
    assert(
      Math.abs(maximizedTitlebarLayout.controlsRight - (maximizedTitlebarLayout.viewportWidth - 8)) <= 1,
      `Maximized titlebar controls are not pinned to the trailing edge: ${JSON.stringify(maximizedTitlebarLayout)}`
    );
    await evaluate(manager, `window.__TAURI_INTERNALS__.invoke('plugin:window|toggle_maximize', { label: 'main' })`);
    await waitForEvaluation(
      manager,
      `innerWidth < ${maximizedTitlebarLayout.viewportWidth}`,
      Boolean
    );

    const compact = JSON.parse(await evaluate(launcher, `JSON.stringify((() => {
      const button = document.querySelector('.launcher-trigger');
      const image = button.querySelector('img');
      const rect = button.getBoundingClientRect();
      const style = getComputedStyle(button);
      return { width: innerWidth, height: innerHeight, left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, display: style.display, visibility: style.visibility, opacity: style.opacity, background: style.background, scale: devicePixelRatio, clipPath: getComputedStyle(image).clipPath, imageLoaded: Boolean(image?.complete && image?.naturalWidth > 0) };
    })())`));
    assert(compact.display !== 'none' && compact.visibility === 'visible' && compact.opacity === '1', 'Compact launcher button is not visible.');
    assert(compact.left >= 0 && compact.top >= 0 && compact.right <= compact.width + 0.5 && compact.bottom <= compact.height + 0.5, `Compact launcher is outside its viewport: ${JSON.stringify(compact)}`);
    assert(compact.imageLoaded, 'Compact launcher icon image did not load.');
    const outline = JSON.parse(fs.readFileSync(path.join(projectRoot, 'src/assets/launcher/hexagon.json'), 'utf8'));
    const expectedClip = `polygon(${outline.map(([x, y]) => `${x}% ${y}%`).join(',')})`;
    assert(compact.clipPath.replaceAll(' ', '') === expectedClip.replaceAll(' ', ''), `Launcher clip differs from native geometry: ${compact.clipPath}`);
    assert(!compact.background.includes('linear-gradient'), 'Compact launcher unexpectedly restored the removed rectangular gradient background.');
    assert(!await evaluate(launcher, 'Boolean(document.querySelector(".launcher-live-indicator"))'), 'Zero-speed launcher unexpectedly shows a traffic capsule.');

    const nativeCompact = JSON.parse(await evaluate(launcher, `(async () => JSON.stringify({
      position: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_position', { label: 'launcher' }),
      clientPosition: await window.__TAURI_INTERNALS__.invoke('plugin:window|inner_position', { label: 'launcher' }),
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
    assert(Math.abs(nativeRegion.x - (nativeCompact.clientPosition.x - nativeCompact.position.x + compact.left * compact.scale)) <= 1,
      `Launcher native/client horizontal origins disagree: ${JSON.stringify({ nativeRegion, nativeCompact, compact })}`);
    assert(Math.abs(nativeRegion.y - (nativeCompact.clientPosition.y - nativeCompact.position.y + compact.top * compact.scale)) <= 1,
      `Launcher native/client vertical origins disagree: ${JSON.stringify({ nativeRegion, nativeCompact, compact })}`);

    if (process.argv.includes('--geometry-only')) {
      process.stdout.write(`Launcher geometry smoke passed: ${JSON.stringify({ scale: compact.scale, clipPath: compact.clipPath, nativeRegion, clientPosition: nativeCompact.clientPosition, outerPosition: nativeCompact.position })}\n`);
      return;
    }

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
      `(async () => JSON.stringify({ state: await window.__TAURI_INTERNALS__.invoke('launcher_interaction_snapshot'), events: window.__launcherSmokeEvents, compact: Boolean(document.querySelector('.launcher-trigger')), expanded: Boolean(document.querySelector('.launcher-panel')), menu: Boolean(document.querySelector('.launcher-context-menu')), menuText: document.querySelector('.launcher-context-menu')?.innerText ?? '', manager: await window.__TAURI_INTERNALS__.invoke('manager_window_snapshot') }))()`,
      (value) => {
        const state = JSON.parse(value).state;
        return state.dragRequests === 1 && state.menuRequests === 1 && JSON.parse(value).menu;
      }
    ));
    assert(launcherInteractions.state.dragRequests === 1, `Launcher drag did not invoke native window dragging: ${JSON.stringify(launcherInteractions)}`);
    assert(launcherInteractions.state.menuRequests === 1, `Launcher right click did not request its custom menu: ${JSON.stringify(launcherInteractions)}`);
    assert(launcherInteractions.compact && !launcherInteractions.expanded && launcherInteractions.menu, `Drag or right click unexpectedly expanded the launcher: ${JSON.stringify(launcherInteractions)}`);
    assert(!launcherInteractions.menuText.includes('本次隐藏悬浮球'), `Launcher menu still exposes temporary hiding: ${JSON.stringify(launcherInteractions)}`);
    assert(
      launcherInteractions.menuText.includes(launcherInteractions.manager.visible ? '隐藏主界面' : '显示主界面'),
      `Launcher menu did not reflect the actual main-window state: ${JSON.stringify(launcherInteractions)}`
    );
    const menuGeometry = JSON.parse(await evaluate(launcher, `JSON.stringify((() => {
      const menu = document.querySelector('.launcher-context-menu').getBoundingClientRect();
      return { width: innerWidth, height: innerHeight, left: menu.left, top: menu.top, right: menu.right, bottom: menu.bottom };
    })())`));
    assert(
      menuGeometry.width >= 304 && menuGeometry.height >= 392
        && menuGeometry.left >= 47 && menuGeometry.top >= 47
        && menuGeometry.right <= menuGeometry.width - 31
        && menuGeometry.bottom <= menuGeometry.height - 71,
      `Launcher context menu does not leave a shadow-safe viewport: ${JSON.stringify(menuGeometry)}`
    );

    await evaluate(launcher, `document.querySelector('.launcher-context-menu [aria-haspopup="menu"]').click()`);
    const visibilityMenu = JSON.parse(await waitForEvaluation(
      launcher,
      `JSON.stringify({ title: document.querySelector('.launcher-context-menu header b')?.textContent, options: Array.from(document.querySelectorAll('.launcher-context-menu [role="menuitemradio"]')).map((item) => item.textContent.trim()) })`,
      (value) => JSON.parse(value).options.length === 3
    ));
    assert(visibilityMenu.title === '悬浮球显示', `Launcher visibility submenu did not open: ${JSON.stringify(visibilityMenu)}`);
    assert(
      ['总是显示', '仅下载时显示', '关闭显示'].every((label) => visibilityMenu.options.some((option) => option.startsWith(label))),
      `Launcher visibility submenu options are incomplete: ${JSON.stringify(visibilityMenu)}`
    );
    await evaluate(launcher, `document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
    const menuCollapsed = await waitForEvaluation(
      launcher,
      '!document.querySelector(".launcher-context-menu") && innerWidth < 200 && innerHeight < 100',
      Boolean
    );
    assert(menuCollapsed, 'Launcher did not return to compact mode after closing its context menu.');

    const foregroundBeforeLifecycleChecks = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('foreground_window_handle_for_test')`);

    await evaluate(manager, `window.__TAURI_INTERNALS__.invoke('request_native_launcher_close_for_test')`);
    const launcherVisibleAfterNativeClose = await waitForEvaluation(
      manager,
      `window.__TAURI_INTERNALS__.invoke('launcher_visible_for_test')`,
      Boolean
    );
    const launcherStillAlive = await waitForEvaluation(
      launcher,
      `Boolean(document.querySelector('.launcher-shell'))`,
      Boolean
    );
    assert(
      launcherStillAlive && launcherVisibleAfterNativeClose && child.exitCode === null,
      `Native launcher close destroyed, hid, or exited the entry point: ${JSON.stringify({ launcherStillAlive, launcherVisibleAfterNativeClose, exitCode: child.exitCode })}.`
    );

    await evaluate(manager, `document.querySelector('button[aria-label="关闭"]').click()`);
    const hiddenAfterCustomClose = JSON.parse(await waitForEvaluation(
      launcher,
      `(async () => JSON.stringify(await window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')))()`,
      (value) => {
        const snapshot = JSON.parse(value);
        return snapshot.exists && !snapshot.visible;
      }
    ));
    assert(hiddenAfterCustomClose.exists && !hiddenAfterCustomClose.visible, 'Custom close destroyed or left the main window visible.');

    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', { action: 'launcher.hide_manager' })`);
    const shownAfterCustomClose = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')`);
    assert(shownAfterCustomClose.exists && shownAfterCustomClose.visible, 'Launcher could not reopen the custom-hidden manager window.');
    const foregroundAfterCustomReopen = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('foreground_window_handle_for_test')`);
    assert(foregroundAfterCustomReopen === foregroundBeforeLifecycleChecks, 'Custom-close reopen activated a foreground window.');

    const closeRequestsBeforeNativeClose = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('manager_close_request_count_for_test')`);
    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('request_native_manager_close_for_test')`);
    const closeRequestsAfterNativeClose = await waitForEvaluation(
      launcher,
      `window.__TAURI_INTERNALS__.invoke('manager_close_request_count_for_test')`,
      (count) => count === closeRequestsBeforeNativeClose + 1
    );
    const hiddenAfterNativeClose = JSON.parse(await waitForEvaluation(
      launcher,
      `(async () => JSON.stringify(await window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')))()`,
      (value) => {
        const snapshot = JSON.parse(value);
        return snapshot.exists && !snapshot.visible;
      }
    ));
    assert(hiddenAfterNativeClose.exists && !hiddenAfterNativeClose.visible, 'Native close destroyed or left the main window visible.');
    assert(closeRequestsAfterNativeClose === closeRequestsBeforeNativeClose + 1, 'Native close did not reach the lifecycle handler.');

    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', { action: 'launcher.hide_manager' })`);
    const shownAfterNativeClose = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')`);
    assert(shownAfterNativeClose.exists && shownAfterNativeClose.visible, 'Launcher could not reopen the native-close-hidden manager window.');
    const foregroundAfterNativeReopen = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('foreground_window_handle_for_test')`);
    assert(foregroundAfterNativeReopen === foregroundBeforeLifecycleChecks, 'Native-close reopen activated a foreground window.');

    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', { action: 'launcher.settings' })`);
    const settingsVisible = await waitForEvaluation(
      manager,
      `Boolean(document.querySelector('.settings-panel'))`,
      Boolean
    );
    assert(settingsVisible, 'Launcher settings action did not navigate the manager to Settings.');

    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', { action: 'launcher.new_download' })`);
    const newDownloadVisible = await waitForEvaluation(
      manager,
      `Boolean(document.querySelector('.new-download-dialog.modal[role="dialog"]'))`,
      Boolean
    );
    assert(newDownloadVisible, 'Launcher new-download action did not open the manager download dialog.');
    await evaluate(manager, `document.querySelector('button[aria-label="关闭新建下载"]').click()`);

    await evaluate(launcher, 'document.querySelector("button[aria-label=\\"打开下载输入\\"]").click()');
    const expanded = JSON.parse(await waitForEvaluation(
      launcher,
      'JSON.stringify({ width: innerWidth, height: innerHeight, panel: Boolean(document.querySelector(".launcher-panel")), action: document.querySelector(".launcher-panel .primary")?.textContent })',
      (value) => {
        const state = JSON.parse(value);
        return state.panel && state.width >= 400 && state.height >= 299;
      }
    ));
    assert(expanded.width >= 400 && expanded.height >= 299, `Launcher did not expand to the input panel: ${JSON.stringify(expanded)}`);
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

    const parsedTask = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('parse_download', { input: ${JSON.stringify(downloadFixture.url)} })`);
    assert(parsedTask && parsedTask.task && parsedTask.task.aria_gid, `Local fixture was not submitted to aria2: ${JSON.stringify(parsedTask)}`);

    const quitStartedAt = Date.now();
    const hiddenAtQuit = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', { action: 'launcher.quit' })`);
    const hideElapsedMs = Date.now() - quitStartedAt;
    assert(hideElapsedMs < 750, `Quit left application windows visible for ${hideElapsedMs}ms.`);
    assert(hiddenAtQuit && !hiddenAtQuit.managerVisible && !hiddenAtQuit.launcherVisible, `Quit did not hide every application window: ${JSON.stringify(hiddenAtQuit)}`);
    const exitCode = await waitForChildExit(child, 8000);
    assert(exitCode === 0, `Application quit returned exit code ${exitCode}.`);
    await waitForProcessesToStop(ownedSidecarPids, 2000);

    const sessionPath = path.join(testDataDir, 'aria2.session');
    assert(fs.existsSync(sessionPath), 'aria2 session was not written before application exit.');
    const session = fs.readFileSync(sessionPath, 'utf8');
    assert(session.includes(downloadFixture.url), `Saved aria2 session does not contain the active fixture URL: ${JSON.stringify(session)}`);
    assert(!session.includes('rpc-secret') && !session.includes('token:'), 'Saved aria2 session leaked RPC credentials.');
    completedQuitFlow = true;

    process.stdout.write(`Launcher background smoke passed: compact ${compact.width}x${compact.height}, native launcher close stayed alive, manager custom/native close reopened without activation, and launcher.quit hid every window in ${hideElapsedMs}ms before saving the active aria2 session and stopping owned sidecar ${ownedSidecarPids.join(', ')}.\n`);
  } finally {
    downloadFixture.server.closeAllConnections?.();
    await new Promise((resolve) => downloadFixture.server.close(resolve));
    if (!completedQuitFlow && child.exitCode === null) {
      child.kill();
      await new Promise((resolve) => {
        const fallback = setTimeout(resolve, 2000);
        child.once('exit', () => { clearTimeout(fallback); resolve(); });
      });
    }
    if (!completedQuitFlow) {
      for (const pid of ownedSidecarPids) {
        if (processExists(pid)) {
          try { process.kill(pid); } catch {}
        }
      }
    }
    // WebView2 tears down its profile handles shortly after the host exits.
    // This keeps the background test isolated without leaking locked temp data.
    await delay(2_000);
    fs.rmSync(testDataDir, { recursive: true, force: true, maxRetries: 8, retryDelay: 150 });
  }
}

run().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
