const { spawn } = require('node:child_process');
const assert = require('node:assert/strict');
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
// DOMRect values from fractional Windows DPI can differ by a few float ULPs.
const geometryEpsilon = 0.01;

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
  const result = await devtoolsCommand(target, 'Runtime.evaluate', {
    expression, awaitPromise: true, returnByValue: true
  });
  if (result.exceptionDetails) {
    const details = result.exceptionDetails;
    const description = details.exception?.description ?? details.exception?.value ?? JSON.stringify(details);
    throw new Error(`${details.text}: ${description}`);
  }
  return result.result.value;
}

async function devtoolsCommand(target, method, params = {}) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error(`DevTools ${method} timed out.`));
    }, 10_000);
    socket.on('open', () => socket.send(JSON.stringify({ id: 1, method, params })));
    socket.on('error', (error) => { clearTimeout(timeout); reject(error); });
    socket.on('message', (data) => {
      const message = JSON.parse(data.toString());
      if (message.id !== 1) return;
      clearTimeout(timeout);
      socket.close();
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else resolve(message.result);
    });
  });
}

async function captureLauncherIcon(target) {
  const { data } = await devtoolsCommand(target, 'Page.captureScreenshot', {
    format: 'png', fromSurface: true,
    clip: { x: 346, y: 328, width: 64, height: 64, scale: 1 }
  });
  return Buffer.from(data, 'base64');
}

async function launcherBounds(target) {
  return evaluate(target, `(async () => ({
    position: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_position', { label: 'launcher' }),
    size: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_size', { label: 'launcher' })
  }))()`);
}

async function launcherSnapshot(target) {
  return evaluate(target, `window.__TAURI_INTERNALS__.invoke('launcher_interaction_snapshot')`);
}

async function assertSvgOutline(target) {
  // This retained outline fixture describes the original native hexagon. The
  // fixed canvas no longer applies a native visual region or clips its WebView.
  const outline = JSON.parse(fs.readFileSync(path.join(projectRoot, 'src/assets/launcher/hexagon.json'), 'utf8'));
  const result = await evaluate(target, `(() => {
    const outline = ${JSON.stringify(outline)};
    const svg = document.querySelector('svg.launcher-mark');
    const shell = svg.querySelector('.launcher-mark-shell');
    const box = svg.viewBox.baseVal;
    const edges = outline.map((a, i) => [a, outline[(i + 1) % outline.length]]);
    const distance = ([x, y], [[ax, ay], [bx, by]]) => {
      const dx = bx - ax, dy = by - ay;
      const t = Math.max(0, Math.min(1, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)));
      return Math.hypot(x - ax - t * dx, y - ay - t * dy);
    };
    const perimeter = edges.reduce((sum, [[ax, ay], [bx, by]]) => sum + Math.hypot(bx - ax, by - ay), 0);
    const length = shell.getTotalLength();
    let maxBoundaryError = 0;
    for (let i = 0; i <= 1024; i += 1) {
      const p = shell.getPointAtLength(length * i / 1024);
      const normalized = [(p.x - box.x) / box.width * 100, (p.y - box.y) / box.height * 100];
      maxBoundaryError = Math.max(maxBoundaryError, Math.min(...edges.map((edge) => distance(normalized, edge))));
    }
    let fillSamples = 0;
    const mismatches = [];
    for (let y = 0.5; y < 100; y += 1) for (let x = 0.5; x < 100; x += 1) {
      if (edges.some((edge) => distance([x, y], edge) < 0.03)) continue;
      let expected = false;
      for (const [[ax, ay], [bx, by]] of edges) {
        if ((ay > y) !== (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax) expected = !expected;
      }
      const actual = shell.isPointInFill(new DOMPoint(box.x + x * box.width / 100, box.y + y * box.height / 100));
      fillSamples += 1;
      if (actual !== expected && mismatches.length < 10) mismatches.push({ x, y, expected, actual });
    }
    return { maxBoundaryError, fillSamples, mismatches, perimeterError: Math.abs(length / box.width * 100 - perimeter) };
  })()`);
  assert(result.fillSamples > 9900 && result.mismatches.length === 0,
    `SVG fill differs from the native hexagon outline: ${JSON.stringify(result)}`);
  assert(result.maxBoundaryError < 0.02 && result.perimeterError < 0.02,
    `SVG boundary differs from the native hexagon outline: ${JSON.stringify(result)}`);
  return result;
}

