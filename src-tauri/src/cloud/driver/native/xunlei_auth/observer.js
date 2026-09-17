(() => {
  'use strict';
  if (location.origin !== 'https://pan.xunlei.com' || window.__alldownloaderXunleiAuth) return;
  const clientId = 'Xqp0kJBXWhwaTpB6';
  const driveHost = 'api-pan.xunlei.com';
  const authHost = 'xluser-ssl.xunlei.com';
  const text = value => typeof value === 'string' ? value : '';
  const json = value => { try { return JSON.parse(value); } catch { return null; } };
  const claims = token => {
    try { return json(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/'))) || {}; }
    catch { return {}; }
  };
  let identity = '', generation = 0, candidate = null, signedInAt = 0, phase = 'waiting_login';
  let oauth = null;
  const tickets = new Map();
  window.addEventListener('message', event => {
    if (event.origin === 'https://i.xunlei.com'
      && event.data?.type === 'alldownloader-xunlei-password'
      && event.data.error === 'plugin_update_required' && !candidate) phase = 'incompatible';
  });
  function begin(url, method, headers, body) {
    try {
      const parsed = new URL(url, location.href);
      if (parsed.protocol !== 'https:' || parsed.port || ![driveHost, authHost].includes(parsed.hostname)) return null;
      const h = new Headers(headers);
      const bearer = (h.get('authorization') || '').replace(/^Bearer\s+/i, '');
      const payload = claims(bearer);
      const sub = text(payload.sub);
      // JWT fields select a candidate only. Both the official response and
      // the native root request must accept it before authorization succeeds.
      if (sub && identity !== sub) {
        identity = sub; generation++; candidate = null;
        if (oauth?.sub !== sub) oauth = null;
        tickets.clear();
        signedInAt = 0; phase = 'checking_session';
      }
      return {url:parsed, method:String(method || 'GET').toUpperCase(), headers:h,
        body:typeof body === 'string' ? json(body) : null, bearer, payload, sub, generation};
    } catch { return null; }
  }
  function finish(request, status, value) {
    if (!request || request.generation !== generation) return;
    const {url, headers, body, bearer, payload, sub, method} = request;
    if (status === 401 && bearer && sub === identity) { candidate = null; phase = 'waiting_login'; return; }
    if (status < 200 || status >= 300 || !value || typeof value !== 'object' || value.error
      || Number(value.error_code || value.code || 0) !== 0) return;
    const now = Math.floor(Date.now() / 1000);
    if (url.hostname === authHost && url.pathname === '/v1/shield/captcha/init') {
      if (body?.client_id !== clientId || body?.meta?.package_name !== 'pan.xunlei.com'
        || !value.captcha_token || value.url || !body.meta.user_id) return;
      tickets.set(value.captcha_token, {user:body.meta.user_id, device:body.device_id,
        captcha_token:value.captcha_token, expires_at:now + Number(value.expires_in || 0),
        captcha_sign:text(body.meta.captcha_sign), timestamp:text(body.meta.timestamp),
        client_version:text(body.meta.client_version), package_name:'pan.xunlei.com'});
      if (tickets.size > 8) tickets.delete(tickets.keys().next().value);
      return;
    }
    if (url.hostname === authHost && /^\/v1\/auth\/(token|signin)$/.test(url.pathname)) {
      const id = text(value.sub || claims(text(value.access_token)).sub);
      if (id && (!identity || id === identity)) oauth = {sub:id, access:text(value.access_token), refresh:text(value.refresh_token)};
      return;
    }
    if (!bearer || !sub || headers.get('x-client-id') !== clientId || !headers.get('x-device-id')) return;
    if (!signedInAt) signedInAt = Date.now();
    if (!candidate) phase = 'waiting_drive';
    if (url.hostname !== driveHost || url.pathname !== '/drive/v1/files' || method !== 'GET'
      || (url.searchParams.get('parent_id') || '') !== '' || !Array.isArray(value.files)) return;
    const device = headers.get('x-device-id');
    const captcha = headers.get('x-captcha-token') || '';
    const ticket = tickets.get(captcha);
    const matched = ticket && ticket.user === sub && ticket.device === device ? ticket : null;
    const version = text(window.PAN_VERSION) || matched?.client_version || '';
    if (!version || !Number.isFinite(payload.exp) || payload.exp <= now) { phase = 'incompatible'; return; }
    // A cached token may precede this document. Its successful root response
    // proves current usability; cap local reuse to two minutes in that case.
    const security = matched ? {...matched} : {captcha_token:captcha, expires_at:now + 120};
    delete security.user; delete security.device;
    security.action = 'GET:/drive/v1/files';
    candidate = {schema:1, user_id:sub, access_token:bearer,
      refresh_token:oauth?.sub === sub && oauth.access === bearer ? oauth.refresh : '',
      expires_at:payload.exp, client_id:clientId, device_id:device, client_version:version,
      package_name:'pan.xunlei.com', user_agent:navigator.userAgent, web_security:security};
    phase = 'ready';
  }
  // Observe the requests the official page already makes. Do not call its
  // private SDK, read its storage, renew captchas, or consume response bodies.
  const nativeFetch = window.fetch;
  if (typeof nativeFetch === 'function') window.fetch = function(input, init) {
    const request = begin(typeof input === 'string' || input instanceof URL ? String(input) : input.url,
      init?.method || input?.method, init?.headers || input?.headers, init?.body);
    const result = nativeFetch.apply(this, arguments);
    if (request) result.then(response => response.clone().json().then(value => finish(request, response.status, value)).catch(() => {}), () => {});
    return result;
  };
  const contexts = new WeakMap();
  const proto = XMLHttpRequest.prototype, open = proto.open, setHeader = proto.setRequestHeader, send = proto.send;
  proto.open = function(method, url) { contexts.set(this,{method,url,headers:new Headers()}); return open.apply(this,arguments); };
  proto.setRequestHeader = function(name,value) { contexts.get(this)?.headers.append(name,value); return setHeader.apply(this,arguments); };
  proto.send = function(body) {
    const context = contexts.get(this);
    const request = context && begin(context.url,context.method,context.headers,body);
    if (request) this.addEventListener('load', () => {
      try { finish(request,this.status,this.responseType === 'json' ? this.response : json(this.responseText)); } catch {}
    }, {once:true});
    return send.apply(this,arguments);
  };
  window.__alldownloaderXunleiAuth = Object.freeze({
    snapshot() {
      if (candidate && candidate.user_id === identity && candidate.expires_at > Date.now()/1000) return JSON.parse(JSON.stringify(candidate));
      if (phase === 'incompatible' || (signedInAt && Date.now()-signedInAt > 45000)) return {authorization_error:'plugin_update_required'};
      return {pending:true};
    },
    status() { return {phase, captured:Boolean(candidate)}; },
  });
})();
