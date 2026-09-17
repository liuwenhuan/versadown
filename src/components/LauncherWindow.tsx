import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { FloatingLauncher, type LauncherPointerPosition } from './FloatingLauncher';
import { LauncherContextMenu, type LauncherMenuAction } from './LauncherContextMenu';
import { anchorLauncherToWorkArea, beginLauncherPointerDrag, getLauncherManagerVisible, getLauncherPopupPlacement, launcherIconRegion, moveLauncherPointerDrag, openLauncherManager, runLauncherMenuAction, sendLauncherInput, setLauncherInputRegions, setLauncherWindowVisible, showLauncherMenu, type LauncherInputRegion, type LauncherPointerDragSession, type LauncherPopupPlacement } from '../lib/launcherWindow';
import { getDownloadGlobalStat, getSettings, isTauriRuntime, scanDownloadInputs, updateSettings } from '../lib/api';
import { formatSpeed, subscribeToDownloadEvents } from '../lib/downloadEvents';
import type { DownloadGlobalStat, LauncherVisibility } from '../lib/types';

export type LauncherSurfaceMode = 'compact' | 'quick-add' | 'context-menu';

export function LauncherWindow() {
  const [activeCount, setActiveCount] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [mode, setMode] = useState<LauncherSurfaceMode>('compact');
  const [popupPlacement, setPopupPlacement] = useState<LauncherPopupPlacement>('above');
  const popupPlacementRef = useRef<LauncherPopupPlacement>('above');
  const [managerVisible, setManagerVisible] = useState(true);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>('always');
  const [settingsReady, setSettingsReady] = useState(false);
  const [anchored, setAnchored] = useState(false);
  const statEventEpoch = useRef(0);
  const requestedMode = useRef<LauncherSurfaceMode>('compact');
  const transitionQueue = useRef(Promise.resolve());
  const regionQueue = useRef(Promise.resolve());
  const lastValidRegions = useRef<LauncherInputRegion[]>([launcherIconRegion]);
  const disposed = useRef(false);
  const quickAddElement = useRef<HTMLElement | null>(null);
  const menuElement = useRef<HTMLElement | null>(null);
  const [inputReady, setInputReady] = useState(false);
  const dragSession = useRef<LauncherPointerDragSession | null>(null);
  const dragPendingPointer = useRef<LauncherPointerPosition | null>(null);
  const dragStartQueue = useRef(Promise.resolve());
  const dragPositionQueue = useRef(Promise.resolve());
  const launcherDragging = useRef(false);
  popupPlacementRef.current = popupPlacement;
  const hasActiveDownload = activeCount > 0;
  const shouldShowLauncher = launcherVisibility === 'always' || (launcherVisibility === 'downloading' && hasActiveDownload);

  const captureQuickAdd = useCallback((element: HTMLElement | null) => { quickAddElement.current = element; }, []);
  const captureMenu = useCallback((element: HTMLElement | null) => { menuElement.current = element; }, []);

  const applyRegions = useCallback((regions: LauncherInputRegion[]) => {
    const update = regionQueue.current.then(async () => {
      if (disposed.current) return;
      await setLauncherInputRegions(regions);
      lastValidRegions.current = regions;
    });
    // Recover the queue, while propagating this request's failure to native dragging.
    regionQueue.current = update.catch(() => undefined);
    return update;
  }, []);

  const requestMode = useCallback((next: LauncherSurfaceMode) => {
    if (disposed.current) return;
    requestedMode.current = next;
    transitionQueue.current = transitionQueue.current.then(async () => {
      if (disposed.current) return;
      const target = requestedMode.current;
      const placement = target === 'compact' ? null : await getLauncherPopupPlacement();
      if (disposed.current || target !== requestedMode.current) return;
      if (placement) setPopupPlacement(placement);
      setMode(target);
    }).catch(() => undefined);
  }, []);

  const refreshLauncherGeometry = useCallback(async () => {
    const regions = lastValidRegions.current;
    try {
      await applyRegions(regions);
    } catch {
      // The native adapter retains the previous safe regions on failure.
    }
    const placement = await getLauncherPopupPlacement().catch(() => popupPlacementRef.current);
    if (!disposed.current) setPopupPlacement(placement);
  }, [applyRegions]);

  useLayoutEffect(() => {
    let disposed = false;
    const element = mode === 'quick-add' ? quickAddElement.current : mode === 'context-menu' ? menuElement.current : null;
    const syncInput = () => {
      const rect = element?.getBoundingClientRect();
      const popup = rect && rect.width > 0 && rect.height > 0
        ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null;
      const update = applyRegions(popup ? [launcherIconRegion, popup] : [launcherIconRegion]);
      void update.then(() => { if (!disposed) setInputReady(true); }).catch(() => undefined);
      return update;
    };
    const update = syncInput();
    // Quick-add validation and menu settings can change the actual input rectangle.
    const observer = element && typeof ResizeObserver !== 'undefined' ? new ResizeObserver(syncInput) : null;
    if (element) observer?.observe(element);
    return () => { disposed = true; observer?.disconnect(); };
  }, [applyRegions, mode, popupPlacement]);

  useEffect(() => {
    disposed.current = false;
    return () => { disposed.current = true; };
  }, []);

  useEffect(() => {
    const close = () => requestMode('compact');
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') close(); };
    window.addEventListener('blur', close);
    window.addEventListener('keydown', escape);
    return () => { window.removeEventListener('blur', close); window.removeEventListener('keydown', escape); };
  }, [requestMode]);

  useEffect(() => {
    void anchorLauncherToWorkArea()
      .then(async () => { if (!disposed.current) await refreshLauncherGeometry(); })
      .catch(() => undefined)
      .finally(() => { if (!disposed.current) setAnchored(true); });
  }, [refreshLauncherGeometry]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let cleanedUp = false;
    const unlisten: (() => void)[] = [];
    const refresh = () => { if (!launcherDragging.current) void refreshLauncherGeometry(); };
    const launcher = getCurrentWindow();
    void launcher.onMoved(refresh).then((stop) => {
      if (cleanedUp) stop(); else unlisten.push(stop);
    }).catch(() => undefined);
    void launcher.onScaleChanged(refresh).then((stop) => {
      if (cleanedUp) stop(); else unlisten.push(stop);
    }).catch(() => undefined);
    return () => {
      cleanedUp = true;
      unlisten.forEach((stop) => stop());
    };
  }, [refreshLauncherGeometry]);

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

  function queueLauncherDragMove(pointer: LauncherPointerPosition) {
    dragPendingPointer.current = pointer;
    if (!dragSession.current) return;
    dragPositionQueue.current = dragPositionQueue.current.then(async () => {
      const next = dragPendingPointer.current;
      dragPendingPointer.current = null;
      const session = dragSession.current;
      if (next && session) await moveLauncherPointerDrag(session, next);
    }).catch(() => undefined);
  }

  async function beginLauncherDrag(pointer: LauncherPointerPosition) {
    if (disposed.current) return;
    launcherDragging.current = true;
    requestedMode.current = 'compact';
    setMode('compact');
    try {
      await applyRegions([launcherIconRegion]);
      if (disposed.current || !launcherDragging.current) return;
      dragSession.current = await beginLauncherPointerDrag(pointer);
      if (disposed.current || !launcherDragging.current) return;
      const pendingPointer = dragPendingPointer.current;
      if (pendingPointer) queueLauncherDragMove(pendingPointer);
    } catch {
      launcherDragging.current = false;
      dragSession.current = null;
      // Never begin pointer-driven dragging after the compact-region update fails.
    }
  }

  function moveLauncherDrag(pointer: LauncherPointerPosition) {
    if (launcherDragging.current) queueLauncherDragMove(pointer);
  }

  function finishLauncherDrag(pointer: LauncherPointerPosition) {
    if (!launcherDragging.current) return;
    queueLauncherDragMove(pointer);
    // A fast pointer-up can arrive while the safe icon-only region is still
    // being applied.  Wait for that start sequence to install its session,
    // then flush the final captured pointer position before refreshing.
    void dragStartQueue.current.then(() => dragPositionQueue.current).finally(async () => {
      launcherDragging.current = false;
      dragSession.current = null;
      dragPendingPointer.current = null;
      if (!disposed.current) await refreshLauncherGeometry();
    });
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
        onDragStart={(pointer) => { dragStartQueue.current = beginLauncherDrag(pointer); }}
        onDragMove={moveLauncherDrag}
        onDragEnd={finishLauncherDrag}
        onContextMenu={openContextMenu}
      />
      {mode === 'context-menu' && <LauncherContextMenu onElement={captureMenu} downloadSpeed={formatSpeed(downloadSpeed)} managerVisible={managerVisible} visibility={launcherVisibility} onClose={closeContextMenu} onAction={runContextAction} onVisibilityChange={updateLauncherVisibility} />}
    </main>
  );
}
