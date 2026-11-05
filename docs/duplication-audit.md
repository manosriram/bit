# Duplication audit

Branch: `refactor/equivalent-duplicates`. Baseline is the existing worktree,
including plugin runtime hardening, demo artifacts and the interrupted unused-code
audit. Those earlier changes are preserved and excluded from this audit's line
counts. No push or merge is part of this work.

Reviewed all five workspace crates, their tests, the Cargo example, Python demo
drivers, shell scripts, Lua guide, manifests and workflows. Exact multiline-block
searches and normalized function-body similarity searches were followed by manual
comparison of inputs, outputs, side effects, errors and call sites. Similarity
scores locate candidates; they are not evidence of behavioral equivalence.

## Groups selected for consolidation

### 1. API-key provider defaults

Keep the existing default behavior in a private `keyed(name, base_url, env)`
constructor in `crates/agent/src/providers/keyed.rs`. All six provider definitions
use it: DEEPSEEK, OPENAI, OPENCODE_GO, ANTHROPIC, OPENROUTER and ZAI. Names, URLs
and environment-variable names remain explicit data. The shared behavior is
Chat Completions, API-key authentication, no cache-routing key, no extra headers,
and `reasoning_effort` translation.

Provider-specific behaviors are deliberately **not** merged: OPENAI still enables
cache routing, OPENCODE_GO still adds its session header, ANTHROPIC still uses the
Messages wire format and `output_config`, and ZAI still translates thinking to
enabled/disabled. Struct updates make those overrides explicit at their original
definitions. ChatGPT's OAuth spec is unchanged.

Equivalence check: a temporary example snapshots every provider's name, base URL,
wire type, auth display/environment, cache setting, generated headers and thinking
fields for none/low/medium/high/xhigh/max before and after the change. The output
must compare byte-for-byte equal. The probe does not send requests or access
credentials and is removed after validation. Source inspection also verifies that
unchanged override callbacks retain their original bodies.

### 2. Process-group kill primitive

Keep the bash tool's guarded `kill(-pid, SIGKILL)` implementation as
`tools::kill_process_group`. The bash `Group` guard and the Lua process runner
both call it. This shares only the identical operating-system operation; it does
not share command execution or cleanup scheduling.

Bash already checks `id > 0`; the plugin caller obtains a positive child PID from
successful spawn, so adding the same guard there does not change behavior. Both
implementations send SIGKILL to the negative group ID and ignore the syscall's
return value. All existing calls retain their ordering, including bash's Drop
cleanup and the plugin runner's exit/error/timeout cleanup.

### 3. Thinking-level availability validation

Keep the identical validation from `Ui::ask_thinking` and `Ui::next_thinking` in
one private helper. Both entry points currently obtain `self.levels()`, emit
`Event::Notice(self.no_levels())` and return if empty. The helper returns the same
static slice when available and emits the same notice once otherwise.

Picker creation and Tab cycling remain separate because they have different
behavior. `Ui::levels`, `effort`, model rendering and explicit-level validation
remain pure or retain their original behavior; they do not gain notices.

### 4. Demo video path assignment

Keep the first `video = folder / 'bit-plugins.mp4'` in
`assets/plugins-demo/package.py` and remove the identical second assignment.
Neither `folder` nor `video` changes in between; all scan, probe, packaging,
replacement and preview call sites use the same initial Path.

Equivalence check: run both script versions against isolated copies of the existing
MP4 and renderer. Compare SHA-256 hashes of all five outputs: MP4, poster PNG,
subtitle SRT, chapter metadata and chapter list. Every output is byte-for-byte
identical. The user's original video and other assets are untouched.

## Similar implementations kept separate

