import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { expect, it } from 'vitest';

const script = readFileSync('src-tauri/src/cloud/password-login.js', 'utf8');

function page(url: string, extra = '') {
  const dom = new JSDOM(`<form><input name="userName"><input type="password">${extra}<button type="button">登录</button></form>`, { url, runScripts: 'outside-only' });
  const ticks: (() => void)[] = [];
  let now = Date.now();
  dom.window.Date.now = () => now;
  dom.window.setInterval = ((callback: () => void) => { ticks.push(callback); return ticks.length; }) as typeof dom.window.setInterval;
  dom.window.clearInterval = () => {};
  dom.window.HTMLElement.prototype.getClientRects = function () {
    for (let element: HTMLElement | null = this; element; element = element.parentElement) {
      if (element.style.display === 'none') return [] as unknown as DOMRectList;
    }
    return [{}] as unknown as DOMRectList;
  };
  let submissions = 0;
  dom.window.document.querySelector('button')!.addEventListener('click', () => { submissions += 1; });
  const run = (provider: string, attempt = 'attempt-1') => dom.window.eval(script)(provider, 'provided-user', 'provided-password', attempt);
  return { dom, run, tick: () => ticks.forEach(tick => tick()), advance: (ms: number) => { now += ms; }, submissions: () => submissions };
}

it.each([
  ['baidu', 'https://passport.baidu.com/login'],
  ['tianyi', 'https://open.e.189.cn/login'],
])('automatically fills and submits the official %s password form once', (provider, url) => {
  const p = page(url);
  try {
    p.run(provider); p.tick();
    expect((p.dom.window.document.querySelector('[name="userName"]') as HTMLInputElement).value).toBe('provided-user');
    expect((p.dom.window.document.querySelector('[type="password"]') as HTMLInputElement).value).toBe('provided-password');
    expect(p.submissions()).toBe(1);
    p.run(provider); p.tick();
    expect(p.submissions()).toBe(1);
  } finally { p.dom.window.close(); }
});

// Both public login pages start in QR mode. The hidden password fields must
// not make the agent wait forever instead of opening the visible account tab.
it.each([
  ['baidu', 'https://pan.baidu.com', '<span id="TANGRAM__PSP_11__changePwdCodeItem">账号登录</span>'],
  ['tianyi', 'https://open.e.189.cn/login', '<li id="tab-pw"><span>账号登录</span></li>'],
])('switches the actual %s QR-login landing state to account login before submitting', (provider, url, tabHtml) => {
  const p = page(url);
  try {
    const form = p.dom.window.document.querySelector('form')!;
    form.style.display = 'none';
    form.insertAdjacentHTML('beforebegin', tabHtml);
    const tab = form.previousElementSibling as HTMLElement;
    tab.addEventListener('click', () => { form.style.display = ''; tab.style.display = 'none'; });
    p.run(provider); p.tick(); p.tick();
    expect(form.style.display).toBe('');
    expect((form.querySelector('[type="password"]') as HTMLInputElement).value).toBe('provided-password');
    expect(p.submissions()).toBe(1);
  } finally { p.dom.window.close(); }
});

it('enables the Baidu login button through its official agreement control before submitting', () => {
  const p = page('https://pan.baidu.com', '<input type="checkbox" name="isAgree" id="TANGRAM__PSP_11__isAgree">');
  try {
    const button = p.dom.window.document.querySelector('button')!;
    const agreement = p.dom.window.document.querySelector('[name="isAgree"]') as HTMLInputElement;
    button.disabled = true;
    agreement.addEventListener('change', () => { button.disabled = !agreement.checked; });
    p.run('baidu'); p.tick(); p.tick();
    expect(agreement.checked).toBe(true);
    expect(p.submissions()).toBe(1);
  } finally { p.dom.window.close(); }
});

it('still fills a password form that loads after the initial fifteen seconds', () => {
  const p = page('https://pan.baidu.com');
  try {
    const form = p.dom.window.document.querySelector('form')!;
    form.style.display = 'none';
    p.run('baidu'); p.tick();
    p.advance(20000);
    form.style.display = '';
    p.tick();
    expect(p.submissions()).toBe(1);
    expect((form.querySelector('[type="password"]') as HTMLInputElement).value).toBe('provided-password');
  } finally { p.dom.window.close(); }
});

