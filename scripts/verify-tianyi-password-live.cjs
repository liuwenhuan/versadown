const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const WebSocket = require('ws');

const projectRoot = path.resolve(__dirname, '..');
const executable = process.env.ALLDOWNLOADER_LIVE_EXECUTABLE
  || path.join(projectRoot, 'artifacts', 'alldownloader.exe');
const savedSessionOnly = process.env.ALLDOWNLOADER_TIANYI_SAVED_SESSION_ONLY === '1';
const shapeOnly = process.env.ALLDOWNLOADER_TIANYI_SHAPE_ONLY === '1';
const manualAuthorization = process.env.ALLDOWNLOADER_TIANYI_MANUAL_AUTH === '1';
const sourceDataDirectory = process.env.ALLDOWNLOADER_LIVE_DATA_DIR
  || path.join(process.env.APPDATA || '', 'com.alldownloader.desktop');
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

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

async function targets(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`);
  return await response.json();
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
      if (message.error || message.result.exceptionDetails) reject(new Error('页面诊断执行失败'));
      else resolve(message.result.result.value);
    });
  });
}

async function command(target, method, params = {}) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    socket.once('open', () => socket.send(JSON.stringify({ id: 1, method, params })));
    socket.once('error', reject);
    socket.on('message', (data) => {
      const message = JSON.parse(data.toString());
      if (message.id !== 1) return;
      socket.close();
      if (message.error) reject(new Error('浏览器后台诊断命令失败'));
      else resolve(message.result);
    });
  });
}

async function captureOfficialAccountRequestShape(target) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    let requestId = '';
    let requestHeaderNames = [];
    let wireHeaderNames = [];
    let cookieOrder = [];
    let associatedCookies = [];
    let status = 0;
    let runtimeFinished = false;
    let timer;
    const finish = () => {
      if (!runtimeFinished || !requestId) return;
      clearTimeout(timer);
      socket.close();
      resolve({ requestHeaderNames, wireHeaderNames, cookieOrder, associatedCookies, status });
    };
    socket.once('error', reject);
    socket.once('open', () => socket.send(JSON.stringify({ id: 1, method: 'Network.enable', params: {} })));
    socket.on('message', (data) => {
      const message = JSON.parse(data.toString());
      if (message.id === 1) {
        socket.send(JSON.stringify({
          id: 2,
          method: 'Runtime.evaluate',
          params: {
            expression: `(async () => {
              const context = window.__alldownloaderTianyiContext;
              const response = await fetch('/api/open/user/getUserInfoForPortal.action?diagnostic=' + Date.now(), {
                method: 'GET', credentials: 'include', cache: 'no-store',
                headers: { Accept: 'application/json;charset=UTF-8', 'Browser-Id': String(context?.browser_id || '') },
              });
              await response.arrayBuffer();
              return response.status;
            })()`,
            awaitPromise: true,
            returnByValue: true,
          },
        }));
        timer = setTimeout(() => reject(new Error('官网请求形状捕获超时')), 10000);
        return;
      }
      if (message.method === 'Network.requestWillBeSent'
          && String(message.params?.request?.url || '').includes('/api/open/user/getUserInfoForPortal.action')) {
        requestId = message.params.requestId;
        requestHeaderNames = Object.keys(message.params.request.headers || {}).sort();
      }
      if (message.method === 'Network.requestWillBeSentExtraInfo' && message.params?.requestId === requestId) {
        const headers = message.params.headers || {};
        wireHeaderNames = Object.keys(headers).sort();
        const cookieHeader = headers.Cookie || headers.cookie || '';
        cookieOrder = String(cookieHeader).split(';').map((pair) => pair.split('=', 1)[0].trim())
          .filter((name) => /^[A-Za-z0-9!#$%&'*+.^_\x60|~-]{1,64}$/.test(name));
        associatedCookies = (message.params.associatedCookies || []).map((entry) => ({
          name: /^[A-Za-z0-9!#$%&'*+.^_\x60|~-]{1,64}$/.test(entry.cookie?.name || '') ? entry.cookie.name : 'Unknown',
          blockedReasons: (entry.blockedReasons || []).map(String).sort(),
        }));
      }
      if (message.method === 'Network.responseReceived' && message.params?.requestId === requestId) {
        status = Number(message.params.response?.status || 0);
      }
      if (message.id === 2) {
        runtimeFinished = true;
        setTimeout(finish, 250);
      }
    });
  });
}

async function probeNativeVariants(page, cookieResult, cookieOrder) {
  const context = await evaluate(page, `(() => ({
    browserId: String(window.__alldownloaderTianyiContext?.browser_id || ''),
    userAgent: String(navigator.userAgent || ''),
    brands: Array.isArray(navigator.userAgentData?.brands) ? navigator.userAgentData.brands : [],
    mobile: Boolean(navigator.userAgentData?.mobile),
    platform: String(navigator.userAgentData?.platform || ''),
  }))()`);
  const cookies = new Map((cookieResult?.cookies || []).map((cookie) => [cookie.name, cookie.value]));
  const validNames = [...cookies.keys()].filter((name) => /^[A-Za-z0-9!#$%&'*+.^_\x60|~-]{1,64}$/.test(name));
  const officialNames = cookieOrder.filter((name) => cookies.has(name));
  const cookieHeader = (names) => names.map((name) => `${name}=${cookies.get(name)}`).join('; ');
  const brands = (context.brands || []).map((brand) => `"${String(brand.brand).replace(/["\\]/g, '')}";v="${String(brand.version).replace(/["\\]/g, '')}"`).join(', ');
  const base = {
    Accept: 'application/json;charset=UTF-8',
    Cookie: cookieHeader(validNames.sort()),
    Origin: 'https://cloud.189.cn',
    Referer: 'https://cloud.189.cn/',
    'User-Agent': context.userAgent,
    'Browser-Id': context.browserId,
  };
  const browserLike = {
      ...base,
      Cookie: cookieHeader(officialNames),
      Origin: undefined,
      'sec-ch-ua': brands,
      'sec-ch-ua-mobile': context.mobile ? '?1' : '?0',
      'sec-ch-ua-platform': `"${context.platform.replace(/["\\]/g, '')}"`,
  };
  const withoutJsession = officialNames.filter((name) => name !== 'JSESSIONID');
  const durableOnly = ['COOKIE_LOGIN_USER', 'SSON'].filter((name) => cookies.has(name));
  const withoutApm = officialNames.filter((name) => !name.startsWith('apm_'));
  const variants = [
    ['account-current', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', base],
    ['account-official-cookie-order', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', { ...base, Cookie: cookieHeader(officialNames) }],
    ['account-without-origin', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', { ...base, Cookie: cookieHeader(officialNames), Origin: undefined }],
    ['account-browser-like', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', browserLike],
    ['account-without-jsession', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', { ...browserLike, Cookie: cookieHeader(withoutJsession) }],
    ['account-without-apm', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', { ...browserLike, Cookie: cookieHeader(withoutApm) }],
    ['account-durable-only', 'https://cloud.189.cn/api/open/user/getUserInfoForPortal.action', { ...browserLike, Cookie: cookieHeader(durableOnly) }],
    ['directory-browser-like', 'https://cloud.189.cn/api/open/file/listFiles.action?folderId=-11&pageNum=1&pageSize=1&orderBy=1&order=ASC', browserLike],
    ['directory-durable-only', 'https://cloud.189.cn/api/open/file/listFiles.action?folderId=-11&pageNum=1&pageSize=1&orderBy=1&order=ASC', { ...browserLike, Cookie: cookieHeader(durableOnly) }],
  ];
  const results = [];
  for (const [name, url, rawHeaders] of variants) {
    const headers = Object.fromEntries(Object.entries(rawHeaders).filter(([, value]) => value !== undefined && value !== ''));
    try {
      const response = await fetch(url, {
        method: 'GET', headers, redirect: 'manual',
      });
      let value = {};
      try { value = await response.json(); } catch (_) {}
      const code = [value?.res_code, value?.result, value?.errorCode, value?.error_code]
        .find((item) => typeof item === 'number' || (typeof item === 'string' && /^[A-Za-z0-9_.-]{1,64}$/.test(item)));
      results.push({ name, status: response.status, code: code === undefined ? '' : String(code) });
    } catch (_) {
      results.push({ name, status: 0, code: 'transport' });
    }
  }
  return results;
}

async function waitForManager(port, child) {
  for (let attempt = 0; attempt < 90; attempt += 1) {
    if (child.exitCode !== null) throw new Error(`应用过早退出（${child.exitCode}）`);
    try {
      for (const target of await targets(port)) {
        if (target.type !== 'page' || target.url === 'about:blank') continue;
        if (await evaluate(target, 'Boolean(document.querySelector(".app-shell"))')) return target;
      }
    } catch (_) {}
    await delay(200);
  }
  throw new Error('后台应用未在限定时间内就绪');
}

async function invoke(target, command, payload) {
  const expression = `(async () => {
    try {
      const result = await window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(payload)});
      return JSON.stringify({ ok: true, result });
    } catch (error) {
      return JSON.stringify({ ok: false, error: { code: error?.code, message: error?.message || String(error) } });
    }
  })()`;
  return JSON.parse(await evaluate(target, expression));
}

async function loginWithSavedCredentials(target) {
  const expression = `(async () => {
    let credentials;
    try {
      credentials = await window.__TAURI_INTERNALS__.invoke('cloud_saved_password_credentials', { provider: 'tianyi' });
      if (!credentials?.username || !credentials?.password) return JSON.stringify({ ok: false, error: { code: 'missing_saved_credentials' } });
      const account = await window.__TAURI_INTERNALS__.invoke('cloud_login_password', { provider: 'tianyi', ...credentials });
      return JSON.stringify({ ok: true, result: { provider: account?.provider, authMethod: account?.authMethod } });
    } catch (error) {
      return JSON.stringify({ ok: false, error: { code: error?.code, message: error?.message || String(error) } });
    } finally {
      if (credentials) { credentials.username = ''; credentials.password = ''; }
    }
  })()`;
  return JSON.parse(await evaluate(target, expression));
}

async function resolveOneNativeDownload(manager, rootDirectory) {
  const pending = (rootDirectory?.items || [])
    .filter((item) => item?.kind === 'folder' && item?.id && item.id !== '__all_downloader_tianyi_recycle_bin__')
    .map((item) => item.id);
  let file = (rootDirectory?.items || []).find((item) => item?.kind === 'file' && item?.id);
  for (let visited = 0; !file && pending.length > 0 && visited < 20; visited += 1) {
    const directoryId = pending.shift();
    const directory = await invoke(manager, 'cloud_list_directory', { provider: 'tianyi', parentId: directoryId });
    if (!directory.ok) continue;
    const items = Array.isArray(directory.result?.items) ? directory.result.items : [];
    file = items.find((item) => item?.kind === 'file' && item?.id);
    pending.push(...items.filter((item) => item?.kind === 'folder' && item?.id).map((item) => item.id));
  }
  if (!file) throw new Error('根目录向下 20 个目录内没有可用于只读解析的文件');
  const resolved = await invoke(manager, 'cloud_tianyi_resolve_download_for_test', { fileId: file.id });
  if (!resolved.ok || resolved.result !== true) {
    throw new Error(`原生 App 下载地址解析失败：${JSON.stringify(resolved.error || { resolved: resolved.result })}`);
  }
  return true;
}

async function officialPageState(port) {
  const pages = (await targets(port)).filter((target) => target.type === 'page');
  const page = pages.find((target) => {
    try { return new URL(target.url).hostname === 'cloud.189.cn'; } catch (_) { return false; }
  });
  if (!page) return { open: false };
  return await evaluate(page, `(() => {
    const context = window.__alldownloaderTianyiContext;
    const diagnostic = window.__alldownloaderAuthDiagnostic;
    let iframeBrowserId = false;
    for (const frame of document.querySelectorAll('iframe[src]')) {
      try { iframeBrowserId ||= /^[a-f0-9]{32}$/i.test(new URL(frame.src, location.href).searchParams.get('browserId') || ''); } catch (_) {}
    }
    return {
      open: true,
      loggedIn: Boolean(document.body?.innerText?.includes('全部文件') && document.body?.innerText?.includes('回收站')),
      captureInstalled: window.__alldownloaderTianyiCapture === true,
      browserIdCaptured: /^[a-f0-9]{32}$/i.test(context?.browser_id || ''),
      userAgentCaptured: typeof context?.user_agent === 'string' && context.user_agent.length > 0,
      iframeBrowserId,
      nativeStage: typeof diagnostic?.stage === 'string' ? diagnostic.stage.slice(0, 40) : '',
      nativeReason: typeof diagnostic?.reason === 'string' ? diagnostic.reason.slice(0, 160) : '',
      title: String(document.title || '').slice(0, 80),
    };
  })()`);
}

async function officialAccountProbe(port) {
  const pages = (await targets(port)).filter((target) => target.type === 'page');
  const page = pages.find((target) => {
    try { return new URL(target.url).hostname === 'cloud.189.cn'; } catch (_) { return false; }
  });
  if (!page) return { available: false };
  const account = await evaluate(page, `(async () => {
    const context = window.__alldownloaderTianyiContext;
    const response = await fetch('/api/open/user/getUserInfoForPortal.action', {
      method: 'GET',
      credentials: 'include',
      headers: {
        Accept: 'application/json;charset=UTF-8',
        'Browser-Id': String(context?.browser_id || ''),
      },
    });
    let value = {};
    try { value = await response.json(); } catch (_) {}
    const safeCode = [value?.res_code, value?.result, value?.errorCode, value?.error_code]
      .find((item) => typeof item === 'number' || (typeof item === 'string' && /^[A-Za-z0-9_.-]{1,64}$/.test(item)));
    return {
      available: true,
      status: response.status,
      code: safeCode === undefined ? '' : String(safeCode),
      topLevelKeys: Object.keys(value || {}).sort().slice(0, 24),
      userShapeKeys: Object.keys(value?.userInfo || value?.data || {}).sort().slice(0, 24),
    };
  })()`);
  const cookieResult = await command(page, 'Network.getCookies', {
    urls: ['https://cloud.189.cn/', 'https://open.e.189.cn/'],
  });
  const cookieShape = (cookieResult?.cookies || []).map((cookie) => ({
    name: /^[A-Za-z0-9!#$%&'*+.^_\x60|~-]{1,64}$/.test(cookie.name || '') ? cookie.name : 'Unknown',
    domain: String(cookie.domain || '').slice(0, 80),
    path: String(cookie.path || '').slice(0, 80),
    httpOnly: Boolean(cookie.httpOnly),
    secure: Boolean(cookie.secure),
    sameSite: String(cookie.sameSite || ''),
  })).sort((left, right) => `${left.name}|${left.domain}|${left.path}`.localeCompare(`${right.name}|${right.domain}|${right.path}`));
  const requestShape = await captureOfficialAccountRequestShape(page);
  const nativeVariants = await probeNativeVariants(page, cookieResult, requestShape.cookieOrder);
  return { ...account, cookieShape, requestShape, nativeVariants };
}

async function closeApplication(port, child) {
  if (child.exitCode !== null) return;
  try {
    const page = (await targets(port)).find((target) => target.type === 'page' && target.url !== 'about:blank');
    if (page) await invoke(page, 'activate_launcher_menu_for_test', { action: 'launcher.quit' });
  } catch (_) { child.kill(); }
  await Promise.race([
    new Promise((resolve) => child.once('exit', resolve)),
    delay(8000).then(() => child.kill()),
  ]);
}

async function run() {
  if (!fs.existsSync(executable)) throw new Error(`未找到发布二进制：${executable}`);
  const sourceSessions = path.join(sourceDataDirectory, 'cloud-sessions');
  if (!manualAuthorization) {
    const required = path.join(
      sourceSessions,
      savedSessionOnly ? 'tianyi.bin' : 'cloud-password-draft-tianyi.bin',
    );
    if (!fs.existsSync(required)) throw new Error('当前应用数据目录中没有所需的天翼会话样本');
  }
  const testDataDirectory = manualAuthorization
    ? path.join(os.tmpdir(), 'alldownloader-tianyi-manual-auth')
    : fs.mkdtempSync(path.join(os.tmpdir(), 'alldownloader-tianyi-live-'));
  fs.mkdirSync(testDataDirectory, { recursive: true });
  const testSessions = path.join(testDataDirectory, 'cloud-sessions');
  fs.mkdirSync(testSessions, { recursive: true });
  for (const name of manualAuthorization ? [] : ['cloud-password-draft-tianyi.bin', 'cloud-password-tianyi.bin', 'tianyi.bin']) {
    const source = path.join(sourceSessions, name);
    if (fs.existsSync(source)) fs.copyFileSync(source, path.join(testSessions, name));
  }
  const port = await availablePort();
  const child = spawn(executable, [], {
    cwd: path.dirname(executable), windowsHide: true, stdio: 'ignore',
    env: {
      ...process.env,
      APPDATA: path.join(testDataDirectory, 'roaming'),
      ALLDOWNLOADER_BACKGROUND_UI_TEST: '1',
      ALLDOWNLOADER_TEST_DATA_DIR: testDataDirectory,
      ALLDOWNLOADER_TEST_DOWNLOAD_DIR: path.join(testDataDirectory, 'downloads'),
      WEBVIEW2_USER_DATA_FOLDER: path.join(testDataDirectory, 'webview2'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
    },
  });
  try {
    const manager = await waitForManager(port, child);
    if (manualAuthorization) {
      const started = await invoke(manager, 'cloud_begin_authorization', { provider: 'tianyi' });
      if (!started.ok) throw new Error(`无法打开天翼人工授权页：${JSON.stringify(started.error)}`);
      const deadline = Date.now() + 600000;
      let browserLoggedAt = 0;
      let manualProbe = { available: false };
      while (Date.now() < deadline) {
        await delay(1000);
        const pageState = await officialPageState(port).catch(() => ({ open: false }));
        if (pageState.loggedIn && pageState.browserIdCaptured) {
          if (!browserLoggedAt) browserLoggedAt = Date.now();
          if (!manualProbe.available) {
            manualProbe = await officialAccountProbe(port).catch(() => manualProbe);
          }
        }
        const completed = await invoke(manager, 'cloud_complete_authorization', { provider: 'tianyi' });
        if (!completed.ok) {
          if (browserLoggedAt && Date.now() - browserLoggedAt > 15000) {
            throw new Error(`人工网页已登录，但原生会话仍未通过；官网接口：${JSON.stringify(manualProbe)}`);
          }
          if (['authorization_pending', 'web_auth_required', 'credentials_expired'].includes(completed.error?.code)) continue;
          throw new Error(`天翼人工网页授权校验失败：${JSON.stringify(completed.error)}`);
        }
        const shape = await invoke(manager, 'cloud_tianyi_session_shape_for_test', {});
        if (!shape.ok) throw new Error(`人工网页授权会话形状读取失败：${JSON.stringify(shape.error)}`);
        const directory = await invoke(manager, 'cloud_list_directory', { provider: 'tianyi', parentId: '-11' });
        if (!directory.ok) throw new Error(`人工网页授权完成但目录读取失败：${JSON.stringify(directory.error)}`);
        const itemCount = Array.isArray(directory.result?.items) ? directory.result.items.length : 0;
        process.stdout.write(`天翼人工网页授权 A 方案验证通过：${JSON.stringify(shape.result)}；根目录 ${itemCount} 项。\n`);
        return;
      }
      throw new Error('等待天翼人工网页授权超时');
    }
    if (savedSessionOnly) {
      const shape = await invoke(manager, 'cloud_tianyi_session_shape_for_test', {});
      if (shape.ok) process.stdout.write(`天翼历史会话安全形状：${JSON.stringify(shape.result)}\n`);
      if (shapeOnly) {
        if (!shape.ok) throw new Error(`历史会话形状读取失败：${JSON.stringify(shape.error)}`);
        return;
      }
      const account = await invoke(manager, 'cloud_account', { provider: 'tianyi' });
      if (!account.ok) throw new Error(`历史网页授权会话账号校验失败：${JSON.stringify(account.error)}`);
      const directory = await invoke(manager, 'cloud_list_directory', { provider: 'tianyi', parentId: '-11' });
      if (!directory.ok) throw new Error(`历史网页授权会话目录读取失败：${JSON.stringify(directory.error)}`);
      const itemCount = Array.isArray(directory.result?.items) ? directory.result.items.length : 0;
      process.stdout.write(`天翼历史网页授权会话原生验证通过：根目录已读取（${itemCount} 项）。\n`);
      return;
    }
    const login = loginWithSavedCredentials(manager);
    let latest = { open: false };
    let officialProbe = { available: false };
    const deadline = Date.now() + 45000;
    while (Date.now() < deadline) {
      latest = await officialPageState(port).catch(() => latest);
      if (latest.loggedIn && latest.browserIdCaptured && !officialProbe.available) {
        officialProbe = await officialAccountProbe(port).catch(() => officialProbe);
      }
      if (
        latest.loggedIn
        && latest.browserIdCaptured
        && /错误码|HTTP\s*\d/i.test(latest.nativeReason || '')
      ) {
        throw new Error(`原生接口校验失败；页面状态：${JSON.stringify(latest)}；官网接口：${JSON.stringify(officialProbe)}`);
      }
      const settled = await Promise.race([login.then((value) => ({ value })), delay(500).then(() => null)]);
      if (settled) {
        if (!settled.value.ok) throw new Error(`登录校验失败：${JSON.stringify(settled.value.error)}；页面状态：${JSON.stringify(latest)}；官网接口：${JSON.stringify(officialProbe)}`);
        const shape = await invoke(manager, 'cloud_tianyi_session_shape_for_test', {});
        if (!shape.ok) throw new Error(`登录完成但会话形状读取失败：${JSON.stringify(shape.error)}`);
        if (shape.result?.transport !== 'nativeApp') {
          throw new Error(`自动登录没有使用原生 App 会话：${JSON.stringify(shape.result)}`);
        }
        process.stdout.write(`天翼自动登录会话安全形状：${JSON.stringify(shape.result)}\n`);
        const directory = await invoke(manager, 'cloud_list_directory', { provider: 'tianyi', parentId: '-11' });
        if (!directory.ok) throw new Error(`登录完成但目录读取失败：${JSON.stringify(directory.error)}`);
        const itemCount = Array.isArray(directory.result?.items) ? directory.result.items.length : 0;
        const downloadResolved = await resolveOneNativeDownload(manager, directory.result);
        process.stdout.write(`天翼真实账号后台验证通过：授权已结束，根目录已读取（${itemCount} 项），下载地址已解析=${downloadResolved}。\n`);
        return;
      }
    }
    throw new Error(`登录校验超时；页面状态：${JSON.stringify(latest)}；官网接口：${JSON.stringify(officialProbe)}`);
  } finally {
    await closeApplication(port, child);
    if (!manualAuthorization) {
      try {
        fs.rmSync(testDataDirectory, { recursive: true, force: true, maxRetries: 8, retryDelay: 500 });
      } catch (_) {
        process.stderr.write(`后台测试临时目录稍后由系统清理：${path.basename(testDataDirectory)}\n`);
      }
    }
  }
}

run().catch((error) => {
  process.stderr.write(`天翼真实账号后台验证失败：${String(error?.message || error).slice(0, 5000)}\n`);
  process.exitCode = 1;
});
