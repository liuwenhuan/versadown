const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const source = fs.readFileSync(
  path.resolve(__dirname, '../src-tauri/src/cloud/driver/native/xunlei_auth/password.js'),
  'utf8',
);

test('Xunlei password login accepts the official agreement before submitting', async () => {
  const dom = new JSDOM(`<!doctype html><body>
    <div id="password-tab">账号密码登录</div>
    <form>
      <input class="xlubase-login-input" type="text" placeholder="请输入手机号">
      <input class="xlubase-login-input" type="text" placeholder="请输入验证码">
      <input class="xlucommon-login-checkbox" type="checkbox">
      <button type="button" disabled>登录</button>
    </form>
  </body>`, {
    url: 'https://i.xunlei.com/center/account/personal/auth/',
    runScripts: 'outside-only',
  });
  const { window } = dom;
  window.HTMLElement.prototype.getClientRects = () => [{ width: 1, height: 1 }];
  const form = window.document.querySelector('form');
  const inputs = form.querySelectorAll('input');
  const agreement = inputs[2];
  const submit = form.querySelector('button');
  let submits = 0;
  window.document.querySelector('#password-tab').addEventListener('click', () => {
    inputs[0].placeholder = '请输入手机号/邮箱/账号';
    inputs[1].type = 'password';
    inputs[1].placeholder = '请输入密码';
  });
  agreement.addEventListener('change', () => { submit.disabled = !agreement.checked; });
  submit.addEventListener('click', event => { event.preventDefault(); submits += 1; });

  try {
    window.eval(`${source}(...${JSON.stringify([
      'xunlei', ['pan.xunlei.com', 'i.xunlei.com'], 'probe-user', 'probe-password', 'probe-attempt',
    ])});`);
    await new Promise(resolve => setTimeout(resolve, 1400));

    assert.equal(inputs[0].value, 'probe-user');
    assert.equal(inputs[1].value, 'probe-password');
    assert.equal(agreement.checked, true);
    assert.equal(submits, 1);
    assert.equal(window.__versadownXunleiPassword.submitted, true);
    window.eval(`${source}('xunlei',[], 'other-user', 'other-password', 'second-attempt');`);
    await new Promise(resolve => setTimeout(resolve, 400));
    assert.equal(submits, 1);
    assert.equal(inputs[0].value, 'probe-user');
  } finally {
    dom.window.close();
  }
});
