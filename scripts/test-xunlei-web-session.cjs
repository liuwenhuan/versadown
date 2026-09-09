// Synthetic browser states only. No account credentials or network calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');

const root = path.resolve(__dirname, '..');
const scriptPath = path.join(root, 'src-tauri/src/cloud/xunlei-web-session.js');
// RED mode runs the old production extractor before its replacement exists.
const source = fs.existsSync(scriptPath)
  ? fs.readFileSync(scriptPath, 'utf8')
  : fs.readFileSync(path.join(root, 'src-tauri/src/cloud/commands.rs'), 'utf8')
    .match(/const XUNLEI_WEB_TOKENS_JS: &str = r#"([\s\S]*?)"#;/)[1];
const clientId = 'Xqp0kJBXWhwaTpB6';
const current = { userId: 'current-user', accessToken: 'current-access' };
function storage(data = {}) {
  return { length: Object.keys(data).length, key: i => Object.keys(data)[i],
    getItem: key => data[key] === undefined ? null : JSON.stringify(data[key]) };
}
function sdkModules({ credentials, sub = 'current-user', signature, device = 'official-device' } = {}) {
  return {
    32: { default: { clientId, packageName: 'pan.xunlei.com' } },
    178: { version: '1.92.98' },
    600: { sso: { oAuth2Client: {
      getCredentials: async () => credentials,
      getCurrentSub: async () => sub,
    } } },
    142: { a: Promise.resolve({ deviceID: Promise.resolve(device) }) },
    1618: { a: signature || (async () => '1.official-sdk-signature') },
  };
}
async function extract({ user = current, hydrated, local = {}, modules, hostname = 'pan.xunlei.com', protocol = 'https:' } = {}) {
  const window = { $nuxt: { $store: { state: { users: { curUser: user } } } },
    __NUXT__: { state: { users: { curUser: hydrated } } } };
  if (modules) {
    const require = id => {
      if (!require.c[id]) {
        const module = require.c[id] = { exports: {} };
        require.m[id](module, module.exports, require);
      }
      return require.c[id].exports;
    };
    require.c = {};
    require.m = Object.fromEntries(Object.entries(modules).map(([id, value]) =>
      [id, module => { module.exports = value; }]));
    window.webpackJsonp = [];
    window.webpackJsonp.push = entry => {
      Object.assign(require.m, entry[1]);
      for (const [id] of entry[2] || []) require(id);
    };
  }
  const value = await vm.runInNewContext(source, { window, location: { protocol, hostname, host: hostname },
    navigator: { userAgent: 'Synthetic browser UA' }, localStorage: storage(local), sessionStorage: storage(),
    setTimeout, clearTimeout, URL, console: { log() { throw Error('Credentials must not be logged'); } } });
  return value == null ? null : JSON.parse(JSON.stringify(value));
}
test('old storage cannot replace the live current account', async () => {
  const value = await extract({ local: { old: { id: 'old-user', access_token: 'old-access', refresh_token: 'old-refresh' } } });
  assert.equal(value.user_id, 'current-user');
  assert.equal(value.access_token, 'current-access');
  assert.equal(value.refresh_token, undefined);
});
test('generic ids and nested secrets are never credentials', async () => {
  assert.equal(await extract({ user: { id: 'photo-id', access_token: 'unrelated', password: { userId: 'secret', accessToken: 'secret' } } }), null);
});
test('only the official HTTPS cloud origin may export credentials', async () => {
  for (const hostname of ['xunlei.com', 'i.xunlei.com', 'evil.pan.xunlei.com', 'pan.xunlei.com.evil.test']) {
    assert.equal(await extract({ hostname }), null);
  }
  assert.equal(await extract({ protocol: 'http:' }), null);
});
test('live logout cannot fall back to hydrated or stored credentials', async () => {
  assert.equal(await extract({ user: null, hydrated: current, local: { credentials: current } }), null);
});
test('SDK returns renewable credentials as one account with normalized expiry and official signature', async () => {
  const value = await extract({ modules: sdkModules({ credentials: {
    sub: 'current-user', access_token: 'sdk-access', refresh_token: 'sdk-refresh',
    expires_at: new Date('2030-01-01T00:00:00Z'), password: 'never-export',
  } }) });
  assert.equal(value.access_token, 'sdk-access');
  assert.equal(value.refresh_token, 'sdk-refresh');
  assert.equal(value.expires_at, 1893456000);
  assert.equal(value.device_id, 'official-device');
  assert.equal(value.client_id, clientId);
  assert.equal(value.user_agent, 'Synthetic browser UA');
  assert.equal(value.web_security.captcha_sign, '1.official-sdk-signature');
  assert.equal(value.web_security.action, 'GET:/drive/v1/files');
  assert.equal(value.password, undefined);
});
test('SDK logout and account switch never reuse a different current credential', async () => {
  assert.equal(await extract({ modules: sdkModules({ credentials: null }) }), null);
  assert.equal(await extract({ modules: sdkModules({ credentials: { sub: 'old-user', access_token: 'old-access' } }) }), null);
});
test('failed official signature leaves a credential candidate without fabricated security data', async () => {
  const value = await extract({ modules: sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' },
    signature: async () => { throw Error('SDK changed'); } }) });
  assert.equal(value.access_token, 'sdk-access');
  assert.equal(value.web_security, undefined);
});
test('client-scoped captcha storage is never mixed into a newly selected account', async () => {
  const value = await extract({ local: { ['captcha_' + clientId]: { token: 'old-account-captcha', expires_at: '2030-01-01' } },
    modules: sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' } }) });
  assert.equal(value.security_token, undefined);
  assert.ok(value.web_security, 'official signature metadata is available');
  assert.equal(value.web_security.captcha_token, undefined);
});
test('unknown webpack/profile fails safely without calling foreign signing code', async () => {
  let signed = false;
  const modules = sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' },
    signature: () => { signed = true; return 'foreign-signature'; } });
  modules[32].default.clientId = 'other-product';
  const value = await extract({ modules });
  assert.equal(value.web_security, undefined);
  assert.equal(value.client_id, clientId);
  assert.equal(signed, false);
});
test('in-place account switch during signing discards the entire candidate', async () => {
  const user = { ...current };
  const modules = sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' },
    signature: async () => { user.userId = 'different-user'; return '1.signature'; } });
  assert.equal(await extract({ user, modules }), null);
});
test('failure to recheck the account after signing cannot return a stale candidate', async () => {
  const modules = sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' },
    signature: async () => {
      modules[600].sso.oAuth2Client.getCurrentSub = async () => { throw Error('logout'); };
      return '1.signature';
    } });
  assert.equal(await extract({ modules }), null);
});
test('official lazy captcha loader makes its signer available before extraction', async () => {
  const modules = sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' } });
  modules[1618] = {};
  modules[16] = { preloadCaptchaRuntime: async () => {
    modules[1618].a = async () => '1.loaded-official-signature';
  } };
  const value = await extract({ modules });
  assert.equal(value.web_security?.captcha_sign, '1.loaded-official-signature');
});
function captchaModules(getCaptchaToken) {
  const modules = sdkModules({ credentials: { sub: 'current-user', access_token: 'sdk-access' } });
  modules[16] = { getCaptchaClient: async () => ({ captcha: {
    _config: { clientId, originDeviceId: 'official-device' }, getCaptchaToken,
  } }) };
  return modules;
}
test('official captcha refresh uses the selected account and returns fresh token expiry', async () => {
  let request;
  const modules = captchaModules(async (renew, options) => {
    request = { renew, options };
    return { token: 'fresh-captcha', expires_at: 'Tue Jan 01 2030 00:00:00 GMT+0000' };
  });
  const value = await extract({ modules });
  assert.equal(value.web_security.captcha_token, 'fresh-captcha');
  assert.equal(value.web_security.expires_at, 1893456000);
  assert.equal(request.renew.forceRenew, true);
  assert.equal(request.options.action, 'GET:/drive/v1/files');
  assert.equal(request.options.withCaptchaMeta.user_id, 'current-user');
  assert.equal(request.options.withCaptchaMeta.captcha_sign, '1.official-sdk-signature');
  assert.equal(request.options.withCaptchaMeta.timestamp, value.web_security.timestamp);
});
test('captcha rejection exports only a fixed failure code without credentials or SDK secrets', async () => {
  const modules = captchaModules(async () => { throw Error('private-token-in-server-error'); });
  assert.deepEqual(await extract({ modules }), { authorization_error: 'captcha_verification_failed' });
});
test('expired or missing captcha result cannot be marked as successful verification', async () => {
  for (const result of [{ token: 'expired', expires_at: '2000-01-01' }, {}, { token: 'no-expiry' }]) {
    assert.deepEqual(await extract({ modules: captchaModules(async () => result) }),
      { authorization_error: 'captcha_verification_failed' });
  }
});
test('account switching during human verification discards the completed captcha', async () => {
  const user = { ...current };
  const modules = captchaModules(async () => {
    user.userId = 'different-user';
    return { token: 'fresh-captcha', expires_at: '2030-01-01' };
  });
  assert.equal(await extract({ user, modules }), null);
});
test('Vuex string zero placeholder cannot override a consistent signed-in SDK account', async () => {
  const value = await extract({ user: { userId: '0' }, modules: captchaModules(async () =>
    ({ token: 'fresh-captcha', expires_at: '2030-01-01' })) });
  assert.equal(value?.user_id, 'current-user');
  assert.equal(value?.access_token, 'sdk-access');
  assert.equal(value?.web_security?.captcha_token, 'fresh-captcha');
});
test('explicit live logout rejects a still-cached SDK credential', async () => {
  const modules = sdkModules({ credentials: { sub: 'current-user', access_token: 'stale-sdk-access' } });
  assert.equal(await extract({ user: null, modules }), null);
});
