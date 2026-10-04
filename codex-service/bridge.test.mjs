import {test} from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {Bridge} from './bridge.mjs';
import {environment} from './environment.mjs';

class FakeRPC extends EventEmitter {
  generation = 1;
  calls = [];
  turns = [];
  async start() {}
  async call(method, params) {
    this.calls.push({method, params});
    if (method === 'thread/start') { this.cwd = params.cwd; return {thread: {id: 'native-thread', cwd: this.cwd}}; }
    if (method === 'thread/resume' || method === 'thread/read') return {thread: {id: 'native-thread', cwd: this.cwd, turns: this.turns}};
    if (method === 'turn/start') return {turn: {id: 'active-turn'}};
    return {};
  }
}
function fixture(t) {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'alivebot-bridge-'));
  t.after(() => fs.rmSync(workspace, {recursive: true, force: true}));
  const directory = path.join(workspace, '12345'); fs.mkdirSync(directory);
  const rpc = new FakeRPC(), trace = [];
  const bridge = new Bridge({rpc, workspace, trace: record => trace.push(record)});
  const id = 'ses_' + 'a'.repeat(32);
  return {bridge, rpc, trace, workspace, directory, id,
    ensure: {id, directory, mayCreate: true, model: 'gpt-6.1-sol', effort: 'high', instructions: 'BOT_PROMPT'}};
}

test('idle record-only input is raw text injection and never starts a turn', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  await f.bridge.submit(f.id, {id: 'msg_background', prompt: {text: '<img:https://example.invalid/a.png>'}, resume: false});
  const call = f.rpc.calls.at(-1);
  assert.equal(call.method, 'thread/inject_items');
  assert.equal(call.params.items[0].content[0].text, '<img:https://example.invalid/a.png>');
  assert.equal(f.rpc.calls.some(call => call.method === 'turn/start'), false);
});

test('active inputs, including own record-only input, always steer with the active turn ID', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  await f.bridge.submit(f.id, {id: 'msg_start', prompt: {text: 'start'}, resume: true});
  await f.bridge.submit(f.id, {id: 'msg_own', prompt: {text: 'non-model own message'}, resume: false});
  assert.equal(f.rpc.calls.at(-1).method, 'turn/steer');
  assert.equal(f.rpc.calls.at(-1).params.expectedTurnId, 'active-turn');
  const count = f.rpc.calls.length;
  await f.bridge.submit(f.id, {id: 'msg_own', prompt: {text: 'duplicate'}, resume: false});
  assert.equal(f.rpc.calls.length, count, 'accepted input cannot be repeated');
});

test('public commentary and final output persist once; tools and reasoning are never delivered', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  for (const [id, phase] of [['comment', 'commentary'], ['final', 'final_answer']]) {
    f.rpc.emit('notification', 'item/completed', {threadId: 'native-thread', turnId: 'turn',
      item: {type: 'agentMessage', id, text: phase, phase}});
  }
  const secret = 'TOOL_ARGUMENT_AND_RESULT_MUST_NOT_APPEAR';
  f.rpc.emit('notification', 'item/completed', {threadId: 'native-thread', turnId: 'turn',
    item: {type: 'commandExecution', id: 'tool', status: 'completed', command: secret, aggregatedOutput: secret}});
  f.rpc.emit('notification', 'item/completed', {threadId: 'native-thread', turnId: 'turn',
    item: {type: 'reasoning', id: 'private', text: secret}});
  assert.equal(JSON.stringify(f.trace).includes(secret), false);
  assert.deepEqual(f.trace.filter(record => record.type === 'tool'),
    [{type: 'tool', directory: f.directory, name: 'exec_command', status: 'completed'}]);
  const history = await f.bridge.history(f.id, 0);
  assert.equal(history.data.filter(event => event.type === 'assistant.completed').length, 2);
  const restartedRPC = new FakeRPC(); restartedRPC.cwd = f.directory;
  restartedRPC.turns = [{id: 'turn', status: 'completed', items: [
    {type: 'agentMessage', id: 'final', text: 'final_answer', phase: 'final_answer'},
  ]}];
  const restarted = new Bridge({rpc: restartedRPC, workspace: f.workspace});
  await restarted.ensure({...f.ensure, mayCreate: false});
  assert.equal((await restarted.history(f.id, 0)).data.filter(e => e.type === 'assistant.completed').length, 2);
  assert.equal((await restarted.message(f.id, 'final')).data.content[0].text, 'final_answer');
});

