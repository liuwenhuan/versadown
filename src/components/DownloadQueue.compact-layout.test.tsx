import { readFileSync } from 'node:fs';
import { fireEvent, render } from '@testing-library/react';
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

it('encloses an expanded task row and its file list in one rounded task card', () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const { container, getByRole } = render(
      <DownloadQueue
        tasks={[{
          id: 'directory', name: '目录任务', kind: 'folder', status: 'downloading', progress: 25,
          children: [{ id: 'child', name: 'file.zip', kind: 'file', status: 'downloading', progress: 25 }],
        }]}
        selectedTaskId={null}
        onSelect={() => {}}
        onCommand={() => {}}
      />,
    );

    fireEvent.click(getByRole('button', { name: '展开 目录任务' }));

    const group = container.querySelector('.task-group.is-expanded');
    const children = group?.querySelector('.children');
    const row = group?.querySelector('.task-row');
    expect(group).not.toBeNull();
    expect(children).not.toBeNull();
    expect(row).not.toBeNull();

    const styles = readFileSync('src/styles.css', 'utf8');
    expect(styles).toContain('.task-group.is-expanded{margin-bottom:10px;overflow:hidden;border:1px solid var(--border);border-radius:12px;');
    expect(styles).toContain('.task-group.is-expanded>.task-row{border-radius:11px 11px 0 0}');
    expect(styles).toContain('.task-group.is-expanded>.children{margin:0;border:0;border-top:1px solid var(--border);border-radius:0}');
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

it('uses stronger text weight for download task tabs', () => {
  const stylesheet = readFileSync('src/styles.css', 'utf8');

  expect(stylesheet).toMatch(/\.workspace-tab-list button\{[^}]*font-weight:700/);
  expect(stylesheet).toMatch(/\.workspace-tab-list button\[aria-selected="true"\]\{[^}]*font-weight:800/);
});
