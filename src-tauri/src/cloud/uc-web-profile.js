(() => {
  // UC's page exposes the signed-in nickname through its already-loaded Redux
  // store. Capture only that presentation field: do not scan localStorage,
  // enumerate cookies, or return a browser token to Rust.
  if (location.protocol !== 'https:' || location.hostname !== 'drive.uc.cn') return {};
  let requireModule;
  try {
    const chunks = window.webpackJsonp;
    if (!Array.isArray(chunks) || chunks.push === Array.prototype.push) return {};
    const key = '__versadown_uc_profile_' + Date.now() + '_' + Math.random().toString(36).slice(2);
    const entry = [[key], { [key]: (_module, _exports, require) => { requireModule = require; } }, [[key]]];
    try { chunks.push(entry); }
    finally {
      if (requireModule?.c) delete requireModule.c[key];
      if (requireModule?.m) delete requireModule.m[key];
      if (chunks[chunks.length - 1] === entry) chunks.pop();
    }
    for (const record of Object.values(requireModule?.c || {})) {
      const exports = record?.exports;
      for (const candidate of [exports, exports?.default, exports?.a, exports?.b]) {
        const state = candidate?.getState?.();
        const nickname = state?.user?.userInfo?.nickname;
        if (typeof nickname === 'string' && nickname.trim()) return { nickname: nickname.trim() };
      }
    }
  } catch (_) {
    // Keep an otherwise valid UC authorization usable when the official page
    // changes its bundle runtime or has not hydrated its store yet.
  }
  return {};
})()
