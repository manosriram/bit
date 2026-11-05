#!/usr/bin/env python3
"""Add chapter metadata and optional subtitles to the VHS video."""
from pathlib import Path
import subprocess
import json
import re
from render import SCENES

folder = Path(__file__).resolve().parent
captions = [
    'A Lua file adds guards, output hooks and custom tools.\nThese are real local executions; no model is involved.',
    'Return a string to block, true to allow, a table to rewrite.\nFalse or nil leaves approval to bit.',
    'Files run alphabetically. A block overrides an earlier allow.\nThe first block stops later hooks.',
    'After hooks replace or append model-visible output.\nPrint sends a human-only notice; hook errors preserve output.',
    'Documentation generates tool schemas. Common fields are injected.\nTables become JSON; nil becomes empty output; errors fail the tool.',
    'A guard can target a tool declared in another plugin file.\nValidation happens after the tools have been discovered.',
    'Headless ask returns empty text. Interactive ask uses request/reply.\nThe demo harness supplies the yes and no responses shown.',
    'Custom tools need approval like bash. Auto mode allows them.\nGuards run first; rewritten arguments are approved and executed.',
    'Run accepts a shell string or a direct argv table.\nRead returns text, or nil and an error if the file is missing.',
    'Each file keeps its Lua state for the session.\nLoading a new plugin instance resets its counters.',
    'Invalid hooks, syntax, colliding tool names and bad annotations\nfail startup with a diagnostic.',
    'Guard errors, bad rewrites, invalid returns, instruction deadlines,\nmemory exhaustion and subprocess timeouts block the call.',
    'The CLI lists plugins, diagnoses errors and prints a working guide.\nUse --no-plugins to recover from broken plugins.',
    'Before hooks have 2 seconds; after hooks and tools have 60 seconds.\nLua memory is bounded; run is not an operating-system sandbox.',
]
def timestamp(ms):
    return f'{ms // 3600000:02}:{ms // 60000 % 60:02}:{ms // 1000 % 60:02},{ms % 1000:03}'
video = folder / 'bit-plugins.mp4'
# Align navigation to actual title changes, since capture can drop frames.
scan = subprocess.run(['ffmpeg', '-hide_banner', '-i', str(video), '-vf',
    "crop=1100:30:30:30,select='gt(scene,0.005)',showinfo", '-an', '-f', 'null', '-'],
    text=True, capture_output=True, check=True)
starts = [0] + [round(float(t) * 1000) for t in re.findall(r'pts_time:([\d.]+)', scan.stderr)]
assert len(starts) == len(SCENES), f'Expected {len(SCENES)} title cards, got {len(starts)}'
probe = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'v:0',
    '-show_entries', 'stream=duration', '-of', 'json', str(video)],
    text=True, capture_output=True, check=True)
duration = round(float(json.loads(probe.stdout)['streams'][0]['duration']) * 1000)
ends = starts[1:] + [duration]
meta = [';FFMETADATA1']
subs = []
chapters = []
for i, ((_, title, _), caption, start, end) in enumerate(zip(SCENES, captions, starts, ends), 1):
    meta += ['[CHAPTER]', 'TIMEBASE=1/1000', f'START={start}', f'END={end}', f'title={title}']
    subs.append(f'{i}\n{timestamp(start)} --> {timestamp(end - 100)}\n{caption}\n')
    chapters.append(f'{start // 60000:02}:{start // 1000 % 60:02} {title}')
(folder / 'chapters.ffmeta').write_text('\n'.join(meta) + '\n')
(folder / 'chapters.txt').write_text('\n'.join(chapters) + '\n')
(folder / 'captions.srt').write_text('\n'.join(subs))
packaged = folder / 'bit-plugins-captioned.mp4'
subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', str(video),
                '-i', str(folder / 'chapters.ffmeta'), '-i', str(folder / 'captions.srt'),
                '-map', '0:v', '-map', '2:0', '-map_metadata', '1', '-map_chapters', '1',
                '-c:v', 'copy', '-c:s', 'mov_text', '-metadata:s:s:0', 'language=eng',
                '-metadata', 'title=bit: Lua plugins, hooks and edge cases',
                '-movflags', '+faststart', str(packaged)], check=True)
packaged.replace(video)
subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-ss', '5',
                '-i', str(video), '-frames:v', '1', str(folder / 'poster.png')], check=True)
print(f'Packaged {video} with {len(SCENES)} chapters and English subtitles.')