| Candidates | Differences that prevent merging their behavior |
|---|---|
| Chat/Responses/Anthropic request adapters | Different wire schemas, message grouping, tool schema shapes, cache controls and thinking preservation. Only the generic LLM client is already shared. |
| OAuth token exchange vs LLM streaming request | Form encoding vs JSON, different authentication/headers, 15-second timeout vs streaming, different error text and refresh semantics. |
| Lua `run` vs built-in bash tool | Blocking/nonblocking vs async I/O; stdout-then-stderr vs merged streams; 1 MiB error vs spill files; different timeout ranges and cancellation lifetime. Only their kill primitive is identical. |
| `Turn::before` vs `Turn::after` | Before returns an approval verdict and mutable argument rewrites; after mutates output and reports errors as notices. Budgets and return contracts differ. |
| Hook callback return validation/conversion | Before blocks/approves/rewrites, after appends/replaces, custom tools return results. One generic return handler would change semantics. JSON conversion is already shared where appropriate. |
| `cap` vs streaming `Capped` | Batch string handling vs bounded streaming/spilling. Similar output is intentional; tests compare the independent implementations. |
| Streaming read vs `tests::whole` | Production reader uses bounded buffers; the test oracle reads whole strings. Sharing implementation would weaken the regression test. |
| TUI `clean`, input insertion, Lua print sanitization | Replace controls with spaces vs drop controls/preserve normalized newlines vs preserve tabs/newlines and limit line count. |
| TUI approval detail vs completed diff rendering | Box borders, indentation, clipping widths and truncation formatting differ. |
| TUI `Term::pin`, `wipe`, `clear` | Similar state resets, but different terminal escape sequences, region sizing and scrollback effects. |
| Temporary fixture loaders and setup | Plugin tests clear stale directories and retain fixtures; demo loader removes its fixtures after loading. Names and lifetimes provide test isolation. |
| Similar bash lifecycle tests | One checks normal parent exit, the other a timeout. They exercise distinct termination paths; independent tests retain those assertions. |
| Similar agent integration tests | Shared mock-server harness already exists; remaining sequences verify different protocol transitions (cancel, retry, headless, approval and login). |
| Provider sign-in/token dispatch | Sign-in opens a browser and obtains credentials; token retrieval may refresh existing credentials. Different callbacks and lifecycle. |
| Config runtime creation vs main runtime creation | Different error formatting and lifetimes; sign-in refresh runs before the session runtime. |
| Session/auth/config filesystem writing | Append-only logs vs owner-only credential replacement vs ordinary remembered settings. Permissions, parent handling and failure messages differ. |
| `floor`/`ceil`, clip/wrap, JSON/Lua conversion | Opposite boundary directions or different input/output structures; superficial loops are not equivalent functions. |
| Installer's platform/package-manager branches and CI matrix | Each invokes a different platform executable, package manager or build target. These are data/platform choices, not duplicate behavior to erase. |
| Lua guide's repeated hook patterns | Distinct examples for rm confirmation, secrets, generated files, formatting and deployments. They are both documentation and executable examples, not interchangeable implementations. |

Tiny repeated expressions (JSON argument parsing, formatting, UI line construction)
that serve different surrounding contracts remain local. The audit does not
introduce a generic callback framework or combine independent test oracles to
remove superficial repetition.

## Validation and line counts

Each group's final `cargo test --workspace` run passed all 83 tests (40 agent,
3 CLI, 14 LLM, 12 tools, 14 TUI), with no failures; all doc-tests passed too.

| Group | Net non-test lines saved | Extra validation |
|---|---:|---|
| Provider defaults | 23 | Before/after snapshots identical for all seven provider specs; six API-key specs refactored |
| Process-group signal | 1 | Existing bash and plugin normal-exit, descendant-kill and timeout tests passed |
| Thinking availability | -4 | Extended existing UI test with unknown/unset model cases; it passed before and after extraction |
| Demo video path | 1 | Real packaging runs generated five byte-identical outputs |
| **Total non-test** | **21** | **Full tests passed after every group** |

The thinking-level test adds 15 lines. Including tests, the six modified source
files contain **61 added lines and 67 deleted lines: 6 fewer lines overall**.
This report and pre-existing work are excluded from those figures. The small net
reduction reflects factoring shared behavior rather than deleting test coverage
or combining implementations whose contracts differ.

Additional final checks:

```text
cargo fmt --all --check                              PASS
cargo clippy --workspace --all-targets -- -D warnings PASS
cargo build --workspace --all-targets                PASS
cargo build --release --workspace                    PASS
git diff --check                                    PASS
```

One intermediate test run correctly rejected the temporary provider probe's
`"anthropic"` wire-format label outside the providers directory. The probe was
removed after its before/after comparison; the unmodified location-rule test and
the entire suite then passed. No test was weakened or removed.

Baseline comparison confirms only the six intended source files changed during
this audit; earlier tracked/untracked source changes and demo artifacts are
preserved. The temporary probe is absent. The branch remains
`refactor/equivalent-duplicates`, with no push or merge.
