(function (_provider, _hosts, username, password, attempt) {
  'use strict';
  if (location.origin !== 'https://i.xunlei.com') return;
  const key = '__alldownloaderXunleiPassword';
  if (window[key]) return;
  const state = {submitted:false, error:''}; window[key] = state;
  const visible = element => element && element.getClientRects().length > 0 && !element.disabled;
  const started = Date.now();
  const clear = () => { username = ''; password = ''; clearInterval(timer); };
  const timer = setInterval(() => {
    if (Date.now()-started > 120000) {
      state.error='plugin_update_required';
      window.parent.postMessage({type:'alldownloader-xunlei-password',error:state.error},'https://pan.xunlei.com');
      clear(); return;
    }
    const fields = Array.from(document.querySelectorAll('input.xlubase-login-input'));
    const secret = fields.find(e => e.type === 'password' && visible(e));
    if (!secret) {
      Array.from(document.querySelectorAll('a,button,span,div')).find(e => visible(e) && e.textContent.trim()==='账号密码登录')?.click();
      return;
    }
    const account = fields.find(e => ['text','tel','email'].includes(e.type) && visible(e));
    if (!account) return;
    const set = (input,value) => {Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,value);input.dispatchEvent(new Event('input',{bubbles:true}));input.dispatchEvent(new Event('change',{bubbles:true}));};
    set(account,username); set(secret,password);
    const agree = document.querySelector('input.xlucommon-login-checkbox[type="checkbox"]');
    if (agree && !agree.checked) { agree.click(); return; }
    const button = Array.from(document.querySelectorAll('button')).find(e => visible(e) && e.textContent.trim()==='登录');
    if (!button) return;
    // Exactly one submission. The official parent iframe callback performs
    // the OAuth handoff; never navigate to the personal account centre.
    state.submitted=true; clear(); button.click();
  },300);
})
