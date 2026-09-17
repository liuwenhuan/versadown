// Executed only in the provider's official document. The official form owns
// encryption, dynamic login parameters and any verification challenge.
(function (provider, allowed, username, password, attemptId) {
  if (!Array.isArray(allowed) || !allowed.every(host => typeof host === 'string' && host)) return;
  if (location.protocol !== 'https:' || (location.port && location.port !== '443') || !allowed.includes(location.hostname)) return;
  const submittedKey = 'alldownloader-login-' + attemptId;
  // This contains only progress flags, never credential values. It lets the
  // native host distinguish a page that is still loading from an official
  // security challenge without inspecting the login form's private contents.
  const status = {
    formReady: false, valuesApplied: false, agreementAccepted: false,
    challengeRequired: false, submitted: false, officialResult: 'pending',
  };
  window.__alldownloaderPasswordLoginStatus = status;
  const report = () => {
    if (document.documentElement) {
      document.documentElement.setAttribute('data-alldownloader-password-login', JSON.stringify(status));
    }
  };
  try {
    if (sessionStorage.getItem(submittedKey)) return;
  } catch (_) {
    // Some official cross-origin login frames deny Web Storage to injected
    // scripts. The in-page attempt guard below remains sufficient here.
  }
  const visible = element => element && element.getClientRects().length > 0
    && !element.disabled && getComputedStyle(element).visibility !== 'hidden';
  const first = selector => [...document.querySelectorAll(selector)].find(visible);
  const setValue = (input, value) => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const started = Date.now();
  const timer = setInterval(() => {
    report();
    // The official login bundle/iframe can arrive well after the landing
    // document. Keep waiting within the native window's 180-second deadline.
    let submitted = false;
    try { submitted = sessionStorage.getItem(submittedKey); } catch (_) {}
    if (Date.now() - started > 120000 || submitted) {
      clearInterval(timer); username = ''; password = ''; return;
    }
    // Tianyi renders inputs below this overlay before its asynchronous page
    // configuration has bound the real login handlers. An early click is a
    // no-op, but would permanently consume our one-submit guard. Its official
    // initializer hides the overlay only after setting up the login modes.
    if (provider === 'tianyi' && first('#j-login-loading')) return;
    if (!first('input[type="password"]')) {
      const passwordTab = [...document.querySelectorAll('a,button,span,li,div')].find(element =>
        visible(element) && /^(账号登录|帐号登录|账号密码登录|帐号密码登录|用户名登录|密码登录)$/.test(element.textContent.trim()));
      if (passwordTab) passwordTab.click();
    }
    const passwordInput = first('input[type="password"]');
    const usernameInput = first('input[name="userName"],input[name="username"],input[name="account"],input#userName,input#username,input[id$="__userName"]');
    if (!passwordInput || !usernameInput) return;
    status.formReady = true; report();
    const form = passwordInput.closest('form');
    if (form && !form.contains(usernameInput)) return;
    if (form?.getAttribute('action')) {
      const action = new URL(form.action, location.href);
      if (action.protocol !== 'https:' || (action.port && action.port !== '443') || !allowed.includes(action.hostname)) return;
    }
    setValue(usernameInput, username);
    setValue(passwordInput, password);
    status.valuesApplied = true; report();
    if (provider === 'tianyi') {
      const agreement = first('#j-agreement-box');
      if (agreement && !agreement.classList.contains('checkbox-icon2')) {
        agreement.click();
        if (!agreement.classList.contains('checkbox-icon2')) return;
      }
    } else if (provider === 'baidu') {
      // Passport keeps its submit control disabled until this official
      // checkbox has dispatched the site's own agreement/change handler.
      const agreement = first('input[type="checkbox"][id$="__isAgree"]');
      if (agreement && !agreement.checked) agreement.click();
    }
    status.agreementAccepted = true; report();
    // Never submit an empty verification field or operate a challenge widget.
    const challenge = first('input[name="verifyCode"],input[name="validateCode"],input[id$="__verifyCode"],input[name="smsCode"]');
    if (challenge && !challenge.value.trim()) {
      status.challengeRequired = true; report();
      clearInterval(timer); username = ''; password = ''; return;
    }
    const submit = [...(form || document).querySelectorAll('button,input[type="submit"],input[type="button"],a')].find(element =>
      visible(element) && /^(登录|登\s+录|立即登录)$/.test((element.value || element.textContent).trim()));
    if (!submit) return;
    status.submitted = true; report();
    try { sessionStorage.setItem(submittedKey, 'submitted'); } catch (_) {}
    clearInterval(timer); username = ''; password = '';
    submit.click();
  }, 300);
})
