import {execFileSync} from 'node:child_process';

// Read Windows' proxy directly so starting from a plain terminal works too.
export function environment(source = process.env, platform = process.platform, registry = () =>
  execFileSync('reg.exe', ['query', 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings'],
    {encoding: 'utf8', windowsHide: true, timeout: 2000, stdio: ['ignore', 'pipe', 'ignore']})) {
  const env = {...source};
  for (const key of Object.keys(env)) {
    if (/^(?:OPENCODE_|LLMSERVER_)/i.test(key) || ['CODEX_THREAD_ID', 'CODEX_INTERNAL_ORIGINATOR_OVERRIDE',
      'OPENAI_API_KEY', 'OPENAI_BASE_URL', 'CODEX_API_KEY', 'CHATGPT_BASE_URL'].includes(key)) delete env[key];
  }
  if (platform === 'win32' && !Object.keys(env).some(key => /^(?:https?|all)_proxy$/i.test(key) && env[key])) {
    try {
      const text = registry();
      const enabled = /^\s*ProxyEnable\s+REG_DWORD\s+(\S+)/mi.exec(text)?.[1];
      const value = /^\s*ProxyServer\s+REG_SZ\s+(.+)/mi.exec(text)?.[1]?.trim();
      if (Number(enabled) === 1 && value) {
        const protocols = Object.fromEntries(value.split(';').filter(part => part.includes('=')).map(part => part.split('=')));
        const proxy = protocols.https || protocols.http || (!value.includes('=') ? value : '');
        if (proxy) env.HTTPS_PROXY = env.HTTP_PROXY = /^https?:\/\//i.test(proxy) ? proxy : `http://${proxy}`;
      }
    } catch { /* An absent Windows proxy does not prevent a direct connection. */ }
  }
  env.NO_PROXY = [...new Set([env.NO_PROXY || env.no_proxy || '', 'localhost', '127.0.0.1', '::1']
    .flatMap(value => value.split(',')).filter(Boolean))].join(',');
  env.no_proxy = env.NO_PROXY;
  return env;
}
