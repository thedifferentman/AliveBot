import fs from 'node:fs';
import path from 'node:path';
import {createHash, randomBytes} from 'node:crypto';

export function atomicJSON(file, value) {
  fs.mkdirSync(path.dirname(file), {recursive: true});
  const temporary = `${file}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`;
  const descriptor = fs.openSync(temporary, 'w', 0o600);
  try {
    fs.writeFileSync(descriptor, JSON.stringify(value, null, 2) + '\n');
    fs.fsyncSync(descriptor);
  } finally { fs.closeSync(descriptor); }
  fs.renameSync(temporary, file);
}

function failure(message, status = 409) {
  const error = new Error(message); error.status = status; return error;
}
function settingsHash(options) {
  return createHash('sha256').update(JSON.stringify(options)).digest('hex');
}
function toolName(item) {
  if (item.type === 'commandExecution') return 'exec_command';
  if (item.type === 'fileChange') return 'apply_patch';
  if (item.type === 'webSearch') return 'web_search';
  if (item.type === 'imageView') return 'view_image';
  if (item.type === 'mcpToolCall') return `${item.server}.${item.tool}`;
  if (item.type === 'dynamicToolCall') return item.tool;
  return null;
}

/** Only public assistant messages are adapted into the QQ delivery journal. */
export class Bridge {
  constructor({rpc, workspace, trace = () => {}}) {
    Object.assign(this, {rpc, trace});
    fs.mkdirSync(workspace, {recursive: true});
    this.workspace = fs.realpathSync(workspace);
    this.sessions = new Map(); this.native = new Map(); this.locks = new Map();
    for (const group of fs.readdirSync(this.workspace, {withFileTypes: true})) {
      if (!group.isDirectory() || !/^\d+$/.test(group.name)) continue;
      const directory = this.directory(path.join(this.workspace, group.name));
      const folder = path.join(directory, '.alivebot', 'codex');
      if (!fs.existsSync(folder)) continue;
      for (const file of fs.readdirSync(folder)) {
        if (!/^ses_[a-f0-9]{32}\.json$/.test(file)) continue;
        const state = JSON.parse(fs.readFileSync(path.join(folder, file), 'utf8'));
        if (state.directory !== directory || state.id + '.json' !== file || !state.threadId) {
          throw new Error(`Invalid saved Codex transport state: ${file}`);
        }
        this.register(state);
      }
    }
    rpc.on('notification', (method, params) => this.notification(method, params));
    rpc.on('disconnected', () => {
      for (const state of this.sessions.values()) state.loaded = 0;
    });
  }

