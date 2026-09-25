import { PhysicalSize } from '@tauri-apps/api/window';

export const mainWindowViewport = { width: 960, height: 640 } as const;

type MainWindowSizer = {
  setMinSize(size: PhysicalSize): Promise<void>;
  setSize(size: PhysicalSize): Promise<void>;
};

function webViewScaleFactor() {
  return typeof window !== 'undefined'
    && Number.isFinite(window.devicePixelRatio)
    && window.devicePixelRatio > 0
    ? window.devicePixelRatio
    : 1;
}

export async function applyMainWindowDpiSizing(
  mainWindow: MainWindowSizer,
  nativeScaleFactor = 1,
) {
  // On fractional-scale X11 desktops Tao can report an unscaled monitor
  // factor while WebKitGTK renders CSS using devicePixelRatio. Keeping the
  // larger valid value preserves the 960x640 CSS viewport on every platform.
  const scaleFactor = Math.max(nativeScaleFactor, webViewScaleFactor(), 1);
  const physicalSize = new PhysicalSize(
    Math.round(mainWindowViewport.width * scaleFactor),
    Math.round(mainWindowViewport.height * scaleFactor),
  );
  await mainWindow.setMinSize(physicalSize);
  await mainWindow.setSize(physicalSize);
}