test('changed instructions reload the idle thread without starting a model turn', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  await f.bridge.ensure({...f.ensure, instructions: 'CHANGED_PROMPT'});
  assert.equal(f.rpc.calls.at(-2).method, 'thread/unsubscribe');
  assert.equal(f.rpc.calls.at(-1).method, 'thread/resume');
  assert.equal(f.rpc.calls.at(-1).params.baseInstructions, 'CHANGED_PROMPT');
  assert.equal(f.rpc.calls.some(c => c.method === 'turn/start'), false);
});

test('directory escape and missing saved session are rejected without discarding state', async t => {
  const f = fixture(t);
  await assert.rejects(f.bridge.ensure({...f.ensure, directory: path.dirname(f.workspace)}), /outside/);
  await assert.rejects(f.bridge.ensure({...f.ensure, mayCreate: false}), /missing/);
});

test('uncertain input acceptance is preserved and never retried blindly', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  const original = f.rpc.call.bind(f.rpc);
  f.rpc.call = async (method, params) => {
    if (method === 'thread/inject_items') throw Object.assign(new Error('timeout'), {uncertain: true});
    return original(method, params);
  };
  await assert.rejects(f.bridge.submit(f.id, {id: 'msg_uncertain', prompt: {text: 'record'}, resume: false}), /timeout/);
  await assert.rejects(f.bridge.submit(f.id, {id: 'msg_uncertain', prompt: {text: 'record'}, resume: false}), /uncertain/);
  assert.equal(f.bridge.get(f.id).pending.id, 'msg_uncertain');
});

test('a completion notification arriving before the start response cannot leave a stuck active turn', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  const original = f.rpc.call.bind(f.rpc);
  f.rpc.call = async (method, params) => {
    if (method === 'turn/start') {
      f.rpc.emit('notification', 'turn/completed', {threadId: 'native-thread', turn: {id: 'fast-turn', status: 'completed'}});
      return {turn: {id: 'fast-turn'}};
    }
    return original(method, params);
  };
  await f.bridge.submit(f.id, {id: 'msg_fast', prompt: {text: 'start'}, resume: true});
  assert.equal(await f.bridge.active(f.id), false);
  await f.bridge.submit(f.id, {id: 'msg_idle', prompt: {text: 'record only'}, resume: false});
  assert.equal(f.rpc.calls.at(-1).method, 'thread/inject_items');
});

test('explicit reset can recover a missing transport session', async t => {
  const f = fixture(t);
  await f.bridge.interrupt(f.id);
  assert.equal(f.rpc.calls.length, 0);
});

test('standalone launch gets Windows proxy without inheriting gateway credentials or personal API overrides', () => {
  const result = environment({PATH: 'tools', OPENCODE_SERVER_PASSWORD: 'PRIVATE', LLMSERVER_PORT: '3000',
    CODEX_THREAD_ID: 'personal', OPENAI_BASE_URL: 'http://wrong-provider'}, 'win32', () =>
    '    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    127.0.0.1:7890\n');
  assert.equal(result.HTTPS_PROXY, 'http://127.0.0.1:7890');
  assert.equal(result.PATH, 'tools');
  for (const key of ['OPENCODE_SERVER_PASSWORD', 'LLMSERVER_PORT', 'CODEX_THREAD_ID', 'OPENAI_BASE_URL']) assert.equal(result[key], undefined);
  assert.match(result.NO_PROXY, /127\.0\.0\.1/);
});

test('fast mode is explicit per new turn and never modifies a running turn through steer', async t => {
  const f = fixture(t); await f.bridge.ensure(f.ensure);
  await f.bridge.submit(f.id, {id: 'msg_fast', prompt: {text: 'start fast'}, resume: true, fastMode: true});
  assert.equal(f.rpc.calls.at(-1).params.serviceTierForTurn, 'priority');
  assert.equal(f.rpc.calls.at(-1).params.serviceTier, undefined);
  await f.bridge.submit(f.id, {id: 'msg_steer', prompt: {text: 'append'}, resume: false, fastMode: false});
  assert.equal(f.rpc.calls.at(-1).method, 'turn/steer');
  assert.equal(f.rpc.calls.at(-1).params.serviceTierForTurn, undefined);
  f.rpc.emit('notification', 'turn/completed', {threadId: 'native-thread', turn: {id: 'active-turn', status: 'completed'}});
  await f.bridge.submit(f.id, {id: 'msg_standard', prompt: {text: 'start standard'}, resume: true});
  assert.equal(f.rpc.calls.at(-1).params.serviceTierForTurn, 'default');
});
