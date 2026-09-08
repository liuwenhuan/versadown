import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { LauncherWindow } from './components/LauncherWindow';
import { isLauncherWindow } from './lib/launcherWindow';
import './styles.css';

const launcherWindow = isLauncherWindow();
if (launcherWindow) document.documentElement.classList.add('launcher-window');

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>{launcherWindow ? <LauncherWindow /> : <App />}</React.StrictMode>
);
