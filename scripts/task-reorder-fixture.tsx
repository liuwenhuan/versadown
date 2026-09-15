import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { DownloadQueue } from '../src/components/DownloadQueue';
import type { QueueTask } from '../src/lib/types';
import '../src/styles.css';

const initialTasks: QueueTask[] = [
  { id: 'three', name: 'three.zip', kind: 'file', status: 'paused', progress: 0 },
  { id: 'one', name: 'one.zip', kind: 'file', status: 'paused', progress: 0 },
  { id: 'two', name: 'two.zip', kind: 'file', status: 'paused', progress: 0 },
];

function TaskReorderFixture() {
  const [tasks, setTasks] = useState(initialTasks);
  return <DownloadQueue
    tasks={tasks}
    selectedTaskId={null}
    onSelect={() => undefined}
    onCommand={() => undefined}
    onReorder={(orderedIds) => setTasks(orderedIds.flatMap((id) => {
      const task = tasks.find((item) => item.id === id);
      return task ? [task] : [];
    }))}
  />;
}

createRoot(document.getElementById('root')!).render(<TaskReorderFixture />);
