import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { FloatingLauncher } from './FloatingLauncher';
import { LauncherContextMenu, type LauncherMenuAction } from './LauncherContextMenu';
import { anchorLauncherToWorkArea, getLauncherManagerVisible, openLauncherManager, runLauncherMenuAction, sendLauncherInput, setLauncherContextMenuVisible, setLauncherExpanded, setLauncherLiveSpeedVisible, setLauncherWindowVisible, showLauncherMenu, startLauncherDragging } from '../lib/launcherWindow';
import { getDownloadEngineState, getDownloadGlobalStat, getSettings, isTauriRuntime, updateSettings } from '../lib/api';
import { formatCompactSpeed, subscribeToDownloadEvents } from '../lib/downloadEvents';
import type { DownloadEngineState, DownloadGlobalStat, LauncherVisibility } from '../lib/types';

function waitForLauncherTransitionPaint() {
  return new Promise<void>((resolve) => {
    if (typeof window.requestAnimationFrame !== 'function') {
      window.setTimeout(resolve, 0);
      return;
    }
    window.requestAnimationFrame(() => window.setTimeout(resolve, 0));
  });
}

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [engineAvailable, setEngineAvailable] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [expandedTransitioning, setExpandedTransitioning] = useState(false);
  const [contextMenuOpen, setContextMenuOpen] = useState(false);
  const [contextMenuTransitioning, setContextMenuTransitioning] = useState(false);
  const [managerVisible, setManagerVisible] = useState(true);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>('always');
  const [settingsReady, setSettingsReady] = useState(false);
  const [anchored, setAnchored] = useState(false);
  const statEventEpoch = useRef(0);
  const engineEventEpoch = useRef(0);
  const hasActiveDownload = activeCount > 0;
  const shouldShowLauncher = launcherVisibility === 'always' || (launcherVisibility === 'downloading' && hasActiveDownload);
  const liveSpeedVisible = shouldShowLauncher && hasActiveDownload && engineAvailable;

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
    if (!settingsReady || !anchored || expanded || expandedTransitioning || contextMenuOpen || contextMenuTransitioning) return;
    void setLauncherLiveSpeedVisible(liveSpeedVisible)
      .then(() => setLauncherWindowVisible(shouldShowLauncher))
      .catch(() => undefined);
  }, [anchored, contextMenuOpen, contextMenuTransitioning, expanded, expandedTransitioning, liveSpeedVisible, settingsReady, shouldShowLauncher]);

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

  function openContextMenu() {
    if (expanded || contextMenuOpen || contextMenuTransitioning) return;
    setContextMenuTransitioning(true);
    void (async () => {
      try {
        await waitForLauncherTransitionPaint();
        await setLauncherContextMenuVisible(true, liveSpeedVisible);
        setContextMenuOpen(true);
        void getLauncherManagerVisible().then(setManagerVisible).catch(() => undefined);
        void showLauncherMenu().catch(() => undefined);
      } catch {
        // The transparent native viewport was not prepared, so do not render a clipped menu.
      } finally {
        setContextMenuTransitioning(false);
      }
    })();
  }

  function closeContextMenu() {
    if (!contextMenuOpen || contextMenuTransitioning) return;
    setContextMenuTransitioning(true);
    setContextMenuOpen(false);
    void waitForLauncherTransitionPaint()
      .then(() => setLauncherContextMenuVisible(false, liveSpeedVisible))
      .catch(() => undefined)
      .finally(() => setContextMenuTransitioning(false));
  }

  function changeExpanded(nextExpanded: boolean) {
    setContextMenuOpen(false);
    setExpanded(nextExpanded);
    setExpandedTransitioning(true);
    void waitForLauncherTransitionPaint()
      .then(() => setLauncherExpanded(nextExpanded, liveSpeedVisible))
      .catch(() => undefined)
      .finally(() => setExpandedTransitioning(false));
  }

  function runContextAction(action: LauncherMenuAction) {
    void runLauncherMenuAction(`launcher.${action}`).then(async () => {
      if (action === 'hide_manager') setManagerVisible(await getLauncherManagerVisible().catch(() => managerVisible));
    }).catch(() => undefined);
  }

  function updateLauncherVisibility(nextVisibility: LauncherVisibility) {
    void getSettings()
      .then((settings) => updateSettings({ ...settings, launcherVisibility: nextVisibility }))
      .then((settings) => setLauncherVisibility(settings.launcherVisibility))
      .catch(() => undefined);
  }

  return (
    <main className={`launcher-shell ${liveSpeedVisible ? 'launcher-has-live-speed' : ''} ${liveSpeedVisible && !expanded ? 'launcher-speed-active' : ''} ${expanded ? 'launcher-expanded' : ''} ${contextMenuOpen ? 'launcher-menu-open' : ''} ${expandedTransitioning || contextMenuTransitioning ? 'launcher-viewport-transitioning' : ''}`} aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        liveSpeed={liveSpeedVisible && !expanded ? { label: `下载速度 ${Math.round(downloadSpeed / 1024)} KB/s`, value: formatCompactSpeed(downloadSpeed) } : undefined}
        onParse={(input) => { void sendLauncherInput(input); }}
        onOpenTaskList={() => { void openLauncherManager(); }}
        onExpandedChange={changeExpanded}
        onDragStart={() => { void startLauncherDragging(); }}
        onContextMenu={openContextMenu}
      />
      {liveSpeedVisible && expanded && <span className="launcher-live-speed">↓ {Math.round(downloadSpeed / 1024)} KB/s</span>}
      {contextMenuOpen && <LauncherContextMenu managerVisible={managerVisible} visibility={launcherVisibility} onClose={closeContextMenu} onAction={runContextAction} onVisibilityChange={updateLauncherVisibility} />}
    </main>
  );
}