it('waits for Tianyi initialization before consuming its only automatic submit attempt', () => {
  const p = page('https://open.e.189.cn/login');
  try {
    const document = p.dom.window.document;
    const form = document.querySelector('form')!;
    // The server initially renders a password form under a loading overlay.
    // The official bundle binds handlers and switches to QR mode later.
    form.insertAdjacentHTML('afterend', '<div id="j-login-loading">加载中</div>');
    const overlay = document.querySelector('#j-login-loading') as HTMLElement;
    p.run('tianyi'); p.tick(); p.tick();
    expect(p.submissions()).toBe(0);
    expect((form.querySelector('[type="password"]') as HTMLInputElement).value).toBe('');
    expect(p.dom.window.sessionStorage.getItem('alldownloader-login-attempt-1')).toBeNull();

    form.style.display = 'none';
    form.insertAdjacentHTML('beforebegin', '<li id="tab-pw">账号登录</li>');
    const tab = document.querySelector('#tab-pw') as HTMLElement;
    tab.addEventListener('click', () => { form.style.display = ''; });
    overlay.style.display = 'none';
    p.tick(); p.tick();
    expect(p.submissions()).toBe(1);
    expect((form.querySelector('[type="password"]') as HTMLInputElement).value).toBe('provided-password');
  } finally { p.dom.window.close(); }
});

it('does not submit Tianyi credentials until the official agreement click actually takes effect', () => {
  const p = page('https://open.e.189.cn/login', '<span id="j-agreement-box" class="checkbox-icon1"></span>');
  try {
    const agreement = p.dom.window.document.querySelector('#j-agreement-box') as HTMLElement;
    p.run('tianyi'); p.tick();
    expect(p.submissions()).toBe(0);
    expect(p.dom.window.sessionStorage.getItem('alldownloader-login-attempt-1')).toBeNull();
    // Only the official page owns this state; merely issuing click is not
    // proof that its handler accepted the agreement.
    agreement.addEventListener('click', () => { agreement.classList.toggle('checkbox-icon2'); });
    p.tick(); p.tick();
    expect(agreement.classList.contains('checkbox-icon2')).toBe(true);
    expect(p.submissions()).toBe(1);
  } finally { p.dom.window.close(); }
});

it('fills credentials but leaves an official verification field for the user', () => {
  const p = page('https://passport.baidu.com/login', '<input name="verifyCode">');
  try {
    p.run('baidu'); p.tick();
    expect((p.dom.window.document.querySelector('[type="password"]') as HTMLInputElement).value).toBe('provided-password');
    expect(p.submissions()).toBe(0);
  } finally { p.dom.window.close(); }
});

it('submits the current Tianyi input-button login control inside its official frame', () => {
  const p = page('https://open.e.189.cn/login');
  try {
    const button = p.dom.window.document.createElement('input');
    button.type = 'button'; button.id = 'j-login'; button.value = '登录';
    p.dom.window.document.querySelector('button')!.replaceWith(button);
    let submitted = false;
    button.addEventListener('click', () => { submitted = true; });
    p.run('tianyi'); p.tick();
    expect(submitted).toBe(true);
    expect((p.dom.window.document.querySelector('[type="password"]') as HTMLInputElement).value).toBe('provided-password');
  } finally { p.dom.window.close(); }
});

it.each(['https://passport.baidu.com.example.org', 'http://passport.baidu.com', 'https://passport.baidu.com:444', 'https://open.e.189.cn'])('does not expose Baidu credentials to %s', url => {
  const p = page(url);
  try {
    p.run('baidu'); p.tick();
    expect((p.dom.window.document.querySelector('[type="password"]') as HTMLInputElement).value).toBe('');
    expect(p.submissions()).toBe(0);
  } finally { p.dom.window.close(); }
});

it('does not fill a login form whose action leaves the official origin', () => {
  const p = page('https://passport.baidu.com/login');
  try {
    p.dom.window.document.querySelector('form')!.action = 'https://example.org/collect';
    p.run('baidu'); p.tick();
    expect((p.dom.window.document.querySelector('[type="password"]') as HTMLInputElement).value).toBe('');
    expect(p.submissions()).toBe(0);
  } finally { p.dom.window.close(); }
});
