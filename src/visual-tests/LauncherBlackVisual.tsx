import ReactDOM from 'react-dom/client';
import { FloatingLauncher } from '../components/FloatingLauncher';
import { setThemeMode } from '../lib/theme';
import '../styles.css';

setThemeMode('mint');
document.documentElement.style.background = '#000';
document.body.style.background = '#000';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <main className="launcher-shell" style={{ position: 'absolute', top: 96, left: 96, width: 64, height: 64 }}>
    <FloatingLauncher onParse={() => undefined} />
  </main>,
);
