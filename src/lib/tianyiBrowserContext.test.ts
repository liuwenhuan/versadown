import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { expect, it } from 'vitest';

it('captures the official Browser-Id without changing its request or reading other headers', () => {
  const dom = new JSDOM('', { url: 'https://cloud.189.cn/web/main', runScripts: 'outside-only' });
  try {
    const sent: string[][] = [];
    dom.window.XMLHttpRequest.prototype.open = function () {};
    dom.window.XMLHttpRequest.prototype.setRequestHeader = function (name, value) { sent.push([name, value]); };
    dom.window.eval(readFileSync('src-tauri/src/cloud/tianyi-browser-context.js', 'utf8'));
    const xhr = new dom.window.XMLHttpRequest();
    xhr.open('GET', '/api/open/user/getUserInfoForPortal.action');
    xhr.setRequestHeader('Browser-Id', '1234567890abcdef1234567890abcdef');
    xhr.setRequestHeader('Authorization', 'must-not-be-captured');
    const context = dom.window.__versadownTianyiContext;
    expect(context.browser_id).toBe('1234567890abcdef1234567890abcdef');
    expect(context.user_agent).toBe(dom.window.navigator.userAgent);
    expect(JSON.stringify(context)).not.toContain('must-not-be-captured');
    expect(sent).toEqual([['Browser-Id', '1234567890abcdef1234567890abcdef'], ['Authorization', 'must-not-be-captured']]);
    xhr.open('GET', 'https://example.org/api');
    xhr.setRequestHeader('Browser-Id', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
    expect(dom.window.__versadownTianyiContext.browser_id).toBe('1234567890abcdef1234567890abcdef');
  } finally { dom.window.close(); }
});

it('is idempotent and ignores invalid IDs and non-API destinations', () => {
  const dom = new JSDOM('', { url: 'https://cloud.189.cn/web/main', runScripts: 'outside-only' });
  try {
    let forwarded = 0;
    dom.window.XMLHttpRequest.prototype.open = function () {};
    dom.window.XMLHttpRequest.prototype.setRequestHeader = function () { forwarded++; };
    const script = readFileSync('src-tauri/src/cloud/tianyi-browser-context.js', 'utf8');
    dom.window.eval(script);
    const hook = dom.window.XMLHttpRequest.prototype.setRequestHeader;
    dom.window.eval(script);
    expect(dom.window.XMLHttpRequest.prototype.setRequestHeader).toBe(hook);
    const xhr = new dom.window.XMLHttpRequest();
    xhr.open('GET', '/api/open/user/getUserInfoForPortal.action');
    xhr.setRequestHeader('browser-id', 'not-an-official-id');
    expect(dom.window.__versadownTianyiContext).toBeUndefined();
    for (const url of ['/web/main', 'http://cloud.189.cn/api/test', 'https://cloud.189.cn:444/api/test']) {
      xhr.open('GET', url);
      xhr.setRequestHeader('browser-id', '1234567890abcdef1234567890abcdef');
      expect(dom.window.__versadownTianyiContext).toBeUndefined();
    }
    xhr.open('GET', '/api/open/user/getUserInfoForPortal.action');
    xhr.setRequestHeader('browser-id', '1234567890abcdef1234567890abcdef');
    expect(dom.window.__versadownTianyiContext.browser_id).toBe('1234567890abcdef1234567890abcdef');
    expect(forwarded).toBe(5);
  } finally { dom.window.close(); }
});

it('does not install a capture hook on another website', () => {
  const dom = new JSDOM('', { url: 'https://cloud.189.cn.example.org', runScripts: 'outside-only' });
  try {
    const original = dom.window.XMLHttpRequest.prototype.setRequestHeader;
    dom.window.eval(readFileSync('src-tauri/src/cloud/tianyi-browser-context.js', 'utf8'));
    expect(dom.window.XMLHttpRequest.prototype.setRequestHeader).toBe(original);
    expect(dom.window.__versadownTianyiContext).toBeUndefined();
  } finally { dom.window.close(); }
});
