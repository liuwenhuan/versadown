// Read-only validation of the encrypted Baidu session used by the native
// client.  Browser-profile capture is intentionally not used here: the
// provider session can remain valid after its WebView profile has expired.
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const projectRoot = path.resolve(__dirname, '..');
const cargo = process.env.VERSADOWN_CARGO || 'cargo';
const manifest = path.join(projectRoot, 'src-tauri', 'Cargo.toml');

if (!fs.existsSync(manifest)) {
  throw new Error(`未找到百度实时测试清单：${manifest}`);
}

const result = spawnSync(cargo, [
  'test',
  '--manifest-path', manifest,
  '--test', 'live_baidu_tianyi',
  'live_baidu_personal_download_bytes',
  '--', '--ignored', '--exact'
], {
  cwd: projectRoot,
  env: { ...process.env, VERSADOWN_LIVE_CLOUD: '1' },
  stdio: ['ignore', 'pipe', 'pipe'],
  windowsHide: true
});

if (result.error) throw result.error;
if (result.signal) throw new Error(`百度实时测试被信号终止：${result.signal}`);
const stdout = String(result.stdout || '');
const stderr = String(result.stderr || '');
process.stdout.write(stdout);
process.stderr.write(stderr);
if (result.status !== 0) {
  process.exitCode = result.status || 1;
} else if (!/running 1 test/.test(stdout)
  || !/test live_baidu_personal_download_bytes \.\.\. ok/.test(stdout)
  || !/test result: ok\. 1 passed; 0 failed; 0 ignored;/.test(stdout)) {
  throw new Error('百度实时测试未精确执行并通过 1 项目标测试');
}
