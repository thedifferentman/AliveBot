import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {fileURLToPath} from 'node:url';
import {randomBytes, timingSafeEqual} from 'node:crypto';
import {CodexRPC} from './rpc.mjs';
import {Bridge, atomicJSON} from './bridge.mjs';
import {environment} from './environment.mjs';

const root = path.dirname(fileURLToPath(import.meta.url));
const configFile = path.resolve(process.argv[2] || path.join(root, 'config.json'));
const config = JSON.parse(fs.readFileSync(configFile, 'utf8'));
const resolve = value => path.resolve(path.dirname(configFile), value);
const home = resolve(config.home), workspace = resolve(config.workspace);
const data = path.join(home, 'alivebot');
fs.mkdirSync(data, {recursive: true});
const tokenFile = path.join(home, 'service-token.txt');
if (!fs.existsSync(tokenFile)) fs.writeFileSync(tokenFile, randomBytes(32).toString('hex'), {mode: 0o600});
const token = fs.readFileSync(tokenFile, 'utf8').trim();
if (token.length < 32) throw new Error('Codex service token must contain at least 32 characters');

function trace(record) {
  record.time = new Date().toISOString();
  const file = path.join(data, `trace-${record.time.slice(0, 10)}.jsonl`);
  fs.appendFileSync(file, JSON.stringify(record) + '\n', {mode: 0o600});
}
const rpc = new CodexRPC({executable: config.executable || 'codex', home, cwd: path.dirname(root), environment: environment()});
rpc.on('diagnostic', line => fs.appendFileSync(path.join(data, 'app-server.stderr.log'), line + '\n', {mode: 0o600}));
rpc.on('disconnected', error => console.log(error.message));
const bridge = new Bridge({rpc, workspace, trace});
const authorized = req => {
  const received = Buffer.from(req.headers.authorization ?? '');
  const expected = Buffer.from('Bearer ' + token);
  return received.length === expected.length && timingSafeEqual(received, expected);
};
async function body(req) {
  const chunks = []; let length = 0;
  for await (const chunk of req) {
    length += chunk.length;
    if (length > 8 * 1024 * 1024) throw Object.assign(new Error('Input too large'), {status: 413});
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}
const server = http.createServer(async (req, res) => {
  res.setHeader('Content-Type', 'application/json; charset=utf-8');
  res.setHeader('Cache-Control', 'no-store');
  try {
    if (!authorized(req)) { res.writeHead(401); res.end('{"error":"Unauthorized"}'); return; }
    const url = new URL(req.url, 'http://127.0.0.1');
    let result;
    if (req.method === 'GET' && url.pathname === '/health') {
      await rpc.start(); result = {ready: true, backend: 'codex', pid: process.pid};
    } else if (req.method === 'POST' && url.pathname === '/sessions/ensure') {
      result = await bridge.ensure(await body(req));
    } else {
      const match = /^\/sessions\/(ses_[a-f0-9]{32})\/(prompt|active|interrupt|history|message\/[^/]+)$/.exec(url.pathname);
      if (!match) throw Object.assign(new Error('Unknown route'), {status: 404});
      const [, id, operation] = match;
      if (req.method === 'POST' && operation === 'prompt') result = await bridge.submit(id, await body(req));
      else if (req.method === 'POST' && operation === 'interrupt') { await bridge.interrupt(id); result = {}; }
      else if (req.method === 'GET' && operation === 'active') result = {active: await bridge.active(id)};
      else if (req.method === 'GET' && operation === 'history') result = await bridge.history(id, Number(url.searchParams.get('after') || 0));
      else if (req.method === 'GET' && operation.startsWith('message/')) result = await bridge.message(id, decodeURIComponent(operation.slice(8)));
      else throw Object.assign(new Error('Method not allowed'), {status: 405});
    }
    res.end(JSON.stringify(result));
  } catch (error) {
    res.writeHead(error.status || 503);
    res.end(JSON.stringify({error: error.message}));
  }
});
await rpc.start();
server.on('error', async error => {
  console.error(`AliveBot Codex service failed: ${error.message}`);
  await rpc.close(); process.exit(1);
});
server.listen(config.port, '127.0.0.1', () => {
  atomicJSON(path.join(data, 'runtime.json'), {pid: process.pid, startedAt: Date.now(), config: configFile,
    home, port: config.port, script: fileURLToPath(import.meta.url)});
  console.log(`AliveBot Codex service ready at 127.0.0.1:${config.port}`);
});
let closing = false;
async function close() {
  if (closing) return; closing = true;
  server.close(); await rpc.close(); process.exit(0);
}
process.on('SIGINT', close); process.on('SIGTERM', close);
process.stdin.setEncoding('utf8');
process.stdin.on('data', line => { if (line.includes('stop-codex')) void close(); });
