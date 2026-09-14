// Run after npm run build; requires Playwright (or a runtime providing NODE_PATH).
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const http = require('node:http');
const path = require('node:path');
const { chromium } = require('playwright');

async function main() {
  const dist = path.resolve(__dirname, '../dist');
  const server = http.createServer(async (req, res) => {
    try {
      const target = path.resolve(dist, '.' + (req.url === '/' ? '/index.html' : req.url.split('?')[0]));
      if (!target.startsWith(dist + path.sep)) { res.writeHead(403).end(); return; }
      res.setHeader('Content-Type', ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' })[path.extname(target)] || 'application/octet-stream');
      res.end(await fs.readFile(target));
    } catch { res.writeHead(404).end(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  let browser;
  try {
    browser = await chromium.launch({ channel: 'msedge', headless: true });
    const context = await browser.newContext({ permissions: ['clipboard-read', 'clipboard-write'] });
    const page = await context.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    const input = page.getByRole('textbox', { name: '搜索任务或粘贴下载链接' });
    const link = 'https://github.com/AI-Shell-Team/aish/releases/download/v0.3.13/aish-0.3.13-linux-amd64.tar.gz';
    for (const width of [960, 1280]) {
      await page.setViewportSize({ width, height: 720 });
      await input.fill('');
      await page.evaluate(text => navigator.clipboard.writeText(text), link);
      await input.focus();
      await page.keyboard.press('Control+V');
      await page.waitForFunction(text => document.querySelector('.global-search textarea').value === text, link);
      const metrics = await input.evaluate(element => ({
        value: element.value, height: element.clientHeight,
        contentHeight: element.scrollHeight, top: element.scrollTop,
      }));
      console.log(JSON.stringify({ width, ...metrics }));
      assert.equal(metrics.value, link, 'paste must retain the entire URL');
      assert.ok(metrics.contentHeight <= metrics.height + 1, 'one-line URL must not wrap into hidden lines');
      assert.equal(metrics.top, 0, 'paste must not vertically scroll to just the final line');
      await page.keyboard.press('Home');
      assert.equal(await input.evaluate(element => element.scrollLeft), 0, 'Home exposes the URL beginning');
      await page.keyboard.press('Control+A');
      await page.keyboard.press('Control+C');
      assert.equal(await page.evaluate(() => navigator.clipboard.readText()), link, 'copy must include the whole URL');
    }
  } finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
