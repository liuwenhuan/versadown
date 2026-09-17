// Isolated native-engine contract check. Never connects to the application's RPC.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { DatabaseSync } = require('node:sqlite');

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const report = value => console.log(JSON.stringify(value));
const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

async function until(description, operation, timeout = 20000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await operation();
    if (value) return value;
    await sleep(50);
  }
  throw Error(`Timed out: ${description}`);
}

async function main() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'alldownloader-native-resume-'));
  const output = path.join(root, 'payload');
  const stateDir = path.join(root, 'aria2-next-state');
  const session = path.join(root, 'aria2-next.tasks');
  fs.mkdirSync(output);
  fs.writeFileSync(session, '');
  const payload = crypto.randomBytes(16 * 1024 * 1024);
  const expectedHash = digest(payload);
  const requests = [];
  const seedServed = new Set();
  let expired = false;
  const server = http.createServer((req, res) => {
    const route = new URL(req.url, 'http://127.0.0.1').pathname;
    const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
    const start = range ? Number(range[1]) : 0;
    const end = range && range[2] ? Math.min(Number(range[2]), payload.length - 1) : payload.length - 1;
    requests.push({ route, at: Date.now(), start, end, range: req.headers.range || null });
    if (route.startsWith('/empty')) {
      req.socket.destroy();
      return;
    }
    if (route.startsWith('/seed') && expired) {
      res.writeHead(403, { 'Content-Length': '0' });
      res.end();
      return;
    }
    if (route.startsWith('/seed') && seedServed.has(route)) {
      req.socket.destroy();
      return;
    }
    if (route.startsWith('/seed')) seedServed.add(route);
    const headers = {
      'Content-Length': String(end - start + 1),
      'Accept-Ranges': 'bytes',
      ETag: '"isolated-native-resume-content-v1"',
      'Content-Type': 'application/octet-stream',
    };
    if (range) headers['Content-Range'] = `bytes ${start}-${end}/${payload.length}`;
    res.writeHead(range ? 206 : 200, headers);
    res.end(payload.subarray(start, end + 1));
  });
  await listen(server);
  const origin = `http://127.0.0.1:${server.address().port}`;
  const reservation = http.createServer();
  await listen(reservation);
  const rpcPort = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  const token = crypto.randomBytes(24).toString('hex');
  const binary = path.resolve(__dirname, '../src-tauri/binaries/aria2-next-x86_64-pc-windows-msvc.exe');
  const child = spawn(binary, [
    '--no-conf=true', '--enable-rpc=true', '--rpc-listen-all=false',
    '--rpc-allow-origin-all=false', '--disable-ipv6=true',
    `--rpc-listen-port=${rpcPort}`, `--rpc-secret=${token}`,
    '--continue=true', `--state-dir=${stateDir}`, `--input-file=${session}`,
    `--save-session=${session}`, '--save-session-interval=10', '--force-save=false',
    '--pause-metadata=true', `--dir=${output}`, `--stop-with-process=${process.pid}`,
    '--split=2', '--max-connection-per-server=2', '--max-concurrent-downloads=2',
    '--seed-ratio=0', '--max-overall-download-limit=0', '--max-overall-upload-limit=0',
    '--enable-dht=false', '--enable-dht6=false', '--bt-enable-lpd=false',
    '--summary-interval=0', '--console-log-level=error', '--all-proxy=',
  ], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let engineOutput = '';
  let spawnError;
  child.on('error', error => { spawnError = error; });
  for (const stream of [child.stdout, child.stderr]) {
    stream.on('data', data => { engineOutput = (engineOutput + data).slice(-16000); });
  }
  const rpc = async (method, params = []) => {
    const response = await fetch(`http://127.0.0.1:${rpcPort}/jsonrpc`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: `aria2.${method}`, params: [`token:${token}`, ...params] }),
      signal: AbortSignal.timeout(3000),
    });
    const result = await response.json();
    if (result.error) throw Error(`RPC ${method}: ${result.error.message}`);
    return result.result;
  };
  const status = gid => rpc('tellStatus', [gid]);
  const waitStatus = (gid, expected, timeout) => until(`status ${expected}`, async () => {
    const value = await status(gid);
    return value.status === expected && value;
  }, timeout);
  const add = (uris, out, options = {}) => rpc('addUri', [uris, {
    dir: output, out, 'all-proxy': '', split: '2', 'max-connection-per-server': '2',
    'min-split-size': '1M', 'file-allocation': 'trunc', 'no-file-allocation-limit': '1M',
    'max-tries': '5', 'retry-wait': '3', timeout: '5', 'connect-timeout': '5',
    ...options,
  }]);
  const routeRequests = route => requests.filter(item => item.route === route);
  const stateFile = path.join(stateDir, 'stream', 'state.db');
  const checkpoint = gid => {
    const db = new DatabaseSync(stateFile, { readOnly: true });
    try { return db.prepare('SELECT * FROM downloads WHERE gid=?').get(gid); }
    finally { db.close(); }
  };
  try {
    await until('new isolated engine ready', async () => {
      if (spawnError) throw spawnError;
      if (child.exitCode !== null) throw Error(`Isolated engine exited ${child.exitCode}`);
      return rpc('getVersion').catch(() => false);
    });
    report({ check: 'isolation', root, enginePid: child.pid, binarySha256: digest(fs.readFileSync(binary)) });

    const finite = await add([`${origin}/empty-finite`], 'finite.bin', { split: '1', 'max-connection-per-server': '1' });
    const finiteResult = await waitStatus(finite, 'error', 35000);
    const finiteRequests = routeRequests('/empty-finite');
    const intervals = finiteRequests.slice(1).map((item, i) => item.at - finiteRequests[i].at);
    assert.equal(finiteRequests.length, 5, 'max-tries=5 must stop after five empty replies');
    assert(intervals.every(ms => ms >= 2900), 'retry-wait=3 must space requests');
    report({ check: 'bounded_retry', attempts: finiteRequests.length, intervalsMs: intervals, ariaErrorCode: finiteResult.errorCode });

    for (const action of ['pause', 'remove']) {
      const route = `/empty-${action}`;
      const gid = await add([`${origin}${route}`], `${action}.bin`, { split: '1', 'max-connection-per-server': '1' });
      await until('first empty response before control', () => routeRequests(route).length > 0);
      await sleep(200);
      const controlAt = Date.now();
      await rpc(action, [gid]);
      const controlled = await waitStatus(gid, action === 'pause' ? 'paused' : 'removed');
      const latency = Date.now() - controlAt;
      const count = routeRequests(route).length;
      await sleep(3500);
      assert.equal(routeRequests(route).length, count, `${action} must prevent retry after its deadline`);
      assert(latency < 2500, `${action} should interrupt the backoff wait`);
      report({ check: `backoff_${action === 'remove' ? 'cancel' : action}`, controlLatencyMs: latency, requests: count, status: controlled.status });
    }

    const seed = async (route, filename) => {
      const gid = await add([`${origin}${route}`], filename, { 'max-tries': '2', 'retry-wait': '0' });
      const failed = await waitStatus(gid, 'error');
      const record = checkpoint(gid);
      assert(record, 'failed partial download must retain native checkpoint');
      assert(record.completed_length > 0 && record.completed_length < payload.length);
      assert.equal(record.total_length, payload.length);
      assert.equal(fs.statSync(path.join(output, filename)).size, payload.length, 'fixture must really be preallocated');
      report({ check: 'partial_checkpoint', filename, completedBytes: record.completed_length, totalBytes: record.total_length, completedRanges: record.completed_ranges, ariaErrorCode: failed.errorCode });
      return { gid, record };
    };
    const first = await seed('/seed-resume', 'resume.bin');
    const lost = await seed('/seed-missing', 'missing.bin');
    const failAgain = await seed('/seed-fail-again', 'fail-again.bin');
    expired = true;
    const failedResume = await add([`${origin}/empty-resume`, failAgain.record.uri], 'fail-again.bin', {
      continue: 'false', 'allow-overwrite': 'false', 'auto-file-renaming': 'false',
      'max-tries': '2', 'retry-wait': '0',
    });
    const failedAgain = await waitStatus(failedResume, 'error');
    const reboundCheckpoint = checkpoint(failedResume);
    assert(reboundCheckpoint, 'failed retry with zero new bytes must still rebind checkpoint GID');
    assert.equal(reboundCheckpoint.completed_length, failAgain.record.completed_length);
    assert.equal(reboundCheckpoint.completed_ranges, failAgain.record.completed_ranges);
    assert.equal(checkpoint(failAgain.gid), undefined);
    report({ check: 'failed_resume_rebind', distinctGid: failedResume !== failAgain.gid, newBytes: 0, completedBytes: reboundCheckpoint.completed_length, completedRanges: reboundCheckpoint.completed_ranges, ariaErrorCode: failedAgain.errorCode, checkpointMovedToNewGid: true });
    const resumeStarted = Date.now();
    const requestStart = requests.length;
    const resume = await add([`${origin}/fresh`, first.record.uri], 'resume.bin', {
      continue: 'false', 'allow-overwrite': 'false', 'auto-file-renaming': 'false',
    });
    assert.notEqual(resume, first.gid);
    const resumed = await until('native resume completion', async () => {
      const value = await status(resume);
      if (value.status === 'error') throw Error(`Resume failed: ${value.errorCode} ${value.errorMessage}`);
      return value.status === 'complete' && value;
    }, 30000);
    const actualHash = digest(fs.readFileSync(path.join(output, 'resume.bin')));
    assert.equal(actualHash, expectedHash, 'restored sparse ranges must produce exact original bytes');
    assert.equal(Number(resumed.completedLength), payload.length);
    const resumeRequests = requests.slice(requestStart).filter(item => ['/fresh', '/seed-resume'].includes(item.route));
    assert.equal(resumeRequests[0].route, '/seed-resume', 'checkpoint URI is first restored, even when fresh is first in addUri');
    assert(resumeRequests.some(item => item.route === '/fresh' && item.range && item.start > 0));
    assert(resumeRequests.every(item => item.start >= first.record.completed_length), 'already completed prefix must not be downloaded again');
    assert.equal(checkpoint(first.gid), undefined, 'new checkpoint must replace old GID row');
    report({ check: 'native_resume', distinctGid: true, elapsedMs: Date.now() - resumeStarted, old403Requests: resumeRequests.filter(item => item.route === '/seed-resume').length, freshRequests: resumeRequests.filter(item => item.route === '/fresh').length, firstRange: resumeRequests[0].range, firstFreshRange: resumeRequests.find(item => item.route === '/fresh').range, expectedSha256: expectedHash, actualSha256: actualHash });

    const missingPath = path.join(output, 'missing.bin');
    const missingHash = digest(fs.readFileSync(missingPath));
    const namesBefore = fs.readdirSync(output).sort();
    const db = new DatabaseSync(stateFile);
    try { assert.equal(db.prepare('DELETE FROM downloads WHERE gid=?').run(lost.gid).changes, 1); }
    finally { db.close(); }
    assert.equal(checkpoint(lost.gid), undefined);
    const requestCount = requests.length;
    const missing = await add([`${origin}/fresh-missing`, lost.record.uri], 'missing.bin', {
      continue: 'false', 'allow-overwrite': 'false', 'auto-file-renaming': 'false',
    });
    const rejected = await until('missing checkpoint rejection', async () => {
      const value = await status(missing);
      assert.notEqual(value.status, 'complete', 'preallocated length must never be mistaken for completion');
      return value.status === 'error' && value;
    });
    assert.equal(digest(fs.readFileSync(missingPath)), missingHash, 'missing checkpoint must not overwrite payload');
    assert.deepEqual(fs.readdirSync(output).sort(), namesBefore, 'missing checkpoint must not auto-rename');
    assert.equal(requests.length, requestCount, 'existing payload safety must reject before HTTP transfer');
    report({ check: 'missing_checkpoint', status: rejected.status, ariaErrorCode: rejected.errorCode, unchangedSha256: missingHash, newRequests: requests.length - requestCount, noRename: true });
    report({ result: 'PASS', root });
  } catch (error) {
    report({ result: 'FAIL', message: String(error.message).replaceAll(token, '<redacted>'), root, engineOutput: engineOutput.replaceAll(token, '<redacted>') });
    throw error;
  } finally {
    if (child.exitCode === null) {
      await rpc('forceShutdown').catch(() => {});
      await until('owned engine stopped', () => child.exitCode !== null, 4000).catch(() => child.kill());
    }
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
}

main().catch(error => { console.error(error.name + ': ' + error.message); process.exitCode = 1; });
