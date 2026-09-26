(() => {
  if (location.origin !== 'https://cloud.189.cn' || window.__versadownTianyiCapture) return;
  window.__versadownTianyiCapture = true;
  // Observe only the identity produced by the official site's own requests.
  // Do not generate a fingerprint, inspect credentials or alter any request.
  const destinations = new WeakMap();
  const prototype = XMLHttpRequest.prototype;
  const originalOpen = prototype.open;
  const originalSetHeader = prototype.setRequestHeader;
  prototype.open = function (method, url) {
    const result = originalOpen.apply(this, arguments);
    destinations.delete(this);
    try { destinations.set(this, new URL(url, location.href)); } catch (_) {}
    return result;
  };
  prototype.setRequestHeader = function (name, value) {
    const result = originalSetHeader.apply(this, arguments);
    const url = destinations.get(this);
    if (url?.origin === 'https://cloud.189.cn' && url.pathname.startsWith('/api/') &&
        typeof name === 'string' && name.toLowerCase() === 'browser-id' &&
        typeof value === 'string' && /^[a-f0-9]{32}$/i.test(value)) {
      window.__versadownTianyiContext = { browser_id: value, user_agent: navigator.userAgent };
    }
    return result;
  };
  const originalFetch = window.fetch;
  window.fetch = function (input, init) {
    try {
      const requestUrl = new URL(
        typeof input === 'string' || input instanceof URL ? input : input.url,
        location.href,
      );
      const headers = new Headers(
        init?.headers || (typeof Request !== 'undefined' && input instanceof Request ? input.headers : undefined),
      );
      const browserId = headers.get('browser-id');
      if (requestUrl.origin === 'https://cloud.189.cn' && requestUrl.pathname.startsWith('/api/') &&
          typeof browserId === 'string' && /^[a-f0-9]{32}$/i.test(browserId)) {
        window.__versadownTianyiContext = { browser_id: browserId, user_agent: navigator.userAgent };
      }
    } catch (_) {}
    return originalFetch.apply(this, arguments);
  };
})();
