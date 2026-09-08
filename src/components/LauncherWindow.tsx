import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { FloatingLauncher } from './FloatingLauncher';
import { anchorLauncherToWorkArea, sendLauncherInput, setLauncherCompactShape, setLauncherExpanded, showLauncherMenu, startLauncherDragging } from '../lib/launcherWindow';
import { getDownloadEngineState, getDownloadGlobalStat, isTauriRuntime } from '../lib/api';
import { formatCompactSpeed, subscribeToDownloadEvents } from '../lib/downloadEvents';
import type { DownloadEngineState, DownloadGlobalStat } from '../lib/types';

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [engineAvailable, setEngineAvailable] = useState(true);
  const [expanded, setExpanded] = useState(false);
  const statEventEpoch = useRef(0);
  const engineEventEpoch = useRef(0);
  useEffect(() => {
    void anchorLauncherToWorkArea().then(() => setLauncherCompactShape(true));
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlistenDownloads: (() => void) | undefined;
    let unlistenEngine: (() => void) | undefined;
    const applyStat = (stat: DownloadGlobalStat) => {
      setActiveCount(stat.num_active);
      setDownloadSpeed(stat.download_speed);
    };
    subscribeToDownloadEvents({
      onTask: () => undefined,
      onGlobalStat: (stat) => {
        statEventEpoch.current += 1;
        if (!disposed) applyStat(stat);
      }
    }).then(async (stop) => {
      if (disposed) { stop(); return; }
      unlistenDownloads = stop;
      unlistenEngine = await listen<DownloadEngineState>('download-engine-state-updated', (event) => {
        engineEventEpoch.current += 1;
        if (!disposed) setEngineAvailable(event.payload.available);
      });
      if (disposed) { unlistenEngine(); return; }
      const statEpoch = statEventEpoch.current;
      const engineEpoch = engineEventEpoch.current;
      void getDownloadGlobalStat().then((stat) => {
        if (!disposed && statEventEpoch.current === statEpoch) applyStat(stat);
      }).catch(() => undefined);
      void getDownloadEngineState().then((state) => {
        if (!disposed && engineEventEpoch.current === engineEpoch) setEngineAvailable(state.available);
      }).catch(() => { if (!disposed) setEngineAvailable(false); });
    }).catch(() => undefined);
    return () => { disposed = true; unlistenDownloads?.(); unlistenEngine?.(); };
  }, []);

  return (
    <main className="launcher-shell" aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        activeCount={activeCount}
        onParse={(input) => { void sendLauncherInput(input); }}
        onExpandedChange={(nextExpanded) => { setExpanded(nextExpanded); void setLauncherExpanded(nextExpanded); }}
        onDragStart={() => { void startLauncherDragging(); }}
        onContextMenu={() => { void showLauncherMenu(); }}
      />
      {downloadSpeed > 0 && (expanded
        ? <span className="launcher-live-speed">↓ {Math.round(downloadSpeed / 1024)} KB/s</span>
        : <span className="launcher-live-indicator" aria-label={`下载速度 ${Math.round(downloadSpeed / 1024)} KB/s`}>{formatCompactSpeed(downloadSpeed)}</span>)}
      {!engineAvailable && <span className="launcher-engine-offline" aria-label="下载引擎不可用">!</span>}
    </main>
  );
}
