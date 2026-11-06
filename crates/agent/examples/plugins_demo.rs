//! Executable scenes for assets/plugins-demo/demo.tape. No model or API key needed.
use agent::plugins::{Before, Fields, Plugins, Req};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::mpsc::unbounded_channel;

fn load(files: &[(&str, &str)]) -> Result<Plugins, String> {
    let dir = std::env::temp_dir().join(format!("bit-plugin-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Every scene starts in a fresh process, with its own fixtures.
    for (name, src) in files {
        std::fs::write(dir.join(name), src).unwrap();
    }
    let result = Plugins::load(&dir);
    std::fs::remove_dir_all(dir).unwrap();
    result
}
fn fields(headless: bool) -> Fields {
    Fields { cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")), mode: "auto", headless }
}
fn before(p: &Plugins, tool: &str, args: &str) -> (Before, String) {
    let (tx, _rx) = unbounded_channel();
    let mut args = args.to_owned();
    let result = p.before(tool, &mut args, &fields(true), &tx, &mut Vec::new());
    println!("  {tool} {args}\n  => {result:?}");
    (result, args)
}
fn tool(p: &Plugins, name: &str, args: &str) -> tools::Output {
    let (tx, _rx) = unbounded_channel();
    let out = p.run_tool(name, args, &fields(true), &tx);
    println!("  {name} {args}\n  => ok={}  {}", out.ok, out.content);
    out
}
fn after(p: &Plugins, content: &str) -> (tools::Output, Vec<String>) {
    let (tx, mut rx) = unbounded_channel();
    let mut out = tools::Output { ok: true, content: content.into() };
    p.after("bash", "{}", &mut out, &fields(true), &tx, &mut Vec::new());
    println!("  model sees: {:?} (ok={})", out.content, out.ok);
    let mut notices = vec![];
    while let Ok(Req::Print(s)) = rx.try_recv() {
        println!("  human sees: {s}");
        notices.push(s);
    }
    (out, notices)
}
fn show(src: &str) {
    println!("{src}\n");
}
fn bad(src: &str, expected: &str) {
    let e = load(&[("plugin.lua", src)]).err().expect("must reject");
    assert!(e.contains(expected), "{e}");
    println!("  {e}");
}
fn main() {
    let scene = std::env::args().nth(1).unwrap_or_else(|| "intro".into());
    match scene.as_str() {
        "intro" => {
            println!(
                "bit / Lua plugins\n\nOne .lua file. Three function prefixes. No SDK.\n\n  before_<tool>    block, allow, rewrite or pass\n  after_<tool>     append or replace the result\n  tool_<name>      add a tool the model can call\n\nFiles: ~/.config/bit/plugins/*.lua\nLoad alphabetically at startup; restart after editing.\n\nThis demo executes the real Plugins API.\nNo model, API keys or remote services are used."
            );
        }
        "guards" => {
            let src = "function before_bash(command)\n  if command == 'danger' then return 'Stop: protected action' end\n  if command == 'safe' then return true end\n  if command == 'rewrite' then return { command = 'echo rewritten' } end\n  if command == 'false' then return false end\nend";
            show(src);
            let p = load(&[("guard.lua", src)]).unwrap();
            for c in ["danger", "safe", "rewrite", "false", "other"] {
                let (v, a) = before(&p, "bash", &format!("{{\"command\":\"{c}\"}}"));
                match c {
                    "danger" => assert!(matches!(v, Before::Block(_))),
                    "safe" => assert_eq!(v, Before::Allow),
                    "rewrite" => assert!(a.contains("echo rewritten")),
                    _ => assert_eq!(v, Before::Pass),
                }
            }
        }
        "ordering" => {
            show(
                "a.lua:  function before_bash() return true end\nb.lua:  function before_bash() return 'b blocks' end\nc.lua:  function before_bash() error('must never run') end",
            );
            let p = load(&[
                ("a.lua", "function before_bash() return true end"),
                ("b.lua", "function before_bash() return 'b blocks' end"),
                ("c.lua", "function before_bash() error('must never run') end"),
            ])
            .unwrap();
            assert!(
                matches!(before(&p,"bash","{}").0, Before::Block(s) if s.ends_with("b blocks"))
            );
            println!(
                "\nAlphabetical order. A block wins over an earlier allow.\nThe first block stops the chain, including c.lua."
            );
        }
        "after" => {
            show(
                "a.lua: after_bash(output) -> { output = output:upper() }\nb.lua: after_bash()       -> 'extra context'\nc.lua: after_bash()       -> print('notice'); error('oops')",
            );
            let p = load(&[
                ("a.lua", "function after_bash(output) return { output = output:upper() } end"),
                ("b.lua", "function after_bash() return 'extra context' end"),
                ("c.lua", "function after_bash() print('notice'); error('oops') end"),
            ])
            .unwrap();
            let (out, n) = after(&p, "done");
            assert_eq!(out.content, "DONE\n[b.lua] extra context");
            assert_eq!(n.len(), 2);
            println!(
                "\nAfter-hook errors emit a notice and preserve the result.\nprint() is for the human; returned text goes to the model."
            );
        }
        "tools" => {
            let src = "--- Greet someone.\n---@param name string\n---@param times integer? Optional repeat count\nfunction tool_greet(name, times, cwd, mode, headless)\n  if name == 'error' then error('bad name') end\n  if name == 'nil' then return nil end\n  return { greeting = 'Hello ' .. name, times = times or 1 }\nend";
            show(src);
            let p = load(&[("greet.lua", src)]).unwrap();
            let schema: serde_json::Value = serde_json::from_str(p.schemas()).unwrap();
            println!("  required = {}", schema["function"]["parameters"]["required"]);
            assert_eq!(schema["function"]["parameters"]["required"], serde_json::json!(["name"]));
            assert!(tool(&p, "greet", r#"{"name":"Ada"}"#).ok);
            assert!(!tool(&p, "greet", r#"{"name":"error"}"#).ok);
            assert_eq!(tool(&p, "greet", r#"{"name":"nil"}"#).content, "");
            assert!(!tool(&p, "missing", "{}").ok);
            println!("\nCommon fields are injected, not exposed as model arguments.");
        }
        "cross-file" => {
            show(
                "a.lua: function before_greet(name)\n         if name == 'root' then return 'No root greetings' end\n       end\nz.lua: --- Greet.\n       function tool_greet(name) return 'Hello ' .. name end",
            );
            let p=load(&[("a.lua","function before_greet(name) if name == 'root' then return 'No root greetings' end end"),("z.lua","--- Greet.\nfunction tool_greet(name) return 'Hello ' .. name end")]).unwrap();
            assert!(matches!(before(&p, "greet", r#"{"name":"root"}"#).0, Before::Block(_)));
            assert_eq!(before(&p, "greet", r#"{"name":"Ada"}"#).0, Before::Pass);
            println!("\nGuards can target a custom tool declared in a later file.");
        }
        "ask" => {
            show(
                "function before_bash()\n  if ask('Proceed?', { 'yes', 'no' }) ~= 'yes' then\n    return 'No approval'\n  end\nend",
            );
            let p=Arc::new(load(&[("ask.lua","function before_bash() if ask('Proceed?', {'yes','no'}) ~= 'yes' then return 'No approval' end end")]).unwrap());
            println!("HEADLESS: ask() returns an empty string immediately.");
            assert!(matches!(before(&p, "bash", "{}").0, Before::Block(_)));
            for answer in ["yes", "no"] {
                let q = p.clone();
                let (tx, mut rx) = unbounded_channel();
                let worker = std::thread::spawn(move || {
                    q.before("bash", &mut "{}".into(), &fields(false), &tx, &mut Vec::new())
                });
                if let Some(Req::Ask { question, options, reply }) = rx.blocking_recv() {
                    println!("\n  UI request: {question} {options:?}\n  demo response: {answer}");
                    reply.send(answer.into()).unwrap();
                } else {
                    panic!("expected question");
                }
                let v = worker.join().unwrap();
                println!("  => {v:?}");
                if answer == "yes" {
                    assert_eq!(v, Before::Pass)
                } else {
                    assert!(matches!(v, Before::Block(_)))
                }
            }
            println!(
                "\nResponses above are supplied by this demo harness.\nTime waiting for a human does not consume the hook budget."
            );
        }
        "api" => {
            let src = "--- Exercise the plugin API.\nfunction tool_api()\n  local a, code = run('printf shell; exit 3')\n  local b = run{ 'printf', '%s', 'argv' }\n  local text = read('Cargo.toml')\n  local missing, err = read('/bit-demo-does-not-exist')\n  return { shell = a, exit = code, argv = b,\n           read = text ~= nil, missing = missing == nil }\nend";
            show(src);
            let p = load(&[("api.lua", src)]).unwrap();
            let out = tool(&p, "api", "{}");
            assert!(out.ok);
            assert!(out.content.contains("\"exit\":3"));
            println!(
                "\nStrings use sh -c; tables pass argv directly.\nread() returns nil + error for missing files."
            );
        }
        "state" => {
            let src = "local count = 0\n--- Count calls in this session.\nfunction tool_count()\n  count = count + 1\n  return count\nend";
            show(src);
            let p = load(&[("state.lua", src)]).unwrap();
            assert_eq!(tool(&p, "count", "{}").content, "1");
            assert_eq!(tool(&p, "count", "{}").content, "2");
            let fresh = load(&[("state.lua", src)]).unwrap();
            assert_eq!(tool(&fresh, "count", "{}").content, "1");
            println!("\nGlobals persist per file, per session. Reload resets state.");
        }
        "validation" => {
            println!("Startup rejects malformed plugins before tools run:\n");
            for (label, src, expected) in [
                ("Typo in field", "function before_bash(comand) end", "no field"),
                ("Unknown tool", "function before_bsh() end", "no tool"),
                ("Syntax error", "function before_bash(", "plugin.lua"),
                ("Built-in collision", "--- x\nfunction tool_bash() end", "already a tool"),
                ("Missing description", "function tool_x() end", "describe it"),
                ("Invalid type", "--- x\n---@param n int\nfunction tool_x(n) end", "has type"),
                (
                    "Unknown annotation",
                    "--- x\n---@param wrong string\nfunction tool_x(n) end",
                    "isn't one",
                ),
            ] {
                println!("{label}:");
                bad(src, expected);
                println!();
            }
        }
        "failures" => {
            println!("Guards fail closed:\n");
            for (label, src, expected) in [
                ("Lua error", "function before_bash() error('boom') end", "boom"),
                (
                    "Unsupported return",
                    "function before_bash() return 42 end",
                    "returned a integer",
                ),
                (
                    "Unknown rewrite field",
                    "function before_bash() return { nope = 1 } end",
                    "isn't a field",
                ),
                (
                    "2-second instruction deadline",
                    "function before_bash() while true do end end",
                    "timed out",
                ),
                (
                    "4-MB memory limit",
                    "function before_bash() return string.rep('x', 8000000) end",
                    "memory",
                ),
                (
                    "run() timeout",
                    "function before_bash() run('sleep 1', {timeout=0.05}) end",
                    "timed out",
                ),
            ] {
                println!("{label}:");
                let p = load(&[("fail.lua", src)]).unwrap();
                let (v, _) = before(&p, "bash", "{}");
                assert!(matches!(v,Before::Block(s) if s.contains(expected)));
                println!();
            }
        }
        "policy" => {
            println!("Real approval-policy checks (no model needed):\n");
            let mut p = agent::Policy::new(vec!["cargo test".into()], vec!["deploy".into()]);
            for (auto, name, args, expected) in [
                (false, "read", "{}", true),
                (false, "bash", r#"{"command":"echo hi"}"#, false),
                (false, "deploy", "{}", false),
                (true, "deploy", "{}", true),
                (false, "bash", r#"{"command":"cargo test -p agent"}"#, true),
                (false, "bash", r#"{"command":"cargo test; echo injected"}"#, false),
            ] {
                let allowed = p.check(auto, name, args);
                assert_eq!(allowed, expected);
                println!("  auto={auto:<5} {name:<6} {args} => allowed={allowed}");
            }
            p.always("deploy", "{}");
            assert!(p.check(false, "deploy", "{}"));
            println!(
                "\n  Always approve deploy => later deploy calls are allowed.\n\nAsk mode prompts for custom tools, like bash.\nHeadless print mode declines calls that need approval.\nBefore hooks run first; blocked calls never reach execution.\nRewritten arguments are the arguments approved and executed.\nA before-hook allow skips approval; a block wins in every mode."
            );
        }
        "limits" => {
            println!("Load-time limits:\n");
            bad("while true do end", "timed out");
            bad("io.open('x')", "nil value");
            bad("require('socket')", "nil value");
            let p = load(&[]).unwrap();
            assert!(p.tools().is_empty());
            println!(
                "\n  Empty plugin directory => no custom tools; normal startup.\n\nRecovery: bit plugins checks each file.\n          bit --no-plugins skips all plugins.\n\nBoundaries to remember:\n  before: 2 s; after/custom tools: 60 s; Lua memory: 4 MB\n  no io, os, require, debug or bytecode loading\n  run() can execute shell commands; this is not an OS sandbox\n  run() timeout kills the direct child, not its descendants\n  shell-pattern guards cannot catch every destructive command\n  no session/input/UI hooks; missing model arguments arrive as nil"
            );
        }
        _ => panic!("unknown scene: {scene}"),
    }
    println!("\n[scene verified]");
}
