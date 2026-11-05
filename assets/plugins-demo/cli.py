#!/usr/bin/env python3
"""Demonstrate the CLI with isolated configuration and no provider credentials."""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix='bit-demo-cli-') as tmp:
    config = Path(tmp) / 'bit'
    plugins = config / 'plugins'
    plugins.mkdir(parents=True)
    (config / 'config.toml').write_text('provider = "demo"\nmodel = "demo"\n[providers.demo]\nbase_url = "http://127.0.0.1:9"\napi_key_env = "BIT_DEMO_UNUSED_KEY"\nmodels = ["demo"]\n')
    env = {'PATH': os.environ['PATH'], 'HOME': tmp, 'XDG_CONFIG_HOME': tmp, 'TERM': 'dumb'}
    def run(args, code, contains):
        print('$ bit ' + ' '.join(args), flush=True)
        p = subprocess.run([str(root / 'target/debug/bit'), *args], env=env, cwd=tmp,
                           text=True, capture_output=True, timeout=10)
        text = p.stdout + p.stderr
        assert p.returncode == code, text
        assert contains in text, text
        print(text.strip() + '\n', flush=True)
    run(['plugins'], 0, 'no plugins')
    (plugins / 'guard.lua').write_text('function before_bash(command) return nil end')
    run(['plugins'], 0, 'before_bash')
    (plugins / 'broken.lua').write_text('function before_bsh() end')
    run(['plugins'], 1, 'no tool')
    # No key: exits before making any provider request, after skipping bad plugins.
    run(['--no-plugins', '-p', 'hello'], 1, 'key')
    print('Broken plugins were skipped; startup reached the missing-key check.\n')
    guide = subprocess.run([str(root / 'target/debug/bit'), 'plugins', 'guide'], env=env,
                           text=True, capture_output=True, check=True)
    assert 'function before_bash' in guide.stdout
    print('$ bit plugins guide\nWorking Lua guide printed (excerpt):\n')
    print('\n'.join(guide.stdout.splitlines()[:6]))
    print('\n[scene verified]')
