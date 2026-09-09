// Explicit live test: all mutations stay under its freshly created cloud folder.
const { evaluate, targets } = require('./xunlei-live-probe.cjs');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');

async function main() {
  if (process.env.ALLDOWNLOADER_LIVE_CLOUD !== '1') throw Error('explicit_live_opt_in_required');
  const port = Number(process.argv[2]);
  const dataDir = path.resolve(process.argv[3] || '.');
  if (!path.basename(dataDir).startsWith('alldownloader-xunlei-live-')) throw Error('isolated_app_required');
  let app;
  for (const candidate of (await targets(port)).filter(t => /tauri.localhost/.test(t.url))) {
    if (await evaluate(candidate, `!!document.querySelector('.cloud-workspace')`)) { app = candidate; break; }
  }
  if (!app) throw Error('cloud_workspace_missing');
  let restored = false;
  for (let attempt=0; attempt<30; attempt++) {
    restored = await evaluate(app, `(()=>{const tab=Array.from(document.querySelectorAll('[role="tab"]')).find(t=>t.textContent==='迅雷网盘');if(!tab)return false;tab.click();return true;})()`);
    if (restored) break;
    await new Promise(resolve=>setTimeout(resolve,1000));
  }
  assert(restored, 'xunlei_saved_session_tab_not_restored');
  console.log('PASS saved Xunlei session restores its workspace tab');
  async function invoke(command, args = {}) {
    const result = await evaluate(app, `(async()=>{try{return {ok:true,value:await window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})}}catch(e){return {ok:false,code:e?.code||'command_failed'}}})()`, 60000);
    if (!result?.ok) throw Error(`${command}: ${result?.code || 'invalid_reply'}`);
    return result.value;
  }
  const local = fs.mkdtempSync(path.join(os.tmpdir(), 'AllDownloader-IT-xunlei-app-'));
  const name = path.basename(local);
  const payload = crypto.randomBytes(32768);
  const fixture = path.join(local, `${name}.bin`);
  fs.writeFileSync(fixture, payload);
  let root;
  try {
    await invoke('cloud_list_directory', {provider:'xunlei',parentId:'0'});
    root = await invoke('cloud_create_directory', {provider:'xunlei',parentId:'0',name});
    assert.equal(root.kind, 'folder'); assert.equal(root.name, name);
    const child = await invoke('cloud_create_directory', {provider:'xunlei',parentId:root.id,name:'nested'});
    const uploaded = await invoke('cloud_upload_files', {provider:'xunlei',parentId:root.id,paths:[fixture]});
    assert.equal(uploaded.length, 1);
    const id = uploaded[0].id;
    await invoke('cloud_rename', {provider:'xunlei',fileId:id,name:'renamed.bin'});
    await invoke('cloud_move', {provider:'xunlei',fileIds:[id],targetParentId:child.id});
    await invoke('cloud_copy', {provider:'xunlei',fileIds:[id],targetParentId:root.id});
    const moved = await invoke('cloud_list_directory', {provider:'xunlei',parentId:child.id});
    assert(moved.items.some(f=>f.id===id && f.name==='renamed.bin'));
    const copied = await invoke('cloud_list_directory', {provider:'xunlei',parentId:root.id});
    assert(copied.items.some(f=>f.kind==='file' && f.name==='renamed.bin' && f.id!==id));
    console.log('PASS app upload, rename, move, copy and directory visibility');
    const outcome = await invoke('cloud_download_directory', {provider:'xunlei',directoryId:root.id,directoryName:name});
    if (outcome.failures.length) console.log('directory_queue_failure_codes', outcome.failures.map(f=>f.code));
    assert.equal(outcome.failures.length, 0); assert.equal(outcome.queuedCount, 2);
    const ids = new Set(outcome.tasks.map(t=>t.id));
    let downloaded;
    for (let attempt=0; attempt<60; attempt++) {
      const tasks = (await invoke('list_tasks')).filter(t=>ids.has(t.id));
      if (tasks.some(t=>t.status==='failed')) throw Error('aria2_download_failed');
      if (tasks.length===2 && tasks.every(t=>t.status==='completed')) {downloaded=tasks;break;}
      await new Promise(resolve=>setTimeout(resolve,1000));
    }
    assert(downloaded, 'aria2_completion_timeout');
    const allowed = path.resolve(dataDir,'downloads') + path.sep;
    const actualPaths = [];
    for (const task of downloaded) {
      const files = task.files.filter(f=>f.selected);
      assert.equal(files.length,1);
      const output=path.resolve(files[0].path);
      assert(output.toLowerCase().startsWith(allowed.toLowerCase()), 'download_outside_test_directory');
      assert.deepEqual(fs.readFileSync(output),payload);
      actualPaths.push(path.relative(allowed,output));
    }
    assert.deepEqual(actualPaths.sort(), [path.join(name,'renamed.bin'),path.join(name,'nested','renamed.bin')].sort());
    const direct=await invoke('cloud_download_url',{provider:'xunlei',fileId:id});
    assert.equal(direct.duplicate,'completed_files_present');
    console.log('PASS real aria2 recursive folder download: two files, exact byte equality; duplicate detection');
  } finally {
    if (root?.id && root.name===name) {
      await invoke('cloud_trash',{provider:'xunlei',fileIds:[root.id]});
      const after=await invoke('cloud_list_directory',{provider:'xunlei',parentId:'0'});
      assert(!after.items.some(f=>f.id===root.id));
      console.log('PASS only this test cloud folder moved to recycle bin');
    }
    // local is the exact mkdtemp result, not a caller-provided directory.
    fs.rmSync(local,{recursive:true});
  }
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
