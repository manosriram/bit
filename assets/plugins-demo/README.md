# bit plugin system demo

`bit-plugins.mp4` is a captioned terminal walkthrough of real plugin executions.
The source snippets, inputs, outputs and assertions run against bit's current Rust
plugin implementation. No LLM responses are fabricated, no provider is contacted,
and the CLI scenes use a temporary configuration directory. Interactive answers
are explicitly supplied by the demo harness rather than a person in the TUI.

Watch with `bit-plugins.mp4`; use `chapters.txt` to navigate and `transcript.txt` for
a searchable, accessible text version. `poster.png` is a preview frame.

## Coverage

| Chapter | Cases exercised |
|---|---|
| 1. Overview | File location, startup loading, three hook prefixes |
| 2. Before hooks | String blocks, true allows, table rewrites, false/nil pass |
| 3. Ordering | Alphabetical order, block beats allow, first block stops later hooks |
| 4. After hooks | Replace output, append text, human-only print, error preserves result |
| 5. Custom tools | Description/schema, required string, optional integer, injected fields, JSON table result, nil result, Lua error, missing tool |
| 6. Cross-file hooks | Guard can precede its custom tool's file; pass and block |
| 7. Questions | Headless empty answer, interactive yes/no request-reply, pass/block |
| 8. Policy | Read-only tool, bash/custom-tool approval, auto mode, allowlist, shell metacharacters, always approve |
| 9. API | Shell string, direct argv, nonzero exit code, relative read, missing file |
| 10. State | Repeated calls retain state; fresh plugin load resets state |
| 11. Validation | Unknown field/tool, syntax error, name collision, missing description, invalid type, unmatched annotation |
| 12. Failures | Guard error, unsupported return, invalid rewrite, infinite-loop deadline, memory exhaustion, subprocess timeout |
| 13. CLI | Empty directory, list hooks, broken file failure, explicit disable/recovery, guide |
| 14. Limits | Top-level loop, unavailable io/require, empty plugin set; documented limits |

The video explains the 60-second after/tool budget and paused question budget;
it does not spend a full minute waiting on a timeout. It describes approval-flow
integration; the interactive questions in chapter 7 exercise the plugin channel,
not the full-screen TUI. Existing agent integration tests separately verify the
approval UI events, custom-tool request prefix and tool execution:

```sh
cargo test -p agent plugin
```

There are infinitely many possible Lua programs and malformed inputs; this demo
covers the public feature paths and the principal implemented failure paths,
not an exhaustive correctness or security audit.

## Reproduce

Requirements: Rust toolchain, Python 3, VHS (with its browser/ttyd dependencies),
and FFmpeg. From the repository root:

```sh
python3 assets/plugins-demo/render.py
vhs assets/plugins-demo/demo.tape
python3 assets/plugins-demo/package.py
```

`render.py` builds the example and bit, validates every scene, and generates the
tape, chapter list and transcript. VHS records the terminal to MP4. `package.py`
adds chapter metadata and subtitle tracks and extracts the preview.

Plugin demonstration source: `crates/agent/examples/plugins_demo.rs`.
CLI demonstration source: `assets/plugins-demo/cli.py`.

The demonstration creates temporary files and runs only local printf/sleep
commands. It does not touch the user's installed plugins or credentials.
