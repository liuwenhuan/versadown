import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { LauncherContextMenu, type LauncherMenuAction } from './LauncherContextMenu';
import { NewDownloadDialog } from './NewDownloadDialog';
import { getDownloadGlobalStat, getSettings, isTauriRuntime, scanDownloadInputs, updateSettings } from '../lib/api';
import { getLauncherManagerVisible, hideLauncherPopup, openLauncherManager, runLauncherMenuAction, sendLauncherInput } from '../lib/launcherWindow';
import { formatSpeed, subscribeToDownloadEvents } from '../lib/downloadEvents';
import type { DownloadGlobalStat, LauncherVisibility } from '../lib/types';
import type { LauncherPopupMode } from '../lib/launcherWindow';

interface LauncherPopupState {
  mode: LauncherPopupMode;
  placement: 'above' | 'below';
}

export function LauncherPopupWindow() {
  const [mode, setMode] = useState<LauncherPopupMode | null>(null);
  const [placement, setPlacement] = useState<'above' | 'below'>('above');
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [managerVisible, setManagerVisible] = useState(true);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>('always');
  const statEventEpoch = useRef(0);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<LauncherPopupState>('launcher-popup-state', (event) => {
      if (!disposed) {
        setMode(event.payload.mode);
        setPlacement(event.payload.placement);
      }
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getSettings().then((settings) => {
      if (!disposed) setLauncherVisibility(settings.launcherVisibility);
    }).catch(() => undefined);
    listen<LauncherVisibility>('launcher-visibility-updated', (event) => {
      if (!disposed) setLauncherVisibility(event.payload);
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlistenDownloads: (() => void) | undefined;
    const applyStat = (stat: DownloadGlobalStat) => setDownloadSpeed(stat.download_speed);
    subscribeToDownloadEvents({
      onTask: () => undefined,
      onGlobalStat: (stat) => {
        statEventEpoch.current += 1;
        if (!disposed) applyStat(stat);
      }
    }).then((stop) => {
      if (disposed) { stop(); return; }
      unlistenDownloads = stop;
      const statEpoch = statEventEpoch.current;
      void getDownloadGlobalStat().then((stat) => {
        if (!disposed && statEventEpoch.current === statEpoch) applyStat(stat);
      }).catch(() => undefined);
    }).catch(() => undefined);
    return () => { disposed = true; unlistenDownloads?.(); };
  }, []);

  useEffect(() => {
    if (mode !== 'menu') return;
    void getLauncherManagerVisible().then(setManagerVisible).catch(() => undefined);
  }, [mode]);

  useEffect(() => {
    if (mode !== 'input') return;
    const hideForWindowBlur = () => closePopup();
    window.addEventListener('blur', hideForWindowBlur);
    return () => window.removeEventListener('blur', hideForWindowBlur);
  }, [mode]);

  function closePopup() {
    setMode(null);
    void hideLauncherPopup().catch(() => undefined);
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

  return <main className={`launcher-popup-shell ${mode === 'menu' ? 'launcher-popup-menu' : 'launcher-popup-input'} ${placement === 'below' ? 'launcher-popup-below' : ''}`} aria-label="AllDownloader 悬浮弹窗">
    {mode === 'input' && <NewDownloadDialog open placement="launcher" onParse={sendLauncherInput} onScan={scanDownloadInputs} onClose={closePopup} onOpenTaskList={openLauncherManager} />}
    {mode === 'menu' && <LauncherContextMenu downloadSpeed={formatSpeed(downloadSpeed)} managerVisible={managerVisible} visibility={launcherVisibility} onClose={closePopup} onAction={runContextAction} onVisibilityChange={updateLauncherVisibility} />}
  </main>;
}
