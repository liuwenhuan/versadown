export function App() {
  return (
    <main className="app-shell">
      <header className="app-header">
        <div className="brand-mark" aria-hidden="true">↓</div>
        <div>
          <p className="eyebrow">ALLDOWNLOADER</p>
          <h1>下载管理</h1>
        </div>
      </header>
      <section className="empty-state" aria-label="下载队列">
        <strong>准备就绪</strong>
        <span>使用右下角悬浮窗粘贴下载链接或文本。</span>
      </section>
    </main>
  );
}
