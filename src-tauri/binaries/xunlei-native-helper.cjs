'use strict';

const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');
const crypto = require('node:crypto');

function requiredEnvironment(name) {
  const value = process.env[name];
  if (!value) throw new Error(`missing_${name.toLowerCase()}`);
  return value;
}

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

let startupStage = 'environment';
try {
  const addonPath = requiredEnvironment('XUNLEI_DK_ADDON');
  const stateDir = requiredEnvironment('XUNLEI_NATIVE_STATE_DIR');
  const peerId = process.env.XUNLEI_NATIVE_PEER_ID || '9CB150B4D2EACQHQ';
  const userId = process.env.XUNLEI_NATIVE_USER_ID || '';
  startupStage = 'addon';
  const addon = require(addonPath);
  const directories = {
    logDir: path.join(stateDir, 'logs'),
    dkCfgDir: path.join(stateDir, 'dkcfg'),
    torrentsCacheDir: path.join(stateDir, 'torrents'),
    tempDir: path.join(stateDir, 'temp'),
  };
  fs.mkdirSync(stateDir, { recursive: true });
  for (const directory of Object.values(directories)) fs.mkdirSync(directory, { recursive: true });

  startupStage = 'initialize';
  addon.initAddon({
    dbDir: stateDir,
    dbName: 'TaskDb.dat',
    ...directories,
    versionName: requiredEnvironment('XUNLEI_CLIENT_VERSION'),
    versionCode: '',
    appName: 'thunder',
    appKey: 'xzcGMudGh1bmRlclg7MA^^SDK==edee53fd0b15e8d65dbfe7824f5f^a23',
    panDstDbName: 'PanUpload.dat',
    guid: peerId,
  });

  startupStage = 'manager';
  const manager = new addon.NativeTaskManager();
  const taskApi = addon.NativeTaskInterface;
  manager.beginInitDownloadServer();

  function taskById(taskId) {
    const task = manager.findTaskById(Number(taskId));
    if (!task || !task.t) throw new Error('task_not_found');
    return task.t;
  }

  async function handle(command) {
    switch (command.action) {
      case 'create': {
        const savePath = path.resolve(command.savePath);
        fs.mkdirSync(savePath, { recursive: true });
        const taskId = manager.createP2spTask({
          background: false,
          taskType: 1,
          taskBaseInfo: {
            categoryId: 1,
            fileSize: Number(command.fileSize || 0),
            savePath,
            taskName: command.name,
            origin: command.url,
            cid: command.cid || '',
            gcid: command.gcid || '',
            userData: [],
          },
        }, {
          url: command.url,
          origin: command.url,
          refUrl: command.referer || '',
          useOriginResourceOnly: false,
          nameFixed: true,
          // The Xunlei plugin owns this limit; application task concurrency
          // settings must not override it.
          originResourceThreadCount: 2,
          cookie: '',
          userAgent: command.userAgent || 'Mozilla/5.0',
          httpHeaderField: '',
          loginFtp: false,
          ftpUserName: '',
          ftpPassword: '',
          autoRenameWhenRepeat: false,
        });
        if (!Number.isSafeInteger(taskId) || taskId <= 0) throw new Error('create_failed');
        return taskId;
      }
      case 'enableVip': {
        const task = taskById(command.taskId);
        taskApi.setTaskEquityToken(task, command.token, -1);
        return true;
      }
      case 'taskInfo': {
        const base = taskApi.getTaskBase(taskById(command.taskId)) || {};
        return { traceId: String(base.traceId || '') };
      }
      case 'vipSpeed': {
        const base = taskApi.getTaskBase(taskById(command.taskId)) || {};
        const category = manager.getCategoryManager().getCategoryById(Number(base.categoryId || 1));
        return Number(category?.getDownloadVipSpeed?.() || 0);
      }
      case 'decryptRule': {
        const key = Buffer.from(command.key, 'utf8');
        const decipher = crypto.createDecipheriv('aes-128-cbc', key, key);
        const plain = Buffer.concat([
          decipher.update(Buffer.from(command.data, 'base64')),
          decipher.final(),
        ]);
        const payload = JSON.parse(plain.toString('utf8'));
        const token = String(payload?.rule?.[0]?.id || '');
        plain.fill(0);
        key.fill(0);
        if (!token) throw new Error('equity_rule_missing');
        return token;
      }
      case 'start':
        manager.batchStartTasks([Number(command.taskId)]);
        return true;
      case 'stop':
        manager.batchStopTasks([Number(command.taskId)], 0);
        return true;
      case 'delete':
        manager.batchDeleteTasks([Number(command.taskId)], false);
        return true;
      case 'shutdown':
        setImmediate(() => process.exit(0));
        return true;
      default:
        throw new Error('unknown_action');
    }
  }

  // The official renderer waits through waitLoadFinish(callback). The same
  // addon invokes that callback outside a V8 HandleScope in Electron's Node
  // mode, so the isolated helper instead lets the server finish its async
  // startup before accepting commands.
  setTimeout(() => {
    if (userId) {
      manager.setUserInfo(userId, '');
      manager.setGlobalExtInfo(`isvip=1,viptype=super,viplevel=,userchannel=,client_peerid=${peerId}`, false);
    }
    manager.setTokenMode(2);
    send({ ready: true });
    const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
    input.on('line', async line => {
      let command;
      try {
        command = JSON.parse(line);
        send({ id: command.id, ok: true, result: await handle(command) });
      } catch (error) {
        send({ id: command && command.id, ok: false, error: String(error && error.message || 'native_error') });
      }
    });
    input.on('close', () => process.exit(0));
  }, 2000);
} catch (error) {
  send({ ready: false, error: `incompatible_${startupStage}` });
  process.exit(2);
}
