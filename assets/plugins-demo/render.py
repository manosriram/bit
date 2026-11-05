#!/usr/bin/env python3
"""Validate every scene and write a VHS recording script; run from repo root."""
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCENES = [
    ('intro', '01 / A tiny plugin system', 13),
    ('guards', '02 / Every before-hook return value', 19),
    ('ordering', '03 / Ordered guards: first block wins', 12),
    ('after', '04 / Replace, append, print, recover', 15),
    ('tools', '05 / Custom tools and generated schemas', 20),
    ('cross-file', '06 / Guards on another plugin tool', 12),
    ('ask', '07 / Interactive and headless questions', 18),
    ('policy', '08 / Approval policy and headless execution', 19),
    ('api', '09 / Shell, argv, exit codes and file reads', 18),
    ('state', '10 / Persistent state and restart', 12),
    ('validation', '11 / Startup validation and helpful errors', 22),
    ('failures', '12 / Errors, invalid returns and resource limits', 24),
    ('cli', '13 / Discovery, diagnostics and recovery', 24),
    ('limits', '14 / Recovery and implementation boundaries', 23),
]
def main():
    subprocess.run(['cargo', 'build', '-q', '-p', 'agent', '--example', 'plugins_demo'], cwd=ROOT, check=True)
    subprocess.run(['cargo', 'build', '-q', '-p', 'bit'], cwd=ROOT, check=True)
    tape = ['Output assets/plugins-demo/bit-plugins.mp4',
            'Set Shell "bash"', 'Set FontSize 20', 'Set Width 1600', 'Set Height 1100',
            'Set Padding 30', 'Set Theme "Catppuccin Mocha"', 'Set TypingSpeed 1ms',
            'Set Framerate 15', 'Hide', 'Type "export PS1=\'bit-demo $ \'"', 'Enter', 'Sleep 500ms']
    transcript = []
    chapters = []
    elapsed = 0
    for scene, title, seconds in SCENES:
        args = ['python3', str(ROOT / 'assets/plugins-demo/cli.py')] if scene == 'cli' else [str(ROOT / 'target/debug/examples/plugins_demo'), scene]
        result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True, check=True, timeout=20)
        transcript.extend([title, '=' * len(title), result.stdout])
        chapters.append(f'{elapsed // 60:02}:{elapsed % 60:02} {title}')
        elapsed += seconds
        executable = "python3 assets/plugins-demo/cli.py" if scene == "cli" else f"target/debug/examples/plugins_demo {scene}"
        command = f"clear; printf '\\033[1;36m{title}\\033[0m\\n\\n'; {executable}"
        # VHS double-quoted strings use escaped embedded double quotes.
        tape += ['Hide', 'Type "' + command.replace('"', '\\"') + '"', 'Enter', 'Sleep 500ms', 'Show', f'Sleep {seconds}s']
    tape += ['Screenshot assets/plugins-demo/poster.png']
    (ROOT / 'assets/plugins-demo/demo.tape').write_text('\n'.join(tape) + '\n')
    (ROOT / 'assets/plugins-demo/transcript.txt').write_text('\n'.join(transcript))
    (ROOT / 'assets/plugins-demo/chapters.txt').write_text('\n'.join(chapters) + '\n')
    print(f'Validated {len(SCENES)} scenes; tape written.')


if __name__ == "__main__":
    main()
