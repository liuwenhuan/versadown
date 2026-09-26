(() => {
  if (window.__versaDownBaiduLoginTriggerInstalled) return;
  window.__versaDownBaiduLoginTriggerInstalled = true;

  const expectedLabel = "去登录";
  const normalize = (value) => String(value || "").replace(/\s+/g, "").trim();
  const visible = (element) => {
    const rect = element.getBoundingClientRect();
    const style = window.getComputedStyle(element);
    return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden";
  };
  const findLoginAction = () => Array.from(document.querySelectorAll(
    'button, a, [role="button"], input[type="button"], input[type="submit"]'
  )).find((element) => {
    const label = element.innerText || element.value || element.getAttribute("aria-label");
    return normalize(label) === expectedLabel && visible(element) && !element.disabled;
  });

  let attempts = 0;
  let timer = 0;
  const stop = () => {
    if (timer) window.clearInterval(timer);
    timer = 0;
  };
  const tryOpen = () => {
    try {
      const element = findLoginAction();
      if (!element) return false;
      element.click();
      stop();
      return true;
    } catch (_) {
      return false;
    }
  };

  timer = window.setInterval(() => {
    attempts += 1;
    if (tryOpen() || attempts >= 20) stop();
  }, 250);
  window.setTimeout(stop, 6000);
  tryOpen();
})()
