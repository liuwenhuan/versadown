//! Linux WebKitGTK memory defaults.
//!
//! The JSC JIT reserves executable memory that this small React UI does not
//! need; it must be disabled before WebKitGTK spawns its web process, and a
//! value the user already exported always wins. Accelerated compositing stays
//! on: the transparent launcher needs it (see the test below), and WebKitGTK
//! 2.52 kills the web process when a live view's acceleration policy changes.

/// Environment defaults applied before Tauri starts WebKitGTK.
const WEBKIT_ENV_DEFAULTS: [(&str, &str); 1] = [("JSC_useJIT", "0")];

/// Returns the defaults the process environment does not already define.
pub fn missing_webkit_env_defaults(
    is_defined: impl Fn(&str) -> bool,
) -> Vec<(&'static str, &'static str)> {
    WEBKIT_ENV_DEFAULTS
        .into_iter()
        .filter(|(name, _)| !is_defined(name))
        .collect()
}

/// Must run before any thread is spawned: `set_var` is not synchronised with
/// concurrent C `getenv` readers.
#[cfg(target_os = "linux")]
pub fn apply_process_defaults() {
    for (name, value) in missing_webkit_env_defaults(|name| std::env::var_os(name).is_some()) {
        std::env::set_var(name, value);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn apply_process_defaults() {}

/// The UI is a local app, not a browser: drop WebKit's page and resource
/// caches. The web context is shared by every window, so this is idempotent.
#[cfg(target_os = "linux")]
pub fn configure_webview(window: &tauri::WebviewWindow) {
    use webkit2gtk::{CacheModel, WebContextExt, WebViewExt};

    let result = window.with_webview(|webview| {
        if let Some(context) = webview.inner().context() {
            context.set_cache_model(CacheModel::DocumentViewer);
        }
    });
    if let Err(error) = result {
        eprintln!("无法设置 WebKit 缓存模式: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
pub fn configure_webview(_window: &tauri::WebviewWindow) {}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_every_default_to_a_clean_environment() {
        assert_eq!(
            missing_webkit_env_defaults(|_| false),
            vec![("JSC_useJIT", "0")]
        );
    }

    #[test]
    fn compositing_is_not_disabled_process_wide() {
        // A process-wide switch also reaches the transparent launcher, which
        // then leaves stale popup pixels and repaints its canvas on every drag.
        assert!(missing_webkit_env_defaults(|_| false)
            .iter()
            .all(|(name, _)| *name != "WEBKIT_DISABLE_COMPOSITING_MODE"));
    }

    #[test]
    fn keeps_values_the_user_already_exported() {
        assert!(missing_webkit_env_defaults(|name| name == "JSC_useJIT").is_empty());
    }
}
