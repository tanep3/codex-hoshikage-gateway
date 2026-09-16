#!/usr/bin/env python3
"""Opt-in real Codex/MCP acceptance; no writes to production config or databases.

Creates a separate Proxy on loopback with a temporary credential copy. Only
Playwright navigation and page-text search are exposed to the test model.
Discord HTTP is simulated by the Rust test, not a real user's button click.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import tomllib
import urllib.request


def main():
    home = Path.home()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--proxy-home', type=Path, default=home / '.config/codex-hoshikage-proxy')
    parser.add_argument('--gateway-config', type=Path, default=home / '.config/codex-hoshikage-gateway/config.toml')
    parser.add_argument('--codex-home', type=Path, default=home / '.codex')
    parser.add_argument('--proxy-bin', type=Path, default=Path(shutil.which('codex-hoshikage-proxy') or home / '.cargo/bin/codex-hoshikage-proxy'))
    parser.add_argument('--case', choices=['once-turn', 'revoke'], default='once-turn')
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    original = tomllib.loads((args.proxy_home / 'config.toml').read_text())
    mcp = tomllib.loads((args.codex_home / 'config.toml').read_text())['mcp_servers']['playwright']
    allowed = {'url', 'tools', 'enabled', 'startup_timeout_sec', 'tool_timeout_sec'}
    if set(mcp) - allowed or mcp.get('enabled') is False:
        raise RuntimeError('Unsupported or disabled MCP connection; do not discard required settings')
    with tempfile.TemporaryDirectory(prefix='gateway-mcp-integration-') as directory:
        root = Path(directory)
        root.chmod(0o700)
        proxy_home, source, work = [root / name for name in ('proxy', 'source', 'work')]
        for path in (proxy_home, source, work, proxy_home / 'codex-home'):
            path.mkdir()
        auth = proxy_home / 'codex-home/auth.json'
        shutil.copyfile(args.proxy_home / 'codex-home/auth.json', auth)
        auth.chmod(0o600)
        (source / 'config.toml').write_text(
            '[mcp_servers.playwright]\nurl = ' + json.dumps(mcp['url']) + '\n'
            'enabled_tools = ["browser_navigate", "browser_find"]\n'
            '[mcp_servers.playwright.tools.browser_navigate]\napproval_mode = "approve"\n'
            '[mcp_servers.playwright.tools.browser_find]\napproval_mode = "prompt"\n')
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        # Build the isolated settings explicitly. Never rewrite production text
        # with substitutions that could accidentally retain its listen address.
        (proxy_home / 'config.toml').write_text(f'''
[server]
host = "127.0.0.1"
port = {port}
v2_enabled = true
default_cwd = {json.dumps(str(work))}
[codex]
command = {json.dumps(original['codex']['command'])}
args = ["app-server", "--listen", "stdio://"]
user_home = {json.dumps(str(source))}
[codex.sandbox]
mode = "workspace-write"
network_access = false
[security]
api_key_env = "PROXY_API_KEY"
allowed_cwds = [{json.dumps(str(work))}]
[approval]
auto_approve_workspace = false
[defaults]
model = "chatgpt/gpt-5.6-luna"
[providers.chatgpt]
codex_id = "openai"
enabled = true
[providers.hoshikage]
codex_id = "hoshikage"
enabled = false
[v2]
mcp_turn_approval_enabled = true
mcp_turn_grant_tools = {{ playwright = ["browser_find"] }}
''')
        key = secrets.token_urlsafe(32)
        keyfile = root / 'key'
        keyfile.write_text(key)
        keyfile.chmod(0o600)
        text = args.gateway_config.read_text()
        config = tomllib.loads(text)
        base = f'http://127.0.0.1:{port}'
        modified = text.replace(config['proxy']['base_url'], base).replace(config['proxy']['api_key_file'], str(keyfile))
        parsed = tomllib.loads(modified)
        if parsed['proxy']['base_url'] != base or parsed['proxy']['api_key_file'] != str(keyfile):
            raise RuntimeError('Cannot create isolated Gateway configuration')
        gateway_config = root / 'gateway.toml'
        gateway_config.write_text(modified)
        gateway_config.chmod(0o600)
        environment = os.environ.copy()
        environment.update(CODEX_HOSHIKAGE_PROXY_HOME=str(proxy_home), PROXY_API_KEY=key)
        with (root / 'proxy.log').open('w+') as log:
            proxy = subprocess.Popen([str(args.proxy_bin)], env=environment,
                                     stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                for _ in range(60):
                    if proxy.poll() is not None:
                        log.flush()
                        log.seek(0)
                        diagnostic = log.read()[-4096:].replace(key, '[REDACTED]')
                        raise RuntimeError('Isolated Proxy exited before readiness: ' + diagnostic)
                    try:
                        request = urllib.request.Request(base + '/v2/codex/capabilities', headers={'Authorization': 'Bearer ' + key})
                        with urllib.request.urlopen(request, timeout=1) as response:
                            capability = json.load(response)
                        if capability.get('mcp_turn_approval', {}).get('enabled'):
                            break
                    except (OSError, ValueError):
                        pass
                    time.sleep(1)
                else:
                    raise RuntimeError('Isolated Proxy readiness deadline exceeded')
                print('Isolated Proxy ready; production settings unchanged.', flush=True)
                test_environment = os.environ.copy()
                test_environment['HOSHIKAGE_LIVE_CONFIG'] = str(gateway_config)
                test_environment['HOSHIKAGE_MCP_CASE'] = args.case
                return subprocess.call(['cargo', 'test', '--test', 'live_mcp_gateway', '--', '--ignored', '--nocapture'], cwd=repository, env=test_environment)
            finally:
                try:
                    os.killpg(proxy.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    proxy.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(proxy.pid, signal.SIGKILL)
                    proxy.wait()


if __name__ == '__main__':
    raise SystemExit(main())
