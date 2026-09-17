import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { LauncherPopupWindow } from './components/LauncherPopupWindow';
import { LauncherWindow } from './components/LauncherWindow';
import { isLauncherPopupWindow, isLauncherWindow } from './lib/launcherWindow';
import './styles.css';

const launcherWindow = isLauncherWindow();
const launcherPopupWindow = isLauncherPopupWindow();
if (launcherWindow || launcherPopupWindow) document.documentElement.classList.add('launcher-window');
if (launcherPopupWindow) document.documentElement.classList.add('launcher-popup-window');

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>{launcherWindow ? <LauncherWindow /> : launcherPopupWindow ? <LauncherPopupWindow /> : <App />}</React.StrictMode>
);
