// Read-only warm-start regression against an explicitly authorized local app.
const { evaluate, targets } = require('./xunlei-live-probe.cjs');
async function main() {
  if (process.env.ALLDOWNLOADER_LIVE_CLOUD !== '1') throw Error('explicit_live_opt_in_required');
  const port = Number(process.argv[2]);
  for (let attempt=0; attempt<80; attempt++) {
    for (const app of (await targets(port).catch(()=>[])).filter(t=>/tauri.localhost/.test(t.url))) {
      const state = await evaluate(app, `(()=>({ready:Array.from(document.querySelectorAll('[role="tab"]')).some(t=>t.textContent==='迅雷网盘'),interrupted:Array.from(document.querySelectorAll('.cloud-operation-status.error')).some(t=>t.textContent.includes('网盘任务中断'))}))()`);
      if (state?.interrupted) throw Error('cloud_startup_task_interrupted');
      if (state?.ready) {console.log('PASS warm startup restores Xunlei tab');return;}
    }
    await new Promise(resolve=>setTimeout(resolve,250));
  }
  throw Error('cloud_startup_restore_timeout');
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
