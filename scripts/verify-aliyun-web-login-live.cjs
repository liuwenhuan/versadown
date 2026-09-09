const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const WebSocket = require('ws');

const projectRoot = path.resolve(__dirname, '..');
const provider = process.env.ALLDOWNLOADER_TEST_PROVIDER || 'aliyun';
const providerLabels = {
  quark: '夸克网盘',
  pikpak: 'PikPak网盘',
  baidu: '百度网盘',
  tianyi: '天翼云盘',
  aliyun: '阿里云盘',
  xunlei: '迅雷云盘',
  uc: 'UC网盘',
};
const executable = process.env.ALLDOWNLOADER_TEST_BINARY || path.join(
  projectRoot,
  'artifacts',
  'AllDownloader-cloud-workspace-v16',
  'AllDownloader.exe',
);
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

if (!providerLabels[provider]) throw new Error(`不支持的测试网盘：${provider}`);

async function availablePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

async function evaluate(target, expression) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    socket.once('open', () => socket.send(JSON.stringify({
      id: 1,
      method: 'Runtime.evaluate',
      params: { expression, awaitPromise: true, returnByValue: true },
    })));
    socket.once('error', reject);
    socket.on('message', (data) => {
      const message = JSON.parse(data.toString());
      if (message.id !== 1) return;
      socket.close();
      if (message.error || message.result.exceptionDetails) {
        reject(new Error(JSON.stringify(message.error || message.result.exceptionDetails)));
      } else {
        resolve(message.result.result.value);
      }
    });
  });
}

async function waitForManager(port, child) {
  for (let attempt = 0; attempt < 90; attempt += 1) {
    if (child.exitCode !== null) throw new Error(`应用过早退出（${child.exitCode}）`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`);
      const targets = await response.json();
      for (const target of targets) {
        if (target.type !== 'page' || target.url === 'about:blank') continue;
        if (await evaluate(target, 'Boolean(document.querySelector(".app-shell"))')) return target;
      }
    } catch (_) {
      // WebView2 debugging port is still starting.
    }
    await delay(200);
  }
  throw new Error('隔离测试应用未在限定时间内就绪');
}

async function invoke(target, command, payload) {
  const expression = `(async () => {
    try {
      const result = await window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(payload)});
      return JSON.stringify({ ok: true, result });
    } catch (error) {
      return JSON.stringify({
        ok: false,
        error: typeof error === 'string'
          ? error
          : { code: error?.code, message: error?.message, name: error?.name },
      });
    }
  })()`;
  return JSON.parse(await evaluate(target, expression));
}

async function authorizationPageState(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`);
  const targets = await response.json();
  const page = targets.find((target) => {
    if (target.type !== 'page') return false;
    try {
      const hostname = new URL(target.url).hostname;
      return hostname !== 'tauri.localhost';
    } catch (_) {
      return false;
    }
  });
  if (!page) return { open: false, sessionReady: false };

  const viewport = await evaluate(page, `(() => {
    const root = document.scrollingElement || document.documentElement || document.body;
    return {
      x: Math.round(window.scrollX),
      y: Math.round(window.scrollY),
      maxX: Math.max(0, Math.round(root.scrollWidth - window.innerWidth)),
    };
  })()`);

  // This runs in the official authorization page.  It deliberately returns
  // only booleans, HTTP status and response field names: credentials and
  // personal account values never cross the test boundary.
  return await evaluate(page, `(async () => {
    const fields = {
      access_token: ['access_token', 'accessToken'],
      refresh_token: ['refresh_token', 'refreshToken'],
      default_drive_id: ['default_drive_id', 'defaultDriveId', 'drive_id', 'driveId'],
    };
    const found = {}; const values = {}; const seen = new Set();
    function inspect(value, depth) {
      if (!value || depth > 6) return;
      if (typeof value === 'string') { try { inspect(JSON.parse(value), depth + 1); } catch (_) {} return; }
      if (typeof value !== 'object' || seen.has(value)) return;
      seen.add(value);
      for (const [key, entry] of Object.entries(value)) {
        const target = Object.entries(fields).find(([, names]) => names.includes(key));
        if (target && typeof entry === 'string' && entry) { found[target[0]] = true; values[target[0]] = entry; }
        else if (!/password/i.test(key)) inspect(entry, depth + 1);
      }
    }
    for (const storage of [localStorage, sessionStorage]) {
      for (let index = 0; index < storage.length; index += 1) {
        inspect({ [storage.key(index)]: storage.getItem(storage.key(index)) }, 0);
      }
    }
    const sessionReady = Boolean(found.access_token && found.refresh_token && found.default_drive_id);
    const result = { open: true, sessionReady, fields: Object.keys(found).sort() };
    if (!sessionReady) return result;
    try {
      const paths = ['/adrive/v2/user/get', '/v2/user/get'];
      result.userProbe = [];
      for (const path of paths) {
        const response = await fetch('https://api.aliyundrive.com' + path, {
          method: 'POST',
          headers: {
            Authorization: 'Bearer ' + values.access_token,
            'Content-Type': 'application/json',
            Referer: 'https://www.aliyundrive.com/',
          },
          body: '{}',
        });
        const body = await response.json().catch(() => null);
        const type = (value) => value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
        const present = (container, keys) => Object.fromEntries(keys.map((key) => [key, type(container?.[key])]));
        const creator = body && typeof body === 'object' && body.creator && typeof body.creator === 'object'
          ? body.creator
          : null;
        result.userProbe.push({
          path,
          status: response.status,
          accountShape: {
            topLevelId: present(body, ['id', 'user_id', 'userId', 'uid']),
            topLevelName: present(body, ['nick_name', 'name', 'user_name', 'phone', 'display_name']),
            creatorKeys: creator ? Object.keys(creator).sort() : [],
            creatorId: present(creator, ['id', 'user_id', 'userId', 'uid']),
            creatorName: present(creator, ['nick_name', 'name', 'user_name', 'phone', 'display_name']),
          },
        });
      }
    } catch (error) {
      result.userProbe = { networkError: String(error?.name || 'fetch_failed') };
    }
    return result;
  })()`).then((session) => ({ ...session, viewport }));
}