  directory(value) {
    const directory = fs.realpathSync(value);
    if (path.dirname(directory).toLowerCase() !== this.workspace.toLowerCase()
      || !/^\d+$/.test(path.basename(directory))) throw failure('Group directory is outside the configured workspace', 400);
    return directory;
  }
  file(state) { return path.join(state.directory, '.alivebot', 'codex', state.id + '.json'); }
  save(state) {
    const {loaded, ...durable} = state;
    atomicJSON(this.file(state), durable);
  }
  register(state) {
    state.loaded = 0;
    state.applied ??= {}; state.messages ??= {}; state.events ??= []; state.seq ??= 0;
    state.suppressedTurns ??= [];
    state.finishedTurns ??= [];
    this.sessions.set(state.id, state); this.native.set(state.threadId, state);
  }
  get(id) {
    const state = this.sessions.get(id);
    if (!state) throw failure('Saved Codex session is missing; use /new to explicitly start over', 404);
    return state;
  }
  async serial(id, task) {
    const previous = this.locks.get(id) ?? Promise.resolve();
    const next = previous.catch(() => {}).then(task);
    this.locks.set(id, next);
    try { return await next; } finally { if (this.locks.get(id) === next) this.locks.delete(id); }
  }
  event(state, type, data = {}) {
    state.events.push({type, durable: {seq: ++state.seq}, data});
    // The consumer must explicitly acknowledge before old journal records are pruned.
  }
  publicItem(state, item, turnId) {
    if (item.type === 'userMessage') {
      const id = item.clientId ?? item.client_id;
      if (id) state.applied[id] = true;
      return;
    }
    if (item.type !== 'agentMessage' || !item.text?.trim() || state.messages[item.id]
      || state.suppressedTurns.includes(turnId)) return;
    state.messages[item.id] = {
      id: item.id, type: 'assistant', finish: 'stop', phase: item.phase ?? null,
      content: [{type: 'text', text: item.text}], error: null,
    };
    this.event(state, 'assistant.completed', {assistantMessageID: item.id});
    this.trace({type: 'output', directory: state.directory, threadId: state.threadId,
      turnId, phase: item.phase, text: item.text});
  }
  complete(state, turn) {
    if (!state.finishedTurns.includes(turn.id)) state.finishedTurns.push(turn.id);
    state.finishedTurns = state.finishedTurns.slice(-256);
    if (state.activeTurn === turn.id) state.activeTurn = null;
    for (const item of turn.items ?? []) this.publicItem(state, item, turn.id);
    if (turn.status === 'failed') {
      const id = `error:${turn.id}`;
      if (!state.messages[id]) {
        state.messages[id] = {id, type: 'assistant', finish: 'error', content: [],
          error: {message: turn.error?.message ?? 'Codex turn failed'}};
        this.event(state, 'assistant.completed', {assistantMessageID: id});
      }
    }
  }
  notification(method, params) {
    const state = this.native.get(params.threadId);
    if (!state) return;
    const item = params.item;
    if (method === 'turn/started') {
      if (!state.finishedTurns.includes(params.turn.id)) state.activeTurn = params.turn.id;
      this.trace({type: 'turn', status: 'started', directory: state.directory, turnId: params.turn.id});
    } else if (method === 'turn/completed') {
      this.complete(state, params.turn);
      this.trace({type: 'turn', status: params.turn.status, directory: state.directory, turnId: params.turn.id});
    } else if (method === 'item/completed') {
      this.publicItem(state, item, params.turnId);
    }
    const name = item && toolName(item);
    if (name && ['item/started', 'item/completed'].includes(method)) {
      this.event(state, 'tool.activity');
      this.trace({type: 'tool', directory: state.directory, name,
        status: method === 'item/started' ? 'started' : item.status ?? 'completed'});
    }
    if (['turn/started', 'turn/completed', 'item/completed', 'item/started'].includes(method)) this.save(state);
  }
  params(state) {
    return {cwd: state.directory, model: state.options.model, approvalPolicy: 'never',
      baseInstructions: state.options.instructions, developerInstructions: '',
      config: {'model_reasoning_effort': state.options.effort}};
  }
  async loaded(state) {
    await this.rpc.start();
    if (state.loaded === this.rpc.generation) return;
    const result = await this.rpc.call('thread/resume', {threadId: state.threadId, ...this.params(state)});
    if (path.resolve(result.thread.cwd).toLowerCase() !== state.directory.toLowerCase()) {
      throw failure('Saved Codex thread belongs to another working directory');
    }
    state.activeTurn = null;
    for (const turn of result.thread.turns ?? []) {
      if (turn.status === 'inProgress') state.activeTurn = turn.id;
      for (const item of turn.items ?? []) this.publicItem(state, item, turn.id);
      if (['completed', 'failed', 'interrupted'].includes(turn.status)) this.complete(state, turn);
    }
    if (result.thread.status?.type === 'idle') state.activeTurn = null;
    state.loaded = this.rpc.generation; this.save(state);
    // A lost reply to a mutating request must be reconciled, never blindly replayed.
    if (state.pending && state.applied[state.pending.id]) { state.pending = null; this.save(state); }
  }
  async refresh(state) {
    const hash = settingsHash(state.options);
    if (state.settingsHash === hash || state.activeTurn) return;
    // Unsubscribe unloads this idle thread before instruction overrides are reapplied.
    await this.rpc.call('thread/unsubscribe', {threadId: state.threadId});
    state.loaded = 0;
    await this.loaded(state);
    state.settingsHash = hash; this.save(state);
    this.trace({type: 'instructions', directory: state.directory, text: state.options.instructions});
  }
  async ensure({id, directory, mayCreate, model, effort, instructions}) {
    if (!/^ses_[a-f0-9]{32}$/.test(id)) throw failure('Invalid transport session ID', 400);
    return this.serial(id, async () => {
      directory = this.directory(directory);
      let state = this.sessions.get(id);
      if (!state) {
        if (!mayCreate) throw failure('Saved Codex transport state is missing; use /new', 404);
        await this.rpc.start();
        state = {id, directory, options: {model, effort, instructions}, seq: 0,
          events: [], messages: {}, applied: {}, suppressedTurns: [], activeTurn: null};
        const result = await this.rpc.call('thread/start', {...this.params(state), ephemeral: false});
        state.threadId = result.thread.id;
        this.register(state); state.loaded = this.rpc.generation;
        state.settingsHash = settingsHash(state.options); this.save(state);
        this.trace({type: 'instructions', directory, text: instructions});
      } else {
        if (state.directory !== directory) throw failure('Session directory mismatch', 400);
        state.options = {model, effort, instructions}; this.save(state);
        await this.loaded(state); await this.refresh(state);
      }
      return {threadId: state.threadId};
    });
  }
  async reconcileTurn(state) {
    const result = await this.rpc.call('thread/read', {threadId: state.threadId, includeTurns: true});
    state.activeTurn = null;
    for (const turn of result.thread.turns ?? []) {
      if (turn.status === 'inProgress') state.activeTurn = turn.id;
      for (const item of turn.items ?? []) this.publicItem(state, item, turn.id);
      if (['completed', 'failed', 'interrupted'].includes(turn.status)) this.complete(state, turn);
    }
    if (result.thread.status?.type === 'idle') state.activeTurn = null;
    this.save(state);
  }
  async submit(id, {id: inputId, prompt, resume, fastMode = false}) {
    if (typeof fastMode !== 'boolean') throw failure('fastMode must be a boolean', 400);
    return this.serial(id, async () => {
      const state = this.get(id); await this.loaded(state);
      if (state.applied[inputId]) return {accepted: true};
      if (state.pending) {
        await this.reconcileTurn(state);
        if (state.applied[state.pending.id]) { state.pending = null; this.save(state); }
        else throw failure('Previous input delivery is uncertain; refusing duplicate delivery. Inspect the saved transport state.');
      }
      await this.refresh(state);
      if (prompt.files?.length) throw failure('Legacy image attachments must be converted to URL text before delivery', 400);
      const text = prompt.text;
      if (typeof text !== 'string') throw failure('Prompt text is required', 400);
      state.pending = {id: inputId}; this.save(state);
      this.trace({type: 'input', directory: state.directory, id: inputId, text});
      try {
        for (let attempt = 0; attempt < 3; attempt++) {
          try {
            if (state.activeTurn) {
              await this.rpc.call('turn/steer', {threadId: state.threadId, expectedTurnId: state.activeTurn,
                clientUserMessageId: inputId, input: [{type: 'text', text}]});
            } else if (resume) {
              const result = await this.rpc.call('turn/start', {threadId: state.threadId,
                clientUserMessageId: inputId, input: [{type: 'text', text}], effort: state.options.effort,
                serviceTierForTurn: fastMode ? 'priority' : 'default'});
              state.activeTurn = state.finishedTurns.includes(result.turn.id) ? null : result.turn.id;
            } else {
              await this.rpc.call('thread/inject_items', {threadId: state.threadId, items: [
                {type: 'message', id: inputId, role: 'user', content: [{type: 'input_text', text}]},
              ]});
            }
            state.applied[inputId] = true; state.pending = null; this.save(state);
            return {accepted: true};
          } catch (error) {
            // Only a definite precondition rejection permits safe mode reevaluation.
            if (error.rpcError && /(?:active turn|turn.*mismatch|expected.*turn|no.*turn|not.*active)/i.test(error.message)
              && attempt < 2) { await this.reconcileTurn(state); continue; }
            throw error;
          }
        }
      } catch (error) {
        if (error.rpcError || error.notSent) { state.pending = null; this.save(state); }
        throw error;
      }
    });
  }
  async interrupt(id) {
    return this.serial(id, async () => {
      if (!this.sessions.has(id)) return;
      const state = this.get(id); await this.loaded(state);
      if (!state.activeTurn) return;
      const turnId = state.activeTurn;
      state.suppressedTurns.push(turnId); this.save(state);
      await this.rpc.call('turn/interrupt', {threadId: state.threadId, turnId});
      const deadline = Date.now() + 10000;
      while (state.activeTurn && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 25));
      if (state.activeTurn) { await this.reconcileTurn(state); }
      if (state.activeTurn) throw failure('Codex interruption has not completed');
    });
  }
  async active(id) { const state = this.get(id); await this.loaded(state); return Boolean(state.activeTurn); }
  async history(id, after) {
    const state = this.get(id); await this.loaded(state);
    if (!Number.isSafeInteger(after) || after < 0 || after > state.seq) throw failure('Invalid delivery cursor', 400);
    const retained = state.events.filter(event => event.durable.seq > after);
    if (retained.length !== state.events.length) { state.events = retained; this.save(state); }
    const data = state.events.filter(event => event.durable.seq > after).slice(0, 100);
    return {data, hasMore: state.events.some(event => event.durable.seq > (data.at(-1)?.durable.seq ?? after))};
  }
  async message(id, messageId) {
    const state = this.get(id); await this.loaded(state);
    const message = state.messages[messageId];
    if (!message) throw failure('Assistant message is missing', 404);
    return {data: message};
  }
}
