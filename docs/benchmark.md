# Memory benchmark

10 interactive bit sessions running at once, each driven through one long turn of 150 tool calls. Produced by `scripts/bench.py`; every number below is measured.

- **Binary:** 3.21 MB (`target/release/bit`, built from `d579f89` plus uncommitted changes, if any)
- **Machine:** Apple A18 Pro, Darwin 27.0.0 arm64
- **Date:** 2026-11-11

## Results

Resident memory (RSS) per bit process, across the 10 sessions.

| Scenario | Idle | Peak during the turn (avg) | Peak (worst session) | All 10 sessions at peak | After the turn |
|---|---:|---:|---:|---:|---:|
| No skills, 128k window | 7.0 MB | 12.1 MB | 12.4 MB | 121.1 MB | 12.1 MB |
| 50 skills, 128k window | 7.2 MB | 12.4 MB | 12.6 MB | 124.4 MB | 12.4 MB |
| 50 skills, 1M window | 7.2 MB | 25.0 MB | 26.9 MB | 249.8 MB | 25.0 MB |

What each session did:

| Scenario | Tool calls | Requests | Compactions | Largest request | Wall time |
|---|---:|---:|---:|---:|---:|
| No skills, 128k window | 150 | 161 | 10 | 0.45 MB | 7 s |
| 50 skills, 128k window | 150 | 161 | 10 | 0.46 MB | 7 s |
| 50 skills, 1M window | 150 | 152 | 1 | 3.04 MB | 7 s |

## Method

- Each session is a real `bit` TUI in its own pty (120x40), in ask-free `auto` mode, started together and given its prompt at the same moment, so all 10 work concurrently.
- The provider is a local mock that speaks Chat Completions over SSE: streamed markdown text, then one tool call per request. No network and no API key are used, so the run is free and repeatable.
- The 150 tool calls cycle through: `bash` printing 300,000 lines (about 2 MB, which bit spills to a file and caps), `read` of a 12 MB file, `grep` across bit's own source, `write` of a 200-line file, and `edit` of it. Every call must succeed, or the run fails.
- The mock reports input tokens as request bytes / 4, so bit compacts the conversation when its context window fills, as it would with a real model. The 128k rows use a model bit doesn't know (128k default window); the 1M row uses a model id with a 1,048,576-token window, so far more history is held in memory.
- Skill rows have 50 skills installed (all listed in the system prompt), and the prompt invokes one with `/skill-07`, which loads its SKILL.md into the turn.
- **Idle** is RSS 5 s after start. **Peak** is the highest RSS of the bit process itself, sampled about every 60 ms with `ps`; a spike shorter than that can be missed. As a cross-check, the kernel's own high-water mark for bit *or any child it ran* (`sh`, `seq`, `rg`) never exceeded 26.9 MB in any session. **After** is RSS 3 s after the turn ends.

## Reproduce

```
cargo build --release
python3 scripts/bench.py
```

`BENCH_INSTANCES` and `BENCH_STEPS` change the session count and the tool calls per session. Needs Python 3 and ripgrep; runs on macOS and Linux.
