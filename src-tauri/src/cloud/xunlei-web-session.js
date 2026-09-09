(async () => {
  // Checked against the official 2026-09-10 pan.xunlei.com bundles; see
  // docs/xunlei-protocol-analysis-2026-09-10.md. A candidate is NOT proof
  // of authorization: Rust must successfully read the account's root first.
  if (location.protocol !== 'https:' || location.hostname !== 'pan.xunlei.com') return null;
  const clientId = 'Xqp0kJBXWhwaTpB6';
  const packageName = 'pan.xunlei.com';
  const text = value => typeof value === 'string' && value.trim() ? value : undefined;
  const field = (object, names) => names.map(name => text(object?.[name])).find(Boolean);
  const userId = object => field(object, ['userId', 'user_id', 'userid', 'sub']);
  const liveUserId = object => {
    const id = userId(object);
    return id === '0' || id?.startsWith('AMS=') ? undefined : id;
  };
  const unixSeconds = value => {
    if (value == null || value === '') return undefined;
    const numeric = typeof value === 'number' ? value : /^\d+(\.\d+)?$/.test(String(value)) ? Number(value) : NaN;
    const seconds = Number.isFinite(numeric)
      ? numeric > 1e12 ? numeric / 1000 : numeric
      : new Date(value).getTime() / 1000;
    return Number.isFinite(seconds) && seconds > 0 ? Math.floor(seconds) : undefined;
  };
  const liveUsers = window.$nuxt?.$store?.state?.users;
  // A live null means logout. Hydration is only usable before the live store exists.
  const currentUser = liveUsers ? liveUsers.curUser : window.__NUXT__?.state?.users?.curUser;
  if (liveUsers && currentUser === null) return null;
  const initialLiveId = liveUserId(currentUser);
  let current = currentUser;
  let oauth;
  let requireModule;
  let profile;
  try {
    // Webpack 4's documented-by-runtime JSONP entry shape. This single private
    // module only obtains its require function; no application module is patched.
    const chunks = window.webpackJsonp;
    if (Array.isArray(chunks) && chunks.push !== Array.prototype.push) {
      const key = '__alldownloader_xunlei_session_' + Date.now() + '_' + Math.random().toString(36).slice(2);
      const entry = [[key], { [key]: (_module, _exports, require) => { requireModule = require; } }, [[key]]];
      try { chunks.push(entry); }
      finally {
        if (requireModule?.c) delete requireModule.c[key];
        if (requireModule?.m) delete requireModule.m[key];
        if (chunks[chunks.length - 1] === entry) chunks.pop();
      }
    }
    const get = id => requireModule?.m?.[id] ? requireModule(id) : undefined;
    profile = get(32)?.default;
    if (profile?.clientId === clientId && profile?.packageName === packageName) {
      let sso = get(600)?.sso;
      if (!sso && typeof get(21)?.getSso === 'function') sso = await get(21).getSso();
      oauth = sso?.oAuth2Client;
      if (typeof oauth?.getCredentials === 'function' && typeof oauth?.getCurrentSub === 'function') {
        // The official SDK alone selects its current_sub/credentials_<client>@<sub>.
        // Do not scan storage or combine access and refresh tokens from records.
        const before = text(await oauth.getCurrentSub());
        const credentials = await oauth.getCredentials();
        const after = text(await oauth.getCurrentSub());
        if (!before || before !== after || userId(credentials) !== after) return null;
        if (initialLiveId && initialLiveId !== after) return null;
        current = credentials;
      }
    }
  } catch (_) {
    // A discovered SDK which cannot establish its current identity cannot be
    // replaced with a potentially stale Vuex credential. Retry on the next poll.
    if (oauth) return null;
  }
  const accessToken = field(current, ['access_token', 'accessToken']);
  const id = userId(current);
  if (!accessToken || !id || id === '0' || id.startsWith('AMS=')) return null;
  const out = {
    user_id: id, access_token: accessToken, client_id: clientId,
    package_name: packageName, user_agent: text(navigator.userAgent),
  };
  const refreshToken = field(current, ['refresh_token', 'refreshToken']);
  if (refreshToken) out.refresh_token = refreshToken;
  const expiresAt = unixSeconds(current.expires_at ?? current.expiresAt);
  if (expiresAt) out.expires_at = expiresAt;
  // Only the chosen account record may contribute these optional legacy fields.
  for (const [name, aliases] of Object.entries({
    session_id: ['session_id', 'sessionId', 'sessionid'],
    peer_id: ['peer_id', 'peerId', 'peerid'],
  })) {
    const value = field(current, aliases);
    if (value) out[name] = value;
  }
  try {
    if (requireModule && profile?.clientId === clientId && profile?.packageName === packageName) {
      const deviceModule = requireModule.m?.[142] && requireModule(142);
      const device = deviceModule?.a && await deviceModule.a;
      const deviceId = device && text(await device.deviceID);
      if (deviceId) out.device_id = deviceId;
      const version = requireModule.m?.[178] && text(requireModule(178)?.version);
      if (version) out.client_version = version;
      let signModule = requireModule.m?.[1618] && requireModule(1618);
      if (typeof signModule?.a !== 'function') {
        // Module 16 is the official lazy loader for utils-async + captcha.
        // Loading through its named export avoids hard-coded chunk URLs/IDs.
        const loader = requireModule.m?.[16] && requireModule(16);
        if (typeof loader?.preloadCaptchaRuntime === 'function') {
          await loader.preloadCaptchaRuntime();
          signModule = requireModule.m?.[1618] && requireModule(1618);
        }
      }
      if (deviceId && version && typeof signModule?.a === 'function') {
        // Call the official signing implementation with its actual millisecond
        // timestamp. No local reconstruction of the obfuscated signature algorithm.
        const timestamp = String(Date.now());
        const signature = text(await signModule.a(timestamp));
        if (signature) out.web_security = {
          action: 'GET:/drive/v1/files', captcha_sign: signature, timestamp,
          client_version: version, package_name: packageName,
        };
      }
    }
  } catch (_) {
    // Credentials remain a candidate when optional SDK/device modules change.
    // No guessed identity/signature or stale client-scoped captcha is substituted.
  }
  let captchaFailed = false;
  if (out.web_security && requireModule?.m?.[16]) {
    try {
      const loader = requireModule(16);
      if (typeof loader?.getCaptchaClient === 'function') {
        if (oauth && text(await oauth.getCurrentSub()) !== id) return null;
        const latestUser = window.$nuxt?.$store?.state?.users?.curUser;
        if (liveUsers && latestUser === null) return null;
        if (initialLiveId && liveUserId(latestUser) !== id) return null;
        const { captcha } = await loader.getCaptchaClient();
        if (typeof captcha?.getCaptchaToken === 'function') {
          // The SDK owns the visible challenge window and redirect lifecycle.
          // forceRenew prevents its client-scoped cache from returning a token
          // generated for a previously logged-in account. The host retains this
          // Promise across polling while the user completes the official UI.
          if (captcha._config?.clientId !== clientId || captcha._config?.originDeviceId !== out.device_id) {
            captchaFailed = true;
          } else {
            const security = out.web_security;
            const result = await captcha.getCaptchaToken({ forceRenew: true }, {
              action: security.action,
              withCaptchaMeta: {
                package_name: packageName, client_version: security.client_version,
                captcha_sign: security.captcha_sign, timestamp: security.timestamp, user_id: id,
              },
            });
            const token = text(result?.token);
            const expiresAt = unixSeconds(result?.expires_at);
            if (token && expiresAt > Math.floor(Date.now() / 1000)) {
              security.captcha_token = token;
              security.expires_at = expiresAt;
            } else captchaFailed = true;
          }
        }
      }
    } catch (_) {
      // Raw SDK error details can contain private data. A fixed code lets the
      // host display failure without treating a rejected challenge as success.
      captchaFailed = true;
    }
  }
  // Signing/device SDK calls can yield while the user changes accounts. Even
  // a failed optional signature must be followed by the same identity check.
  try {
    if (oauth && text(await oauth.getCurrentSub()) !== id) return null;
    const latestUser = window.$nuxt?.$store?.state?.users?.curUser;
    if (liveUsers && latestUser === null) return null;
    const latestId = liveUserId(latestUser);
    if (liveUsers && latestId !== initialLiveId && latestId !== id) return null;
  } catch (_) { return null; }
  if (captchaFailed) return { authorization_error: 'captcha_verification_failed' };
  return out;
})()
