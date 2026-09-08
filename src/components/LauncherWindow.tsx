import { FloatingLauncher } from './FloatingLauncher';
import { sendLauncherInput, setLauncherExpanded } from '../lib/launcherWindow';

export function LauncherWindow() {
  return (
    <main className="launcher-shell" aria-label="AllDownloader 悬浮下载窗口">
      <FloatingLauncher
        activeCount={0}
        onParse={(input) => { void sendLauncherInput(input); }}
        onExpandedChange={(expanded) => { void setLauncherExpanded(expanded); }}
      />
    </main>
  );
}
