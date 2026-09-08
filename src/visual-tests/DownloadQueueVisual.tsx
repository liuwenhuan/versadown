import React from 'react';
import ReactDOM from 'react-dom/client';
import { DownloadQueue } from '../components/DownloadQueue';
import { QueueTask } from '../lib/types';
import '../styles.css';

const rawMagnet = 'magnet:?xt=urn:btih:46A00FD699618BC0319C45F7075C067D564A0819&dn=Lanterns.S01E04.1080p.HMAX.WEBRip.AAC5.1.10bits.x265-Rapta&tr=udp%3A%2F%2Ftracker.example';
const tasks: QueueTask[] = [
  {
    id: 'magnet-visual', name: rawMagnet, source: rawMagnet, kind: 'magnet', status: 'downloading', progress: 37,
    speed: '12.8 MB/s', size: '1.2 GB / 3.1 GB', eta: '2分31秒', connections: 18,
    children: [
      { id: 'episode-1', name: 'Lanterns.S01E04.1080p.HMAX.WEBRip.AAC5.1.10bits.x265-Rapta.mkv', kind: 'file', status: 'downloading', progress: 41, size: '1.2 GB / 2.9 GB' },
      { id: 'subtitle-1', name: 'Lanterns.S01E04.English.Forced.Subtitle.with.an.intentionally.long.file.name.ass', kind: 'file', status: 'completed', progress: 100, size: '218 KB / 218 KB' },
    ],
  },
  {
    id: 'http-visual', name: 'This-is-an-extremely-long-Windows-game-release-file-name-that-must-never-overlap-the-progress-or-controls.part01.rar',
    source: 'https://download.example.com/releases/game/part01.rar', kind: 'file', status: 'paused', progress: 68,
    speed: '0 B/s', size: '8.7 GB / 12.8 GB', connections: 0,
  },
];

ReactDOM.createRoot(document.getElementById('root')!).render(
  <div style={{ width: '100vw', minHeight: '100vh', padding: 40, background: 'var(--surface)' }}>
    <DownloadQueue tasks={tasks} selectedTaskId="magnet-visual" onSelect={() => {}} onCommand={() => {}} onConfirm={() => {}} />
  </div>,
);
