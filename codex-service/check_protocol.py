"""Isolated end-to-end check; the real Codex CLI talks only to a local fake model."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import json
import os
import socket
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.request

SCRIPT = Path(__file__).with_name('server.mjs').resolve()
EXE = os.environ.get('CODEX_EXECUTABLE') or shutil.which('codex')
if not EXE:
    raise SystemExit('Install Codex CLI or set CODEX_EXECUTABLE to its executable path')
captured = []
first_received = threading.Event()
release_first = threading.Event()


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        captured.append(request)
        number = len(captured)
        if number == 1:
            first_received.set()
            assert release_first.wait(20)
            output = [
                {'type': 'message', 'id': 'msg_commentary', 'role': 'assistant', 'phase': 'commentary',
                 'status': 'completed', 'content': [{'type': 'output_text', 'text': 'PUBLIC_PROGRESS', 'annotations': []}]},
                {'type': 'function_call', 'id': 'fc_shell', 'call_id': 'call_shell', 'name': 'exec_command',
                 'arguments': json.dumps({'cmd': 'Write-Output "LOCAL_TOOL_CHECK"', 'shell': 'powershell',
                                          'login': False, 'max_output_tokens': 1000}), 'status': 'completed'},
            ]
        else:
            output = [{'type': 'message', 'id': f'msg_answer_{number}', 'role': 'assistant', 'phase': 'final_answer',
                       'status': 'completed', 'content': [{'type': 'output_text', 'text': f'PUBLIC_FINAL_{number}', 'annotations': []}]}]
        response = {'id': f'resp_fixture_{number}', 'object': 'response', 'model': 'gpt-6.1-sol'}
        events = [('response.created', {'response': {**response, 'status': 'in_progress', 'output': []}})]
        for index, item in enumerate(output):
            events.append(('response.output_item.added', {'output_index': index, 'item': {**item, 'status': 'in_progress'}}))
            events.append(('response.output_item.done', {'output_index': index, 'item': item}))
        events.append(('response.completed', {'response': {**response, 'status': 'completed', 'output': output,
                     'usage': {'input_tokens': 100, 'output_tokens': 5, 'total_tokens': 105,
                               'input_tokens_details': {'cached_tokens': 0}, 'output_tokens_details': {'reasoning_tokens': 0}}}}))
        wire = ''.join('event: ' + kind + '\ndata: ' + json.dumps({'type': kind, **data}) + '\n\n' for kind, data in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(wire)))
        self.end_headers()
        self.wfile.write(wire)


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


model = ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=model.serve_forever, daemon=True).start()
child = None
try:
    with tempfile.TemporaryDirectory(prefix='alivebot-codex-integration-', ignore_cleanup_errors=True) as temporary:
        root = Path(temporary)
        home, workspace = root / 'home', root / 'workspace'
        directory = workspace / '12345'
        home.mkdir(); directory.mkdir(parents=True)
        (home / 'config.toml').write_text(f'''
model = "gpt-6.1-sol"
model_provider = "local-check"
model_context_window = 262144
model_auto_compact_token_limit = 245760
model_reasoning_effort = "high"
approval_policy = "never"
sandbox_mode = "danger-full-access"
notify = []
[model_providers.local-check]
name = "Local integration fixture"
base_url = "http://127.0.0.1:{model.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
''', encoding='utf-8')
        port = free_port()
        config = root / 'service.json'
        config.write_text(json.dumps({'port': port, 'executable': EXE, 'home': str(home), 'workspace': str(workspace)}))
        diagnostics = []
        token = None
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def api(route, payload=None):
            req = urllib.request.Request(f'http://127.0.0.1:{port}' + route,
                    data=None if payload is None else json.dumps(payload).encode(),
                    headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
            try:
                with opener.open(req, timeout=35) as response:
                    return json.load(response)
            except urllib.error.HTTPError as error:
                raise RuntimeError(error.read().decode()) from None

        def start():
            global child, token
            child = subprocess.Popen(['node', str(SCRIPT), str(config)], stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding='utf-8',
                    creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            threading.Thread(target=lambda: diagnostics.extend(child.stdout), daemon=True).start()
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                try:
                    token = (home / 'service-token.txt').read_text().strip()
                    if api('/health')['ready']:
                        return
                except (OSError, RuntimeError):
                    if child.poll() is not None:
                        raise RuntimeError('Service exited: ' + ''.join(diagnostics)[-2000:])
                    time.sleep(.1)
            raise RuntimeError('Service did not start: ' + ''.join(diagnostics)[-2000:])

        def stop():
            global child
            if child is not None and child.poll() is None:
                child.stdin.write('{"action":"stop-codex"}\n'); child.stdin.flush()
                child.wait(timeout=10)
            child = None

        session = 'ses_' + '1' * 32
        settings = {'id': session, 'directory': str(directory), 'mayCreate': True,
                    'model': 'gpt-6.1-sol', 'effort': 'high', 'instructions': 'BASE_PROMPT_ONE'}
        prefix = f'/sessions/{session}'
        start()
        thread = api('/sessions/ensure', settings)['threadId']
        api(prefix + '/prompt', {'id': 'msg_record', 'prompt': {'text': 'BACKGROUND_MARKER <img:https://example.invalid/image.png>'}, 'resume': False})
        assert not captured and not api(prefix + '/active')['active'], 'Record-only input invoked model'
        api(prefix + '/prompt', {'id': 'msg_start', 'prompt': {'text': 'START_MARKER'}, 'resume': True, 'fastMode': True})
        assert first_received.wait(10)
        api(prefix + '/prompt', {'id': 'msg_own', 'prompt': {'text': 'OWN_NON_MODEL_STEER_MARKER'}, 'resume': False})
        release_first.set()
        deadline = time.monotonic() + 20
        while api(prefix + '/active')['active'] and time.monotonic() < deadline:
            time.sleep(.1)
        assert not api(prefix + '/active')['active']
        assert len(captured) == 2, len(captured)
        assert all(request.get('service_tier') == 'priority' for request in captured), f"Fast tier did not reach the provider: {[request.get('service_tier') for request in captured]}"
        second = json.dumps(captured[1])
        for marker in ['BACKGROUND_MARKER', 'OWN_NON_MODEL_STEER_MARKER', 'LOCAL_TOOL_CHECK', 'BASE_PROMPT_ONE']:
            assert marker in second, marker + ' missing from actual model request'
        assert not any(item.get('type') == 'input_image' for item in captured[0].get('input', [])), 'Image URL became attachment'
        history = api(prefix + '/history?after=0')['data']
        public = [api(prefix + '/message/' + event['data']['assistantMessageID'])['data']
                  for event in history if event['type'] == 'assistant.completed']
        assert [message['content'][0]['text'] for message in public] == ['PUBLIC_PROGRESS', 'PUBLIC_FINAL_2'], public
        print('PASS: idle injection, URL-only input, active own-message steer, real shell tool, commentary and final output.', flush=True)
        api('/sessions/ensure', {**settings, 'instructions': 'BASE_PROMPT_TWO', 'mayCreate': False})
        api(prefix + '/prompt', {'id': 'msg_new_prompt', 'prompt': {'text': 'PROMPT_REFRESH_MARKER'}, 'resume': True})
        deadline = time.monotonic() + 15
        while api(prefix + '/active')['active'] and time.monotonic() < deadline:
            time.sleep(.1)
        assert 'BASE_PROMPT_TWO' in json.dumps(captured[-1]), 'Native instruction refresh did not reach model'
        assert captured[-1].get('service_tier') != 'priority', 'Standard turn inherited Fast from the previous turn'
        print('PASS: per-turn Fast reaches the provider; steer preserves the running tier; next Standard turn disables Fast.', flush=True)
        assert 'BASE_PROMPT_ONE' not in json.dumps(captured[-1].get('instructions')), 'Old base instructions were retained'
        print('PASS: changed main instructions take effect in the same native thread without a service restart.', flush=True)
        stop(); start()
        resumed = api('/sessions/ensure', {**settings, 'mayCreate': False, 'instructions': 'BASE_PROMPT_TWO'})['threadId']
        assert resumed == thread
        api(prefix + '/prompt', {'id': 'msg_restart', 'prompt': {'text': 'RESTART_MARKER'}, 'resume': True})
        deadline = time.monotonic() + 15
        while api(prefix + '/active')['active'] and time.monotonic() < deadline:
            time.sleep(.1)
        assert 'BACKGROUND_MARKER' in json.dumps(captured[-1]) and 'OWN_NON_MODEL_STEER_MARKER' in json.dumps(captured[-1])
        assert 'BASE_PROMPT_TWO' in json.dumps(captured[-1])
        trace = '\n'.join(file.read_text(encoding='utf-8') for file in (home / 'alivebot').glob('trace-*.jsonl'))
        tool_records = [json.loads(line) for line in trace.splitlines() if json.loads(line)['type'] == 'tool']
        assert tool_records and all('command' not in record and 'output' not in record for record in tool_records)
        print('PASS: thread and context survive full service restart; tool trace contains names/status only.', flush=True)
        stop()
finally:
    release_first.set()
    if child is not None and child.poll() is None:
        child.stdin.write('{"action":"stop-codex"}\n'); child.stdin.flush()
        child.wait(timeout=10)
    model.shutdown(); model.server_close()
