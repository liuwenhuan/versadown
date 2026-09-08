import { useEffect, useState } from 'react';
import { FloatingLauncher } from './FloatingLauncher';
import { anchorLauncherToWorkArea, sendLauncherInput, setLauncherCompactShape, setLauncherExpanded, showLauncherMenu, startLauncherDragging } from '../lib/launcherWindow';
import { isTauriRuntime } from '../lib/api';
import { formatCompactSpeed, subscribeToDownloadEvents } from '../lib/downloadEvents';

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [expanded, setExpanded] = useState(false);
  useEffect(() => {
    void anchorLauncherToWorkArea().then(() => setLauncherCompactShape(true));
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    subscribeToDownloadEvents({
      onTask: () => undefined,
      onGlobalStat: (stat) => { if (!disposed) { setActiveCount(stat.num_active); setDownloadSpeed(stat.download_speed); } }
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
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
    </main>
  );
}
