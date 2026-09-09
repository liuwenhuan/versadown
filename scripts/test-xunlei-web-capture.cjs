const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

const source = fs.readFileSync('src-tauri/src/cloud/xunlei-web-capture.js', 'utf8');
test('authorization polls await one async capture without reopening its challenge', async () => {
  const context = vm.createContext({window: {}});
  const capture = vm.runInContext(source, context);
  let finish, calls = 0;
  const collect = () => { calls++; return new Promise(resolve => { finish = resolve; }); };
  assert.equal(capture(collect).done, false);
  await Promise.resolve();
  assert.equal(capture(collect).done, false);
  assert.equal(calls, 1);
  finish({user_id: 'fixture-account'});
  await new Promise(resolve => setImmediate(resolve));
  const result = capture(collect);
  assert.equal(result.done, true);
  assert.equal(result.value.user_id, 'fixture-account');
  assert.equal(calls, 1);
  assert.equal(capture(collect).done, false);
  await Promise.resolve();
  assert.equal(calls, 2);
});

test('capture failures discard rejected details and allow a later fresh attempt', async () => {
  const capture = vm.runInNewContext(source, {window: {}});
  assert.equal(capture(() => Promise.reject(new Error('sensitive-provider-response'))).done, false);
  await new Promise(resolve => setImmediate(resolve));
  const failed = capture(() => { throw Error('must not run again yet'); });
  assert.equal(failed.done, true);
  assert.equal(failed.value, null);
  assert.equal(JSON.stringify(failed).includes('sensitive'), false);
  assert.equal(capture(() => ({user_id:'retry'})).done, false);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(capture(() => null).value.user_id, 'retry');
});

test('a completed capture cannot deliver the previous account after a between-polls switch', async () => {
  const users = {curUser: {userId:'account-A'}};
  const capture = vm.runInNewContext(source, {window: {$nuxt: {$store: {state: {users}}}}});
  capture(() => ({user_id:'account-A', access_token:'fixture-secret'}));
  await new Promise(resolve => setImmediate(resolve));
  users.curUser = {userId:'account-B'};
  assert.equal(capture(() => null).value, null);
});

test('logout between completion and consumption discards the old credential', async () => {
  const users = {curUser: {userId:'account-A'}};
  const capture = vm.runInNewContext(source, {window: {$nuxt: {$store: {state: {users}}}}});
  capture(() => ({user_id:'account-A'}));
  await new Promise(resolve => setImmediate(resolve));
  users.curUser = null;
  assert.equal(capture(() => null).value, null);
});
