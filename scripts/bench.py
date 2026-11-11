#!/usr/bin/env python3
"""Memory benchmark: N interactive bit sessions at once, each driven through a long turn of
tool calls against a local mock provider. No network, no API key, no cost.

    cargo build --release && python3 scripts/bench.py

Writes docs/benchmark.md. Each bit runs in its own pty, exactly as a user would run it.
"""
import fcntl, json, os, platform, pty, select, shutil, struct, subprocess, sys, tempfile
import termios, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIT = os.path.join(ROOT, "target/release/bit")
N = int(os.environ.get("BENCH_INSTANCES", 10))
STEPS = int(os.environ.get("BENCH_STEPS", 150))
SKILLS = 50
SUMMARIZE = b"Summarize this conversation so that you can continue"

# Per API key (one per instance): tool steps served, requests, compactions, done.
state, lock = {}, threading.Lock()


def call(k):
    """The k-th tool call: a cycle of heavy bash output, a big read, a grep, a write, an edit."""
    kind = k % 5
    if kind == 0:
        return "bash", {"command": "seq 1 300000"}  # ~2 MB of output, spilled and capped
    if kind == 1:
        return "read", {"path": "big.txt", "offset": 1 + 2000 * (k // 5)}
    if kind == 2:
        return "grep", {"pattern": "fn ", "path": "crates"}
    lines = "".join(f"note {i} of step {k}\n" for i in range(200))
    if kind == 3:
        return "write", {"path": "notes.md", "content": f"MARK-{k}\n{lines}"}
    return "edit", {"path": "notes.md", "old": f"MARK-{k - 1}", "new": f"DONE-{k}"}


def sse(deltas, tokens):
    out = "".join(f"data: {json.dumps({'choices': [{'delta': d}]})}\n\n" for d in deltas)
    usage = {"prompt_tokens": tokens, "completion_tokens": 50,
             "prompt_tokens_details": {"cached_tokens": tokens * 9 // 10}}
    return out + f"data: {json.dumps({'choices': [], 'usage': usage})}\n\ndata: [DONE]\n\n"


class Mock(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers["content-length"]))
        key = self.headers["authorization"].split()[-1]
        with lock:
            s = state.setdefault(key, {"step": 0, "requests": 0, "compactions": 0,
                                       "done": False, "max_body": 0, "errors": 0})
            s["requests"] += 1
            s["max_body"] = max(s["max_body"], len(body))
            # The last tool result is at the end of the body.
            s["errors"] += b'"content":"error:' in body[-400:]
            summarize = SUMMARIZE in body[-1200:]
            k = s["step"]
            if summarize:
                s["compactions"] += 1
            elif k < STEPS:
                s["step"] += 1
            else:
                s["done"] = True
        text = [{"content": f"Step {k}: **checking** `the code`, then moving on. "} for _ in range(8)]
        if summarize:
            deltas = [{"content": "The user wants the stress run finished; continue the steps. "}] * 40
        elif k < STEPS:
            name, args = call(k)
            fn = {"name": name, "arguments": json.dumps(args)}
            deltas = text + [{"content": "\n"}, {"tool_calls": [
                {"index": 0, "id": f"c{k}", "type": "function", "function": fn}]}]
        else:
            deltas = [{"content": f"- item {i}: `done` with **all** of it\n"} for i in range(300)]
        out = sse(deltas, len(body) // 4).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(out)


def rss(pids):
    """Resident memory in KB of each live pid."""
    out = subprocess.run(["ps", "-o", "pid=,rss=", "-p", ",".join(map(str, pids))],
                         capture_output=True, text=True).stdout
    return {int(p): int(r) for p, r in (l.split() for l in out.splitlines())}


def scenario(name, port, skills, model):
    base = tempfile.mkdtemp(prefix="bit-bench-")
    conf = os.path.join(base, "config", "bit")
    os.makedirs(conf)
    with open(os.path.join(conf, "config.toml"), "w") as f:
        f.write(f'provider = "local"\nmodel = "{model}"\n[providers.local]\n'
                f'base_url = "http://127.0.0.1:{port}/v1"\napi_key_env = "BENCH_KEY"\n'
                f'models = ["{model}"]\n')
    for i in range(skills):
        d = os.path.join(conf, "skills", f"skill-{i:02}")
        os.makedirs(d)
        with open(os.path.join(d, "SKILL.md"), "w") as f:
            f.write(f"---\nname: skill-{i:02}\ndescription: Benchmark skill {i}; use it for "
                    f"tasks of kind {i}, such as reviewing, testing or refactoring code.\n---\n"
                    + "Follow these steps carefully and report what you did.\n" * 80)
    prompt = "/skill-07 run the stress steps\r" if skills else "run the stress steps\r"
    procs = {}
    for i in range(N):
        work = os.path.join(base, f"work-{i}")
        shutil.copytree(os.path.join(ROOT, "crates"), os.path.join(work, "crates"))
        with open(os.path.join(work, "big.txt"), "w") as f:
            f.writelines(f"{n:07} " + "x" * 90 + "\n" for n in range(120_000))  # ~12 MB
        key = f"{name}-{i}"
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(work)
            env = dict(os.environ, XDG_CONFIG_HOME=os.path.join(base, "config"), BENCH_KEY=key)
            os.execve(BIT, ["bit", "--mode", "auto"], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        procs[pid] = {"fd": fd, "key": key, "peak": 0}

    def drain(secs):
        """Reads every pty so no bit blocks on output, sampling RSS as it goes."""
        end = time.time() + secs
        while time.time() < end:
            fds = [p["fd"] for p in procs.values()]
            for fd in select.select(fds, [], [], 0.05)[0]:
                try:
                    os.read(fd, 1 << 16)
                except OSError:
                    pass
            for pid, kb in rss(procs).items():
                procs[pid]["peak"] = max(procs[pid]["peak"], kb)

    drain(5)
    idle = rss(procs)
    assert len(idle) == N, "a bit instance exited at startup"
    start = time.time()
    for p in procs.values():
        os.write(p["fd"], prompt.encode())
    while not all(state.get(p["key"], {}).get("done") for p in procs.values()):
        drain(0.2)
        assert time.time() - start < 1800, f"timed out: {state}"
        assert len(rss(procs)) == N, "a bit instance died mid-run"
    secs = time.time() - start
    drain(3)
    after = rss(procs)
    rows = []
    for pid, p in procs.items():
        os.kill(pid, 9)
        _, _, ru = os.wait4(pid, 0)
        # Peak of bit or any child it ran (sh, seq, rg): an upper bound on bit's own peak.
        bound = ru.ru_maxrss // 1024 if sys.platform == "darwin" else ru.ru_maxrss
        s = state[p["key"]]
        assert s["step"] == STEPS and s["errors"] == 0, s
        rows.append({"idle": idle[pid], "peak": p["peak"], "after": after[pid], "bound": bound, **s})
    sessions = os.listdir(os.path.join(conf, "sessions"))
    assert len(sessions) == N, sessions
    shutil.rmtree(base)
    return {"name": name, "skills": skills, "model": model, "secs": secs, "rows": rows}


def mb(kb):
    return f"{kb / 1024:.1f}"


def report(results):
    cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                         text=True).stdout.strip() or platform.processor()
    size = os.path.getsize(BIT)
    head = subprocess.run(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip()
    out = [
        "# Memory benchmark", "",
        f"{N} interactive bit sessions running at once, each driven through one long turn of "
        f"{STEPS} tool calls. Produced by `scripts/bench.py`; every number below is measured.", "",
        f"- **Binary:** {size / 1e6:.2f} MB (`target/release/bit`, built from `{head}` plus "
        "uncommitted changes, if any)",
        f"- **Machine:** {cpu}, {platform.system()} {platform.release()} {platform.machine()}",
        f"- **Date:** {time.strftime('%Y-%m-%d')}", "",
        "## Results", "",
        f"Resident memory (RSS) per bit process, across the {N} sessions.", "",
        "| Scenario | Idle | Peak during the turn (avg) | Peak (worst session) "
        f"| All {N} sessions at peak | After the turn |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for r in results:
        rows = r["rows"]
        avg = lambda k: sum(x[k] for x in rows) / len(rows)
        out.append(f"| {r['label']} | {mb(avg('idle'))} MB | {mb(avg('peak'))} MB | "
                   f"{mb(max(x['peak'] for x in rows))} MB | "
                   f"{mb(sum(x['peak'] for x in rows))} MB | {mb(avg('after'))} MB |")
    out += ["", "What each session did:", "",
            "| Scenario | Tool calls | Requests | Compactions | Largest request | Wall time |",
            "|---|---:|---:|---:|---:|---:|"]
    for r in results:
        x = r["rows"][0]
        out.append(f"| {r['label']} | {x['step']} | {x['requests']} | {x['compactions']} | "
                   f"{max(y['max_body'] for y in r['rows']) / 1e6:.2f} MB | {r['secs']:.0f} s |")
    bound = max(x["bound"] for r in results for x in r["rows"])
    out += [
        "",
        "## Method", "",
        f"- Each session is a real `bit` TUI in its own pty (120x40), in ask-free `auto` mode, "
        f"started together and given its prompt at the same moment, so all {N} work "
        "concurrently.",
        "- The provider is a local mock that speaks Chat Completions over SSE: streamed "
        "markdown text, then one tool call per request. No network and no API key are used, "
        "so the run is free and repeatable.",
        f"- The {STEPS} tool calls cycle through: `bash` printing 300,000 lines (about 2 MB, "
        "which bit spills to a file and caps), `read` of a 12 MB file, `grep` across bit's own "
        "source, `write` of a 200-line file, and `edit` of it. Every call must succeed, or the "
        "run fails.",
        "- The mock reports input tokens as request bytes / 4, so bit compacts the "
        "conversation when its context window fills, as it would with a real model. The "
        "128k rows use a model bit doesn't know (128k default window); the 1M row uses a "
        "model id with a 1,048,576-token window, so far more history is held in memory.",
        f"- Skill rows have {SKILLS} skills installed (all listed in the system prompt), and "
        "the prompt invokes one with `/skill-07`, which loads its SKILL.md into the turn.",
        "- **Idle** is RSS 5 s after start. **Peak** is the highest RSS of the bit process "
        "itself, sampled about every 60 ms with `ps`; a spike shorter than that can be "
        "missed. As a cross-check, the kernel's own high-water mark for bit *or any child "
        f"it ran* (`sh`, `seq`, `rg`) never exceeded {mb(bound)} MB in any session. "
        "**After** is RSS 3 s after the turn ends.", "",
        "## Reproduce", "",
        "```", "cargo build --release", "python3 scripts/bench.py", "```", "",
        "`BENCH_INSTANCES` and `BENCH_STEPS` change the session count and the tool calls per "
        "session. Needs Python 3 and ripgrep; runs on macOS and Linux.", "",
    ]
    with open(os.path.join(ROOT, "docs/benchmark.md"), "w") as f:
        f.write("\n".join(out))
    print("\n".join(out))


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    port = server.server_address[1]
    plan = [
        ("plain", "No skills, 128k window", 0, "bench-model"),
        ("skills", f"{SKILLS} skills, 128k window", SKILLS, "bench-model"),
        ("big", f"{SKILLS} skills, 1M window", SKILLS, "kimi-k3"),
    ]
    results = []
    for name, label, skills, model in plan:
        r = scenario(name, port, skills, model)
        r["label"] = label
        results.append(r)
        print(f"{label}: done in {r['secs']:.0f}s", file=sys.stderr)
    report(results)


if __name__ == "__main__":
    main()
