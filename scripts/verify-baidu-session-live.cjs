const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const WebSocket = require('ws');

const projectRoot = path.resolve(__dirname, '..');
const executable = process.env.ALLDOWNLOADER_LIVE_EXECUTABLE
  || path.join(projectRoot, 'artifacts', 'alldownloader.exe');
const dataDirectory = process.env.ALLDOWNLOADER_LIVE_DATA_DIR
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

async function evaluate(target, expression) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    socket.once('open', () => socket.send(JSON.stringify({
      id: 1,
      method: 'Runtime.evaluate',
      params: { expression, awaitPromise: true, returnByValue: true }
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
  for (let attempt = 0; attempt < 60; attempt += 1) {
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
  throw new Error('后台应用未在限定时间内就绪');
}

function safeError(error) {
  if (typeof error === 'string') return error.slice(0, 300);
  if (error && typeof error === 'object') {
    const candidate = error.message || error.code || JSON.stringify(error);
    return String(candidate).slice(0, 300);
  }
  return String(error).slice(0, 300);
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
          : { code: error?.code, message: error?.message, name: error?.name }
      });
    }
  })()`;
  return JSON.parse(await evaluate(target, expression));
}

async function waitForExit(child) {
  if (child.exitCode !== null) return child.exitCode;
  return await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('后台应用未能退出')), 8000);
    child.once('exit', (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
}

async function run() {
  if (!fs.existsSync(executable)) throw new Error(`未找到发布二进制：${executable}`);
  if (!fs.existsSync(path.join(dataDirectory, 'cloud-sessions', 'baidu.bin'))) {
    throw new Error('当前应用数据目录中不存在百度授权会话');
  }

  const port = await availablePort();
  const child = spawn(executable, [], {
    cwd: path.dirname(executable),
    windowsHide: true,
    stdio: 'ignore',
    env: {
      ...process.env,
      ALLDOWNLOADER_BACKGROUND_UI_TEST: '1',
      ALLDOWNLOADER_BAIDU_DIAGNOSTIC: '1',
      ALLDOWNLOADER_TEST_DATA_DIR: dataDirectory,
      ALLDOWNLOADER_TEST_DOWNLOAD_DIR: path.join(dataDirectory, 'downloads'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`
    }
  });

  try {
    const manager = await waitForManager(port, child);
    const account = await invoke(manager, 'cloud_account', { provider: 'baidu' });
    if (!account.ok) throw new Error(`百度账号校验失败：${safeError(account.error)}`);

    const directory = await invoke(manager, 'cloud_list_directory', { provider: 'baidu', parentId: '/' });
    if (!directory.ok) throw new Error(`百度根目录读取失败：${safeError(directory.error)}`);

    const itemCount = Array.isArray(directory.result?.items) ? directory.result.items.length : 0;
    process.stdout.write(`百度真实会话后台验证通过：账号已确认，根目录已读取（${itemCount} 项）。\n`);
  } finally {
    if (child.exitCode === null) {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/json/list`);
        const targets = await response.json();
        const launcher = targets.find((target) => target.type === 'page' && target.url !== 'about:blank' && target.webSocketDebuggerUrl);
        if (launcher) await invoke(launcher, 'activate_launcher_menu_for_test', { action: 'launcher.quit' });
      } catch (_) {
        child.kill();
      }
      await waitForExit(child).catch(() => child.kill());
    }
  }
}

run().catch((error) => {
  process.stderr.write(`百度真实会话后台验证失败：${safeError(error)}\n`);
  process.exitCode = 1;
});
