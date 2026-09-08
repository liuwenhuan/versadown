import { useEffect } from 'react';
import { FloatingLauncher } from './FloatingLauncher';
import { anchorLauncherToWorkArea, sendLauncherInput, setLauncherCompactShape, setLauncherExpanded, showLauncherMenu, startLauncherDragging } from '../lib/launcherWindow';

export function LauncherWindow() {
  useEffect(() => {
    void anchorLauncherToWorkArea().then(() => setLauncherCompactShape(true));
  }, []);

  return (
    <main className="launcher-shell" aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        activeCount={0}
        onParse={(input) => { void sendLauncherInput(input); }}
        onExpandedChange={(expanded) => { void setLauncherExpanded(expanded); }}
        onDragStart={() => { void startLauncherDragging(); }}
        onContextMenu={() => { void showLauncherMenu(); }}
      />
    </main>
  );
}
