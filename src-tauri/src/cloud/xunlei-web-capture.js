(collect => {
  // One in-flight capture per official page. A human challenge may outlive
  // many native polling calls; never launch another captcha for each poll.
  const key = '__alldownloader_xunlei_capture';
  const current = window[key];
  const account = window.$nuxt?.$store?.state?.users?.curUser;
  const liveId = [account?.userId, account?.user_id, account?.userid, account?.sub]
    .find(value => typeof value === 'string' && value && value !== '0' && !value.startsWith('AMS='));
  if (current?.done) {
    delete window[key];
    // Recheck synchronously at consumption: a completed Promise can sit here
    // until the next native poll while the user changes accounts or logs out.
    if (current.value?.user_id && (account === null
      || (liveId && liveId !== current.value.user_id)
      || (current.initialLiveId && !liveId))) return {done: true, value: null};
    return {done: true, value: current.value};
  }
  if (!current) {
    const slot = {done: false, initialLiveId: liveId};
    window[key] = slot;
    Promise.resolve().then(collect).then(value => {
      slot.value = value;
      slot.done = true;
    }, () => {
      slot.value = null;
      slot.done = true;
    });
  }
  return {done: false};
})
