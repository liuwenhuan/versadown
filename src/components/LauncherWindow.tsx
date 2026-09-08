import { useEffect } from 'react';
import { FloatingLauncher } from './FloatingLauncher';
import { anchorLauncherToWorkArea, sendLauncherInput, setLauncherCompactShape, setLauncherExpanded, showLauncherMenu, startLauncherDragging } from '../lib/launcherWindow';
import { isTauriRuntime } from '../lib/api';
import { subscribeToDownloadEvents } from '../lib/downloadEvents';
import { useState } from 'react';

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  useEffect(() => {
    void anchorLauncherToWorkArea().then(() => setLauncherCompactShape(true));
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let unlisten: (() => void) | undefined;
    subscribeToDownloadEvents({
      onTask: () => undefined,
      onGlobalStat: (stat) => { setActiveCount(stat.num_active); setDownloadSpeed(stat.download_speed); }
    }).then((stop) => { unlisten = stop; }).catch(() => undefined);
    return () => { unlisten?.(); };
  }, []);

  return (
    <main className="launcher-shell" aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        activeCount={activeCount}
        onParse={(input) => { void sendLauncherInput(input); }}
        onExpandedChange={(expanded) => { void setLauncherExpanded(expanded); }}
        onDragStart={() => { void startLauncherDragging(); }}
        onContextMenu={() => { void showLauncherMenu(); }}
      />
      {downloadSpeed > 0 && <span className="launcher-live-speed">↓ {Math.round(downloadSpeed / 1024)} KB/s</span>}
    </main>
  );
}
