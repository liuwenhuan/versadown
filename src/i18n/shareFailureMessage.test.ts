import { afterEach, describe, expect, it } from 'vitest';
import { setLocalePreference } from './index';
import { shareFailureMessage } from './shareFailureMessage';
import { downloadErrorMessage } from '../lib/queueTask';

afterEach(async () => { await setLocalePreference('zh-CN'); });

describe('share failure messages', () => {
  it('localizes MEGA link failures in Chinese and English', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.mega.linkIncomplete')).toContain('缺少密钥');
    await setLocalePreference('en');
    expect(shareFailureMessage('cloud.error.mega.linkIncomplete')).toContain('key is missing');
    expect(shareFailureMessage('cloud.error.mega.linkUnavailable')).toContain('expired or been removed');
  });

  it('localizes MEGA account sign-in failures in Chinese and English', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.mega.invalidCredentials')).toContain('不正确');
    expect(shareFailureMessage('cloud.error.mega.mfaRequired')).toContain('两步验证');
    await setLocalePreference('en');
    expect(shareFailureMessage('cloud.error.mega.invalidCredentials')).toContain('incorrect');
    expect(shareFailureMessage('cloud.error.mega.mfaInvalid')).toContain('two-factor');
    expect(shareFailureMessage('cloud.error.mega.accountBlocked')).toContain('suspended');
    expect(shareFailureMessage('cloud.error.mega.accessDenied')).toContain('permission');
    expect(shareFailureMessage('cloud.error.mega.storageFull')).toContain('full');
    expect(shareFailureMessage('cloud.error.mega.invalidName')).toContain('Invalid name');
    expect(shareFailureMessage('cloud.error.mega.invalidMove')).toContain('subfolders');
    expect(shareFailureMessage('cloud.error.mega.shareSingleOnly')).toContain('one file or folder');
    expect(shareFailureMessage('cloud.error.mega.shareOptionsUnsupported')).toContain('do not support');
    expect(shareFailureMessage('cloud.error.mega.folderShareUnsupported')).toContain('no share key');
    expect(shareFailureMessage('cloud.error.mega.keysConflict')).toContain('another app');
    expect(shareFailureMessage('cloud.error.mega.keysUnsupported')).toContain('key data');
    expect(shareFailureMessage('cloud.error.mega.copyTooLarge')).toContain('too many items');
  });

  it('turns the quota wait in seconds into whole minutes, rounding up', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:1800')).toContain('约 30 分钟后重试');
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:61')).toContain('约 2 分钟后重试');
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:5')).toContain('约 1 分钟后重试');
    await setLocalePreference('en');
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:1800')).toContain('about 30 min');
  });

  it('uses the plain quota message when MEGA reports no wait time', async () => {
    await setLocalePreference('zh-CN');
    const plain = shareFailureMessage('cloud.error.mega.quotaExceeded');
    expect(plain).toContain('稍后重试');
    expect(plain).not.toContain('约');
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:0')).toBe(plain);
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:abc')).toBe(plain);
  });

  it('names the MEGA account when its own transfer quota is used up', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.mega.accountQuotaExceeded:1800')).toContain('MEGA 账号传输配额已用完（可等待约 30 分钟后重试）');
    expect(shareFailureMessage('cloud.error.mega.accountQuotaExceeded')).toContain('MEGA 账号传输配额已用完，请稍后重试');
    await setLocalePreference('en');
    expect(downloadErrorMessage('cloud.error.mega.accountQuotaExceeded:600')).toContain('account transfer quota is used up (you can retry in about 10 min)');
    expect(shareFailureMessage('cloud.error.mega.accountQuotaExceeded')).toContain('account transfer quota');
    // The anonymous wording is unchanged.
    expect(shareFailureMessage('cloud.error.mega.quotaExceeded:600')).toContain('free MEGA transfer quota');
  });

  it('names a failed MEGA integrity check in both languages', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.mega.integrityFailed')).toContain('完整性校验失败');
    await setLocalePreference('en');
    expect(downloadErrorMessage('cloud.error.mega.integrityFailed')).toContain('integrity check');
  });

  it('localizes OneDrive link and sign-in failures in Chinese and English', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.onedrive.linkUnavailable')).toContain('已失效');
    expect(shareFailureMessage('cloud.error.onedrive.clientIdMissing')).toContain('应用 ID');
    expect(shareFailureMessage('cloud.authorization.onedriveBrowser')).toContain('浏览器');
    await setLocalePreference('en');
    expect(shareFailureMessage('cloud.error.onedrive.authorizationDenied')).toContain('declined access');
    expect(shareFailureMessage('cloud.error.onedrive.serviceBusy')).toContain('busy');
    expect(shareFailureMessage('cloud.error.onedrive.anonymousInterfaceChanged')).toContain('may have changed');
    expect(shareFailureMessage('cloud.error.gdrive.folderPageChanged')).toContain('may have changed');
    expect(shareFailureMessage('cloud.error.gofile.linkExpired')).toContain('expired');
    expect(downloadErrorMessage('cloud.error.onedrive.linkUnsupported')).toContain('not supported');
  });

  it('ignores unrelated text and shows task failures through the queue helper', async () => {
    expect(shareFailureMessage('something else')).toBeUndefined();
    expect(shareFailureMessage('constructor')).toBeUndefined();
    await setLocalePreference('en');
    expect(downloadErrorMessage('download.error.bandwidthLimited')).toContain('quota');
    expect(downloadErrorMessage('cloud.error.mega.quotaExceeded:600')).toContain('about 10 min');
  });
});

describe('Google Drive failure messages', () => {
  it('localizes the sharing, quota and sign-in failures in Chinese and English', async () => {
    await setLocalePreference('zh-CN');
    expect(shareFailureMessage('cloud.error.gdrive.accessDenied')).toContain('知道链接的任何人');
    expect(shareFailureMessage('cloud.error.gdrive.quotaExceeded')).toContain('24 小时');
    expect(shareFailureMessage('cloud.error.gdrive.notConfigured')).toContain('公开分享链接');
    await setLocalePreference('en');
    expect(shareFailureMessage('cloud.error.gdrive.accessDenied')).toContain('anyone with the link');
    expect(shareFailureMessage('cloud.error.gdrive.consentTimeout')).toContain('Timed out');
    expect(downloadErrorMessage('cloud.error.gdrive.quotaExceeded')).toContain('too many downloads');
  });

  it('has a message for every failure the backend can send', async () => {
    const keys = ['linkUnsupported', 'accessDenied', 'notFound', 'quotaExceeded', 'folderTooLarge', 'notConfigured', 'consentDenied', 'consentTimeout', 'clientNotAllowed', 'nativeNotDownloadable', 'rateLimited'];
    for (const locale of ['zh-CN', 'en'] as const) {
      await setLocalePreference(locale);
      for (const key of keys) {
        const text = shareFailureMessage(`cloud.error.gdrive.${key}`);
        expect(text, `${locale} ${key}`).toBeTruthy();
        expect(text).not.toContain('cloud.error.gdrive');
      }
    }
  });
});
