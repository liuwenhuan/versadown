(() => {
  if (location.origin !== 'https://cloud.189.cn' || window.__alldownloaderTianyiCapture) return;
  window.__alldownloaderTianyiCapture = true;
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
      window.__alldownloaderTianyiContext = { browser_id: value, user_agent: navigator.userAgent };
    }
    return result;
  };
})();