async function hasGoogleOAuthPopup(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`);
  const targets = await response.json();
  return targets.some((target) => {
    if (target.type !== 'page') return false;
    try {
      const hostname = new URL(target.url).hostname;
      return hostname === 'accounts.google.com' || hostname.endsWith('.accounts.google.com');
    } catch (_) {
      return false;
    }
  });
}

function safeError(error) {
  if (typeof error === 'string') return error.slice(0, 300);
  if (error && typeof error === 'object') {
    return String(error.message || error.code || JSON.stringify(error)).slice(0, 300);
  }
  return String(error).slice(0, 300);
}

function isAuthorizationPending(result) {
  const code = typeof result?.error === 'object' ? result.error?.code : undefined;
  return code === 'authorization_pending' || String(result?.error || '').includes('authorization_pending');
}

async function waitForExit(child) {
  if (child.exitCode !== null) return child.exitCode;
  return await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('隔离测试应用未能退出')), 8000);
    child.once('exit', (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
}

async function closeApplication(port, child) {
  if (child.exitCode !== null) return;
  try {
    const response = await fetch(`http://127.0.0.1:${port}/json/list`);
    const targets = await response.json();
    const page = targets.find((target) => target.type === 'page' && target.url !== 'about:blank');
    if (page) await invoke(page, 'activate_launcher_menu_for_test', { action: 'launcher.quit' });
  } catch (_) {
    child.kill();
  }
  await waitForExit(child).catch(() => child.kill());
}

async function removeTemporaryDirectory(directory) {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    try {
      fs.rmSync(directory, { recursive: true, force: true, maxRetries: 2, retryDelay: 150 });
      return;
    } catch (_) {
      await delay(500);
    }
  }
  process.stderr.write(`隔离测试数据清理失败：${directory}\n`);
}

