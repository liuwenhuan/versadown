import { readFileSync } from 'node:fs';
import { render } from '@testing-library/react';
import { expect, it } from 'vitest';
import { DownloadQueue } from './DownloadQueue';

it('keeps queue tasks compact while preserving a generous row hit area', () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const { container } = render(
      <DownloadQueue
        tasks={[{ id: 'compact-task', name: 'compact-task', kind: 'file', status: 'queued', progress: 0 }]}
        selectedTaskId={null}
        onSelect={() => {}}
        onCommand={() => {}}
      />,
    );

    const row = container.querySelector('.task-row');
    const title = container.querySelector('.task-name b');
    const action = container.querySelector('.task-actions button');

    expect(getComputedStyle(row!).minHeight).toBe('72px');
    expect(getComputedStyle(row!).paddingTop).toBe('8px');
    expect(getComputedStyle(row!).paddingBottom).toBe('8px');
    expect(getComputedStyle(title!).fontSize).toBe('13px');
    expect(getComputedStyle(action!).width).toBe('34px');
    expect(getComputedStyle(action!).height).toBe('34px');
  } finally {
    stylesheet.remove();
  }
});

it('uses text-like batch actions and a denser uninterrupted task list', () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const fixture = document.createElement('div');
    fixture.innerHTML = `
      <button class="bulk-pause">全部暂停</button>
      <section class="download-queue">
        <div class="task-group"><article class="task-row"><div class="task-name"><b>first</b></div></article></div>
        <div class="task-group"><article class="task-row"><div class="task-name"><b>second</b></div></article></div>
      </section>
    `;
    document.body.appendChild(fixture);

    const action = fixture.querySelector('.bulk-pause');
    const title = fixture.querySelector('.task-name b');
    const secondTaskGroup = fixture.querySelectorAll('.task-group')[1];

    expect(getComputedStyle(action!).borderTopWidth).toBe('0px');
    expect(getComputedStyle(action!).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(getComputedStyle(action!).minHeight).toBe('32px');
    expect(getComputedStyle(title!).fontSize).toBe('13px');
    expect(getComputedStyle(secondTaskGroup!).borderTopWidth).toBe('0px');
  } finally {
    stylesheet.remove();
  }
});

it('aligns the borderless sidebar toggle with the task sort control', () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const fixture = document.createElement('header');
    fixture.className = 'download-tabs';
    fixture.innerHTML = `
      <button class="sidebar-toggle" aria-label="收起侧边栏">toggle</button>
      <div class="task-sort-control"><button class="task-sort-trigger" aria-label="排序任务">sort</button></div>
    `;
    document.body.appendChild(fixture);

    const sidebarToggle = fixture.querySelector('.sidebar-toggle');
    const sortTrigger = fixture.querySelector('.task-sort-trigger');

    expect(getComputedStyle(sidebarToggle!).height).toBe('34px');
    expect(getComputedStyle(sidebarToggle!).alignSelf).toBe('center');
    expect(getComputedStyle(sidebarToggle!).borderTopWidth).toBe('0px');
    expect(getComputedStyle(sortTrigger!).height).toBe('34px');
  } finally {
    stylesheet.remove();
  }
});
