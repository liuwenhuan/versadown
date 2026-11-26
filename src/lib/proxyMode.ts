import type { ProxyMode, ProxySettings } from './types';

/** The settings store the mode as two flags; `direct` wins over `enabled`. */
export function proxyMode(proxy: ProxySettings): ProxyMode {
  if (proxy.direct) return 'direct';
  return proxy.enabled ? 'manual' : 'system';
}

/** Switch modes while keeping the manual address for later use. */
export function withProxyMode(proxy: ProxySettings, mode: ProxyMode): ProxySettings {
  return { ...proxy, enabled: mode === 'manual', direct: mode === 'direct' };
}

/** Whether a manual proxy address exists, whichever mode is active. */
export function hasManualProxy(proxy: ProxySettings | undefined): boolean {
  return Boolean(proxy?.host.trim());
}
