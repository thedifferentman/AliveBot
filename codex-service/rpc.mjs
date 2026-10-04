import {EventEmitter} from 'node:events';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';

export class CodexRPC extends EventEmitter {
  constructor({executable, home, cwd, environment = process.env}) {
    super();
    Object.assign(this, {executable, home, cwd, environment});
    this.pending = new Map();
    this.serial = 0;
    this.generation = 0;
  }

  async start() {
    if (this.ready) return;
    if (this.starting) return this.starting;
    this.starting = this.open().finally(() => { this.starting = null; });
    return this.starting;
  }

  async open() {
    const child = spawn(this.executable, ['app-server', '--stdio'], {
      cwd: this.cwd, env: {...this.environment, CODEX_HOME: this.home},
      windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.child = child;
    this.generation++;
    let exited = false;
    const end = error => {
      if (exited) return;
      exited = true;
      if (this.child !== child) return;
      this.ready = false;
      for (const {reject, timer} of this.pending.values()) {
        clearTimeout(timer); reject(error);
      }
      this.pending.clear();
      this.emit('disconnected', error);
    };
    child.on('error', error => end(new Error(`Cannot start Codex: ${error.message}`)));
    child.on('exit', code => end(new Error(`Codex app-server exited (${code})`)));
    child.stdin.on('error', error => end(new Error(`Codex connection failed: ${error.message}`)));
    createInterface({input: child.stderr}).on('line', line => {
      // Never expose credentials from diagnostics, or unbounded command output.
      const safe = line.replace(/(?:Bearer\s+\S+|sk-[\w-]+|eyJ[\w.-]{30,})/g, '[redacted]');
      this.emit('diagnostic', safe.slice(0, 2000));
    });
    createInterface({input: child.stdout}).on('line', line => {
      let message;
      try { message = JSON.parse(line); } catch { return; }
      if (message.method && message.id !== undefined) {
        // QQ bots do not have a human approval/question dialog.
        const result = /requestApproval$/.test(message.method) ? {decision: 'decline'} : null;
        this.send(result ? {id: message.id, result} : {
          id: message.id, error: {code: -32600, message: 'Interactive requests are unavailable; use ordinary chat text.'},
        });
      } else if (message.id !== undefined) {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id); clearTimeout(pending.timer);
        if (message.error) {
          const error = new Error(message.error.message || 'Codex request failed');
          error.code = message.error.code; error.rpcError = true;
          pending.reject(error);
        } else pending.resolve(message.result);
      } else if (message.method) this.emit('notification', message.method, message.params ?? {});
    });
    try {
      await this.call('initialize', {
        clientInfo: {name: 'alivebot', version: '0.1.0'}, capabilities: {experimentalApi: false},
      });
      this.send({method: 'initialized', params: {}});
      this.ready = true;
    } catch (error) {
      child.kill(); throw error;
    }
  }

  send(message) {
    if (!this.child?.stdin.writable) throw new Error('Codex connection is unavailable');
    this.child.stdin.write(JSON.stringify(message) + '\n');
  }

  call(method, params, timeout = 30000) {
    const id = ++this.serial;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        const error = new Error(`Codex request timed out: ${method}`);
        error.uncertain = true; reject(error);
      }, timeout);
      this.pending.set(id, {resolve, reject, timer});
      try { this.send({id, method, params}); } catch (error) {
        error.notSent = true;
        clearTimeout(timer); this.pending.delete(id); reject(error);
      }
    });
  }

  async close() {
    this.ready = false;
    const child = this.child;
    if (!child || child.exitCode !== null) return;
    const ended = new Promise(resolve => child.once('exit', resolve));
    child.stdin.end();
    const timer = setTimeout(() => child.kill(), 3000);
    await ended; clearTimeout(timer);
  }
}
