import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { FloatingLauncher } from './FloatingLauncher';
import { LauncherContextMenu, type LauncherMenuAction } from './LauncherContextMenu';
import { anchorLauncherToWorkArea, getLauncherManagerVisible, getLauncherPopupPlacement, launcherIconRegion, openLauncherManager, runLauncherMenuAction, sendLauncherInput, setLauncherInputRegions, setLauncherWindowVisible, showLauncherMenu, startLauncherDragging, type LauncherInputRegion, type LauncherPopupPlacement } from '../lib/launcherWindow';
import { getDownloadGlobalStat, getSettings, isTauriRuntime, scanDownloadInputs, updateSettings } from '../lib/api';
import { formatSpeed, subscribeToDownloadEvents } from '../lib/downloadEvents';
import type { DownloadGlobalStat, LauncherVisibility } from '../lib/types';

export type LauncherSurfaceMode = 'compact' | 'quick-add' | 'context-menu';

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [mode, setMode] = useState<LauncherSurfaceMode>('compact');
  const [popupPlacement, setPopupPlacement] = useState<LauncherPopupPlacement>('above');
  const [managerVisible, setManagerVisible] = useState(true);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>('always');
  const [settingsReady, setSettingsReady] = useState(false);
  const [anchored, setAnchored] = useState(false);
  const statEventEpoch = useRef(0);
  const requestedMode = useRef<LauncherSurfaceMode>('compact');
  const transitionQueue = useRef(Promise.resolve());
  const inputQueue = useRef(Promise.resolve());
  const quickAddElement = useRef<HTMLElement | null>(null);
  const menuElement = useRef<HTMLElement | null>(null);
  const dragRequested = useRef(false);
  const [inputReady, setInputReady] = useState(false);
  const hasActiveDownload = activeCount > 0;
  const shouldShowLauncher = launcherVisibility === 'always' || (launcherVisibility === 'downloading' && hasActiveDownload);

  const captureQuickAdd = useCallback((element: HTMLElement | null) => { quickAddElement.current = element; }, []);
  const captureMenu = useCallback((element: HTMLElement | null) => { menuElement.current = element; }, []);

  const updateInput = useCallback((regions: LauncherInputRegion[]) => {
    const update = inputQueue.current.then(() => setLauncherInputRegions(regions));
    // Recover the queue, but propagate this update's failure to callers such as dragging.
    inputQueue.current = update.catch(() => undefined);
    return update;
  }, []);

  const requestMode = useCallback((next: LauncherSurfaceMode) => {
    requestedMode.current = next;
    if (next !== 'compact') dragRequested.current = false;
    transitionQueue.current = transitionQueue.current.then(async () => {
      const target = requestedMode.current;
      const placement = target === 'compact' ? null : await getLauncherPopupPlacement();
      if (target !== requestedMode.current) return;
      if (placement) setPopupPlacement(placement);
      setMode(target);
    }).catch(() => undefined);
  }, []);

  async function finishDrag(inputUpdate: Promise<void>) {
    try {
      await inputUpdate;
      if (!dragRequested.current || requestedMode.current !== 'compact') return;
      dragRequested.current = false;
      await startLauncherDragging();
      const placement = await getLauncherPopupPlacement();
      if (requestedMode.current === 'compact') setPopupPlacement(placement);
    } catch {
      dragRequested.current = false;
      // Keep the previous safe native regions; never replace them with the canvas.
    }
  }

  useLayoutEffect(() => {
    let disposed = false;
    const element = mode === 'quick-add' ? quickAddElement.current : mode === 'context-menu' ? menuElement.current : null;
    const syncInput = () => {
      const rect = element?.getBoundingClientRect();
      const popup = rect && rect.width > 0 && rect.height > 0
        ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null;
      const update = updateInput(popup ? [launcherIconRegion, popup] : [launcherIconRegion]);
      void update.then(() => { if (!disposed) setInputReady(true); }).catch(() => undefined);
      return update;
    };
    const update = syncInput();
    if (mode === 'compact' && dragRequested.current) void finishDrag(update);
    // Quick-add validation and menu settings can change the actual input rectangle.
    const observer = element && typeof ResizeObserver !== 'undefined' ? new ResizeObserver(syncInput) : null;
    if (element) observer?.observe(element);
    return () => { disposed = true; observer?.disconnect(); };
  }, [mode, popupPlacement, updateInput]);

  useEffect(() => {
    const close = () => requestMode('compact');
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') close(); };
    window.addEventListener('blur', close);
    window.addEventListener('keydown', escape);
    return () => { window.removeEventListener('blur', close); window.removeEventListener('keydown', escape); };
  }, [requestMode]);

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
    if (!settingsReady || !anchored || !inputReady) return;
    void setLauncherWindowVisible(shouldShowLauncher).catch(() => undefined);
  }, [anchored, inputReady, settingsReady, shouldShowLauncher]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlistenDownloads: (() => void) | undefined;
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
      const statEpoch = statEventEpoch.current;
      void getDownloadGlobalStat().then((stat) => {
        if (!disposed && statEventEpoch.current === statEpoch) applyStat(stat);
      }).catch(() => undefined);
    }).catch(() => undefined);
    return () => { disposed = true; unlistenDownloads?.(); };
  }, []);

  function openContextMenu() {
    requestMode('context-menu');
    void getLauncherManagerVisible().then(setManagerVisible).catch(() => undefined);
    // Records menu usage only; this command never changes native visibility.
    void showLauncherMenu().catch(() => undefined);
  }

  function closeContextMenu() {
    requestMode('compact');
  }

  function beginDrag() {
    dragRequested.current = true;
    requestMode('compact');
    if (mode === 'compact') void finishDrag(updateInput([launcherIconRegion]));
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
    <main className={`launcher-shell ${mode === 'quick-add' ? 'launcher-expanded' : ''} ${mode === 'context-menu' ? 'launcher-menu-open' : ''} ${popupPlacement === 'below' ? 'launcher-popup-below' : ''}`} aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        mode={mode}
        placement={popupPlacement}
        onModeChange={requestMode}
        onQuickAddElement={captureQuickAdd}
        onParse={(input) => { void sendLauncherInput(input); }}
        onScan={scanDownloadInputs}
        onOpenTaskList={() => { void openLauncherManager(); }}
        onDragStart={beginDrag}
        onContextMenu={openContextMenu}
      />
      {mode === 'context-menu' && <LauncherContextMenu onElement={captureMenu} downloadSpeed={formatSpeed(downloadSpeed)} managerVisible={managerVisible} visibility={launcherVisibility} onClose={closeContextMenu} onAction={runContextAction} onVisibilityChange={updateLauncherVisibility} />}
    </main>
  );
}
