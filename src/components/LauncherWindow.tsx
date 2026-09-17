import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { FloatingLauncher } from './FloatingLauncher';
import { anchorLauncherToWorkArea, sendLauncherInput, setLauncherWindowVisible, showLauncherPopup, startLauncherDragging } from '../lib/launcherWindow';
import { getDownloadGlobalStat, getSettings, isTauriRuntime, scanDownloadInputs } from '../lib/api';
import { subscribeToDownloadEvents } from '../lib/downloadEvents';
import type { DownloadGlobalStat, LauncherVisibility } from '../lib/types';

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>('always');
  const [settingsReady, setSettingsReady] = useState(false);
  const [anchored, setAnchored] = useState(false);
  const statEventEpoch = useRef(0);
  const shouldShowLauncher = launcherVisibility === 'always' || (launcherVisibility === 'downloading' && activeCount > 0);

  useEffect(() => {
    void anchorLauncherToWorkArea().catch(() => undefined).finally(() => setAnchored(true));
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) { setSettingsReady(true); return; }
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getSettings().then((settings) => {
      if (!disposed) setLauncherVisibility(settings.launcherVisibility);
    }).catch(() => undefined).finally(() => { if (!disposed) setSettingsReady(true); });
    listen<LauncherVisibility>('launcher-visibility-updated', (event) => {
      if (!disposed) setLauncherVisibility(event.payload);
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!settingsReady || !anchored) return;
    void setLauncherWindowVisible(shouldShowLauncher).catch(() => undefined);
  }, [anchored, settingsReady, shouldShowLauncher]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlistenDownloads: (() => void) | undefined;
    const applyStat = (stat: DownloadGlobalStat) => setActiveCount(stat.num_active);
    subscribeToDownloadEvents({
      onTask: () => undefined,
      onGlobalStat: (stat) => {
        statEventEpoch.current += 1;
        if (!disposed) applyStat(stat);
      }
    }).then(async (stop) => {
      if (disposed) { stop(); return; }
      unlistenDownloads = stop;
      const statEpoch = statEventEpoch.current;
      void getDownloadGlobalStat().then((stat) => {
        if (!disposed && statEventEpoch.current === statEpoch) applyStat(stat);
      }).catch(() => undefined);
    }).catch(() => undefined);
    return () => { disposed = true; unlistenDownloads?.(); };
  }, []);

  return (
    <main className="launcher-shell" aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        onParse={(input) => { void sendLauncherInput(input); }}
        onScan={scanDownloadInputs}
        onOpenInput={() => { void showLauncherPopup('input').catch(() => undefined); }}
        onDragStart={() => { void startLauncherDragging(); }}
        onContextMenu={() => { void showLauncherPopup('menu').catch(() => undefined); }}
      />
    </main>
  );
}