async function run() {
  if (!fs.existsSync(executable)) throw new Error(`未找到发布二进制：${executable}`);
  const viewportOnly = process.env.ALLDOWNLOADER_VERIFY_AUTH_VIEWPORT_ONLY === '1';
  const popupOnly = process.env.ALLDOWNLOADER_VERIFY_OAUTH_POPUP_ONLY === '1';
  if (!viewportOnly && !popupOnly && provider !== 'aliyun') {
    throw new Error('完整网页登录验证当前仅支持 aliyun；其他网盘请设置视口或 OAuth 弹窗测试模式');
  }
  if (popupOnly && provider !== 'pikpak') {
    throw new Error('OAuth 弹窗测试当前仅支持 pikpak');
  }

  const dataDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'alldownloader-aliyun-live-'));
  const port = await availablePort();
  const child = spawn(executable, [], {
    cwd: path.dirname(executable),
    windowsHide: true,
    stdio: 'ignore',
    env: {
      ...process.env,
      ALLDOWNLOADER_BACKGROUND_UI_TEST: '1',
      ALLDOWNLOADER_TEST_DATA_DIR: dataDirectory,
      ALLDOWNLOADER_TEST_DOWNLOAD_DIR: path.join(dataDirectory, 'downloads'),
      WEBVIEW2_USER_DATA_FOLDER: path.join(dataDirectory, 'webview2'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
    },
  });

  try {
    const manager = await waitForManager(port, child);
    const begin = await invoke(manager, 'cloud_begin_authorization', { provider });
    if (!begin.ok) throw new Error(`${providerLabels[provider]}授权窗口无法打开：${safeError(begin.error)}`);

    process.stdout.write(popupOnly
      ? `${providerLabels[provider]}官方授权窗口已打开。请点击“通过 Google 登录”；后台将确认 Google OAuth 窗口出现。\n`
      : `${providerLabels[provider]}官方授权窗口已打开。请在该窗口完成登录；后台将自动验证账号与根目录读取。\n`);
    const deadline = Date.now() + (10 * 60 * 1000);
    let sessionDetected = false;
    while (Date.now() < deadline) {
      if (child.exitCode !== null) throw new Error(`应用在授权完成前退出（${child.exitCode}）`);
      const authorization = await authorizationPageState(port).catch(() => ({ open: false, sessionReady: false }));
      if (popupOnly) {
        if (await hasGoogleOAuthPopup(port).catch(() => false)) {
          process.stdout.write('PikPak Google OAuth 弹窗验证通过：accounts.google.com 已在独立窗口打开。\n');
          return;
        }
        await delay(500);
        continue;
      }
      if (viewportOnly && authorization.open && authorization.viewport) {
        const { x, y, maxX } = authorization.viewport;
        if (y === 0 && Math.abs(x - maxX) <= 1) {
          process.stdout.write(`${providerLabels[provider]}授权页初始视口验证通过：x=${x} / maxX=${maxX}，y=${y}。\n`);
          return;
        }
      }
      if (!authorization.sessionReady) {
        await delay(1500);
        continue;
      }
      if (!sessionDetected) {
        sessionDetected = true;
        process.stdout.write(`已检测到阿里网页登录会话字段（${authorization.fields.join(', ')}），开始后台验证。\n`);
      }
      const account = await invoke(manager, 'cloud_complete_authorization', { provider: 'aliyun' });
      if (account.ok) {
        const directory = await invoke(manager, 'cloud_list_directory', { provider: 'aliyun', parentId: 'root' });
        if (!directory.ok) throw new Error(`阿里根目录读取失败：${safeError(directory.error)}`);
        const itemCount = Array.isArray(directory.result?.items) ? directory.result.items.length : 0;
        process.stdout.write(`阿里云盘真实网页登录验证通过：账号已确认，根目录已读取（${itemCount} 项）。\n`);
        return;
      }
      if (!isAuthorizationPending(account)) {
        const probe = authorization.userProbe ? `；官方页面账号探测：${JSON.stringify(authorization.userProbe)}` : '';
        throw new Error(`阿里授权完成校验失败：${safeError(account.error)}${probe}`);
      }
      await delay(1500);
    }
    throw new Error('等待阿里网页登录超时（10 分钟）');
  } finally {
    await closeApplication(port, child);
    await removeTemporaryDirectory(dataDirectory);
  }
}

run().catch((error) => {
  process.stderr.write(`阿里云盘真实网页登录验证失败：${safeError(error)}\n`);
  process.exitCode = 1;
});