async function runPopupCycles(target) {
  const closed = '!document.querySelector(".launcher-panel, .launcher-context-menu")';
  await waitForEvaluation(target, closed, Boolean);
  const before = await launcherBounds(target);
  const beforeSnapshot = await launcherSnapshot(target);
  const iconBefore = await captureLauncherIcon(target);
  // Equal blank captures would not establish icon stability. Check two solid
  // pixels against the SVG's current theme colors before comparing PNG bytes.
  const iconPixels = await evaluate(target, `(async () => {
    const image = new Image();
    image.src = ${JSON.stringify(`data:image/png;base64,${iconBefore.toString('base64')}`)};
    await image.decode();
    const canvas = document.createElement('canvas');
    canvas.width = image.naturalWidth;
    canvas.height = image.naturalHeight;
    const context = canvas.getContext('2d');
    context.drawImage(image, 0, 0);
    return [['shell', 15], ['chevron', 34]].map(([part, y]) => ({
      part,
      actual: Array.from(context.getImageData(Math.floor(canvas.width / 2), Math.floor(y * canvas.height / 64), 1, 1).data),
      expected: [...getComputedStyle(document.querySelector('.launcher-mark-' + part)).fill.match(/\\d+/g).map(Number).slice(0, 3), 255]
    }));
  })()`);
  for (const pixel of iconPixels) assert.deepEqual(pixel.actual, pixel.expected, `Icon screenshot did not capture its ${pixel.part} pixels.`);
  let lastSnapshot = beforeSnapshot;
  for (let cycle = 0; cycle < 50; cycle += 1) {
    const quickAdd = cycle % 2 === 0;
    const selector = quickAdd ? '.launcher-panel' : '.launcher-context-menu';
    await evaluate(target, quickAdd
      ? `document.querySelector('.launcher-trigger').click()`
      : `document.querySelector('.launcher-trigger').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, button: 2 }))`);
    const popup = await waitForEvaluation(target, `(() => {
      const popup = document.querySelector('${selector}');
      const rect = popup?.getBoundingClientRect();
      return { width: innerWidth, height: innerHeight,
        count: document.querySelectorAll('.launcher-panel, .launcher-context-menu').length,
        rect: rect && { x: rect.x, y: rect.y, width: rect.width, height: rect.height } };
    })()`, (value) => Boolean(value.rect));
    assert.equal(popup.count, 1, `Cycle ${cycle}: more than one popup is visible.`);
    assert.deepEqual([popup.width, popup.height], [410, 720], `Cycle ${cycle}: viewport changed.`);
    assert(popup.rect.x >= -geometryEpsilon && popup.rect.x + popup.rect.width <= 410 + geometryEpsilon
      && popup.rect.y >= -geometryEpsilon && popup.rect.y + popup.rect.height <= 720 + geometryEpsilon
      && (popup.rect.y + popup.rect.height <= 328 + geometryEpsilon || popup.rect.y >= 392 - geometryEpsilon),
    `Cycle ${cycle}: popup escapes its slot: ${JSON.stringify(popup.rect)}`);
    await waitForEvaluation(target, `window.__TAURI_INTERNALS__.invoke('launcher_interaction_snapshot')`,
      (value) => value.inputRegionUpdateRequests > lastSnapshot.inputRegionUpdateRequests);
    const points = [[378, 360], [Math.round(popup.rect.x + popup.rect.width / 2), Math.round(popup.rect.y + popup.rect.height / 2)]];
    await waitForEvaluation(target,
      `window.__TAURI_INTERNALS__.invoke('launcher_input_hit_test_for_test', { points: ${JSON.stringify(points)} })`,
      (hits) => hits.some((item) => item.sameThread)
        && hits.filter((item) => item.sameThread).every((item) => item.installed && item.hits.every((hit) => hit === 1)));
    assert.deepEqual(await launcherBounds(target), before, `Cycle ${cycle}: popup changed native bounds.`);
    if (quickAdd) {
      // Exercise the mounted input and verify focus without submitting a download.
      const inputWorked = await evaluate(target, `(() => {
        const input = document.querySelector('.launcher-panel textarea');
        input.focus();
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(input, 'cycle-${cycle}');
        input.dispatchEvent(new Event('input', { bubbles: true }));
        return document.activeElement === input && input.value === 'cycle-${cycle}';
      })()`);
      assert(inputWorked, `Cycle ${cycle}: quick-add input did not accept focus/text.`);
    }
    const openSnapshot = await launcherSnapshot(target);
    await evaluate(target, `document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
    await waitForEvaluation(target, closed, Boolean);
    lastSnapshot = await waitForEvaluation(target, `window.__TAURI_INTERNALS__.invoke('launcher_interaction_snapshot')`,
      (value) => value.inputRegionUpdateRequests > openSnapshot.inputRegionUpdateRequests);
    await waitForEvaluation(target,
      `window.__TAURI_INTERNALS__.invoke('launcher_input_hit_test_for_test', { points: ${JSON.stringify(points)} })`,
      (hits) => hits.some((item) => item.sameThread)
        && hits.filter((item) => item.sameThread).every((item) => item.installed && item.hits[0] === 1 && item.hits[1] === -1));
    assert.deepEqual(await launcherBounds(target), before, `Cycle ${cycle}: closing changed native bounds.`);
  }
  const after = await launcherBounds(target);
  const snapshot = await launcherSnapshot(target);
  const iconAfter = await captureLauncherIcon(target);
  assert.deepEqual(after.position, before.position);
  assert.deepEqual(after.size, before.size);
  assert.equal(snapshot.boundsUpdateRequests, 0);
  assert(snapshot.menuRequests - beforeSnapshot.menuRequests >= 25);
  assert(snapshot.inputRegionUpdateRequests - beforeSnapshot.inputRegionUpdateRequests >= 100);
  assert(iconBefore.equals(iconAfter), 'The 64x64 icon PNG changed across popup cycles.');
  return { before, after, snapshot, menuRequests: snapshot.menuRequests - beforeSnapshot.menuRequests,
    inputRegionUpdates: snapshot.inputRegionUpdateRequests - beforeSnapshot.inputRegionUpdateRequests,
    iconPngBytes: iconBefore.length, iconPixels, svgOutline: await assertSvgOutline(target) };
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
      const mark = button.querySelector('svg.launcher-mark');
      const shell = mark?.querySelector('.launcher-mark-shell');
      const markBounds = shell?.getBBox();
      const rect = button.getBoundingClientRect();
      const style = getComputedStyle(button);
      return { width: innerWidth, height: innerHeight, left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, display: style.display, visibility: style.visibility, opacity: style.opacity, background: style.background, scale: devicePixelRatio, markBounds: markBounds && { x: markBounds.x, y: markBounds.y, width: markBounds.width, height: markBounds.height }, markViewBox: mark && { width: mark.viewBox.baseVal.width, height: mark.viewBox.baseVal.height }, iconRendered: Boolean(shell && mark.querySelector('.launcher-mark-chevron') && shell.isPointInFill(new DOMPoint(64, 64)) && !shell.isPointInFill(new DOMPoint(0, 0)) && getComputedStyle(mark).display !== 'none') };
    })())`));
    assert(compact.display !== 'none' && compact.visibility === 'visible' && compact.opacity === '1', 'Compact launcher button is not visible.');
    assert.equal(compact.width, 410);
    assert.equal(compact.height, 720);
    assert.deepEqual([compact.left, compact.top, compact.right, compact.bottom], [346, 328, 410, 392]);
    assert(compact.iconRendered, 'Compact launcher SVG icon did not render.');
    const svgOutline = await assertSvgOutline(launcher);
    assert(!compact.background.includes('linear-gradient'), 'Compact launcher unexpectedly restored the removed rectangular gradient background.');
    assert(!await evaluate(launcher, 'Boolean(document.querySelector(".launcher-live-indicator"))'), 'Zero-speed launcher unexpectedly shows a traffic capsule.');

    const nativeCompact = JSON.parse(await evaluate(launcher, `(async () => JSON.stringify({
      position: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_position', { label: 'launcher' }),
      clientPosition: await window.__TAURI_INTERNALS__.invoke('plugin:window|inner_position', { label: 'launcher' }),
      size: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_size', { label: 'launcher' }),
      monitor: await window.__TAURI_INTERNALS__.invoke('plugin:window|current_monitor')
    }))()`));
    const workArea = nativeCompact.monitor.workArea;
    const iconScreen = {
      left: nativeCompact.clientPosition.x + Math.round(346 * compact.scale),
      top: nativeCompact.clientPosition.y + Math.round(328 * compact.scale),
      right: nativeCompact.clientPosition.x + Math.round(410 * compact.scale),
      bottom: nativeCompact.clientPosition.y + Math.round(392 * compact.scale)
    };
    assert(iconScreen.left >= workArea.position.x && iconScreen.top >= workArea.position.y
      && iconScreen.right <= workArea.position.x + workArea.size.width
      && iconScreen.bottom <= workArea.position.y + workArea.size.height,
    `Launcher icon extends outside the work area: ${JSON.stringify({ iconScreen, workArea })}`);
    assert.deepEqual(nativeCompact.size, {
      width: Math.round(410 * compact.scale), height: Math.round(720 * compact.scale)
    });

    // This is command-path + native message coverage, not OS mouse routing.
    const beforeInputUpdate = await launcherSnapshot(launcher);
    await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('set_launcher_input_regions', {
      regions: [{ x: 346, y: 328, width: 64, height: 64 }]
    })`);
    const afterInputUpdate = await launcherSnapshot(launcher);
    assert.equal(afterInputUpdate.inputRegionUpdateRequests, beforeInputUpdate.inputRegionUpdateRequests + 1,
      'A successfully applied native input-region update must be counted.');
    assert.equal(afterInputUpdate.boundsUpdateRequests, 0, 'Fixed-canvas bounds sentinel must remain zero.');
    const hitPoints = [[378, 360], [20, 20], [400, 20], [20, 650], [400, 650]];
    const nativeHits = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('launcher_input_hit_test_for_test', { points: ${JSON.stringify(hitPoints)} })`);
    assert(nativeHits.length > 1, 'Native hit-test smoke did not enumerate the WebView2 child HWNDs.');
    for (const target of nativeHits.filter((item) => item.sameThread)) {
      assert(target.installed, `Same-thread launcher HWND has no input subclass: ${JSON.stringify(target)}`);
      assert(JSON.stringify(target.hits) === JSON.stringify([1, -1, -1, -1, -1]), `Unexpected native WM_NCHITTEST results: ${JSON.stringify(target)}`);
    }
    const invalidUpdateRejected = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('set_launcher_input_regions', {
      regions: [{ x: 0, y: 0, width: -1, height: 64 }]
    }).then(() => false, () => true)`);
    assert(invalidUpdateRejected, 'Invalid input-region update was accepted.');
    const hitsAfterInvalidUpdate = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('launcher_input_hit_test_for_test', { points: ${JSON.stringify(hitPoints)} })`);
    assert(JSON.stringify(hitsAfterInvalidUpdate) === JSON.stringify(nativeHits), 'Invalid input-region update changed native hit results.');
    const managerUpdateRejected = await evaluate(manager, `window.__TAURI_INTERNALS__.invoke('set_launcher_input_regions', {
      regions: [{ x: 0, y: 0, width: 64, height: 64 }]
    }).then(() => false, () => true)`);
    assert(managerUpdateRejected, 'Manager window was allowed to change launcher input regions.');
    assert.equal((await launcherSnapshot(launcher)).inputRegionUpdateRequests, afterInputUpdate.inputRegionUpdateRequests,
      'Rejected invalid/caller input-region updates must not increment diagnostics.');
    const boundsAfterInput = await evaluate(launcher, `(async () => ({
      position: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_position', { label: 'launcher' }),
      size: await window.__TAURI_INTERNALS__.invoke('plugin:window|outer_size', { label: 'launcher' })
    }))()`);
    assert(JSON.stringify(boundsAfterInput.position) === JSON.stringify(nativeCompact.position)
      && JSON.stringify(boundsAfterInput.size) === JSON.stringify(nativeCompact.size), 'Input-region update changed native launcher bounds.');
    process.stdout.write(`Launcher input command/native-message smoke passed: ${JSON.stringify(nativeHits)}. DOM events and SendMessage(WM_NCHITTEST) do not prove OS mouse click-through; visible four-quadrant/background-window validation remains required.\n`);

    if (process.argv.includes('--geometry-only')) {
      process.stdout.write(`Launcher geometry smoke passed: ${JSON.stringify({ scale: compact.scale, svgOutline, iconScreen, size: nativeCompact.size })}\n`);
      return;
    }

    await evaluate(launcher, `(() => {
      window.__launcherSmokeEvents = [];
      const button = document.querySelector('.launcher-trigger');
      for (const type of ['mousedown', 'mousemove', 'mouseup', 'click', 'contextmenu']) {
        button.addEventListener(type, (event) => window.__launcherSmokeEvents.push({ type, button: event.button, buttons: event.buttons, clientX: event.clientX, clientY: event.clientY }), { capture: true, once: true });
      }
      button.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, clientX: 378, clientY: 360 }));
      button.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, button: 0, buttons: 1, clientX: 388, clientY: 368 }));
      button.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, buttons: 0, clientX: 388, clientY: 368 }));
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
      menuGeometry.width === 410 && menuGeometry.height === 720
        && menuGeometry.left >= 16 - geometryEpsilon && menuGeometry.top >= 16 - geometryEpsilon
        && menuGeometry.right <= 394 + geometryEpsilon && menuGeometry.bottom <= 704 + geometryEpsilon
        && (menuGeometry.bottom <= 312 + geometryEpsilon || menuGeometry.top >= 408 - geometryEpsilon),
      `Launcher context menu must fit its fixed popup slot with a 16px shadow gutter: ${JSON.stringify(menuGeometry)}`
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
      '!document.querySelector(".launcher-context-menu") && innerWidth === 410 && innerHeight === 720',
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
        return state.panel;
      }
    ));
    assert(expanded.width === 410 && expanded.height === 720, `Launcher quick-add changed the fixed viewport: ${JSON.stringify(expanded)}`);
    assert(expanded.action === '开始解析下载', 'Expanded launcher action label is incorrect.');

    await evaluate(launcher, 'document.querySelector("button[aria-label=\\"收起下载输入\\"]").click()');
    const collapsed = await waitForEvaluation(
      launcher,
      'Boolean(document.querySelector(".launcher-trigger")) && !document.querySelector(".launcher-panel") && innerWidth === 410 && innerHeight === 720',
      Boolean
    );
    assert(collapsed, 'Launcher did not collapse back to the floating button.');

    const cycles = await runPopupCycles(launcher);
    process.stdout.write(`Launcher 50-cycle smoke passed: ${JSON.stringify(cycles)}; native position/size and icon PNG bytes unchanged.\n`);

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

    const sessionPath = path.join(testDataDir, 'aria2-next.tasks');
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
