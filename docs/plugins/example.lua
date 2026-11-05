-- How to write a bit plugin, as one working plugin. `bit plugins guide` prints this file.
--
-- A plugin is a .lua file in ~/.config/bit/plugins/. Every file there loads when bit
-- starts, in alphabetical order, so after adding or changing one:
--   bit plugins     check that every plugin loads, and list each one's hooks
--   bit -c          restart bit, continuing the conversation
--
-- The basics:
--   * There are exactly three hooks: before_<tool>, after_<tool> and tool_<name>. There
--     is no hook for session start, user input, the end of a turn, or drawing in the TUI.
--     If a request needs one, say it isn't possible yet instead of inventing a name.
--     Functions with other names are plain helpers bit never calls.
--   * The function NAME says when it runs.
--   * The PARAMETER NAMES say what it gets: bit passes only what you ask for, by name. A
--     typo like `comand` stops startup with a list of the valid names.
--   * The RETURN VALUE says what happens. Returning nothing means "no opinion".
--
-- Every hook can also ask for:
--   cwd       the project directory
--   mode      "ask" or "auto"
--   headless  true in `bit -p`, where no human can answer
--
-- Built-in tools and their fields:
--   read(path, offset, limit)   write(path, content)        edit(path, old, new)
--   bash(command, timeout)      grep(pattern, path, glob)   glob(pattern, path)
--   ask(question, options)
--
-- The API, all of it:
--   run(cmd [, { timeout = s }])  -> output, exit_code
--       cmd as a table runs argv directly: run{ "git", "status" }
--       cmd as a string runs through sh -c: run("git log | head")
--       stderr comes after stdout; combined output is limited to 1 MiB
--       timeout must be finite and non-negative, at most the hook's remaining budget
--       the process group is killed on exit, error or timeout
--   read(path)                    -> text, or nil, err; relative paths start at cwd
--   ask(question [, options])     -> the human's answer; "" when headless
--   print(...)                    a note to the human: max 20 lines, no colours; the
--                                 model never sees it
--
-- The rules:
--   * Plugins load once, when bit starts: tool_ schemas are fixed for the session.
--   * Lua 5.4 with string, table, math and utf8. There is no io, os, require or debug:
--     use read and run instead. 4 MB of memory per plugin.
--   * Budgets: before_ hooks 2 s, after_ and tool_ 60 s. Time spent in ask() doesn't count.
--   * Plugins fail closed: an error or a timeout in a before_ hook blocks the call. A
--     file that doesn't load stops bit from starting; `bit --no-plugins` skips them all.
--   * Several plugins on one hook run in file order. The first to block wins, and a
--     block beats another plugin's `return true`.

---------------------------------------------------------------------------------------
-- Plugin state. Each file gets one Lua state for the whole session, so top-level
-- variables (local or global) keep their values between calls and work as counters and
-- flags. Plugins can't see each other's variables.
---------------------------------------------------------------------------------------

local edits_since_test = 0
local PROTECTED = { "%.env$", "%.pem$", "id_rsa", "secrets/" }

-- A plain helper: bit only looks at before_*, after_* and tool_* names.
local function is_protected(path)
  for _, pattern in ipairs(PROTECTED) do
    if path:find(pattern) then return true end
  end
  return false
end

---------------------------------------------------------------------------------------
-- before_<tool>: guards. They run before every call of the tool, ahead of bit's own
-- approval, in ask, auto and print mode alike.
--
--   return "reason"      block; the model reads the reason and adapts
--   return { field = }   rewrite the arguments, then continue. Only the tool's own
--                        fields; in ask mode the human approves the rewritten call
--   return true          run without asking the human
--   return nil           no opinion: bit's allowlist, the mode and the human decide
---------------------------------------------------------------------------------------

function before_bash(command, mode, headless)
  -- 1. Ask the human first ("ask me before any rm"). Match the WORD with frontier
  --    patterns: %f[%w]rm%f[%W] finds `rm -r x`, `sudo rm x` and `/bin/rm x`, but not
  --    `npm run` or `perform`. In ask mode bit already asks before every command, so
  --    asking here too would ask twice: only ask in auto mode. Headless, ask() returns ""
  --    and the command is blocked. A shell guard is a speed bump, not a wall: `find
  --    -delete` or a script can still delete files.
  if command:find("%f[%w]rm%f[%W]") then
    if mode == "ask" then return end -- bit's own approval prompt covers it
    if ask("Run `" .. command .. "`?", { "yes", "no" }) ~= "yes" then
      return "The human said no to: " .. command .. ". Don't retry it; ask them what to do."
    end
  end

  -- 2. Stricter when nobody is watching.
  if command:match("^git push") and (mode == "auto" or headless) then
    return "Pushing needs a human. Commit, then stop and say it's ready to push."
  end

  -- 3. Rewrite instead of blocking: a pager would hang a non-interactive shell.
  if command:match("^git log") and not command:find("--no-pager", 1, true) then
    return { command = (command:gsub("^git", "git --no-pager", 1)) }
  end

  -- 4. Approve safe, read-only commands so the human isn't asked.
  if command == "cargo check" or command == "git status" or command == "git diff" then
    return true
  end

  -- 5. Anything else: no opinion. bit asks the human as usual in ask mode.
end

-- Keep secrets out of the conversation: the model never sees these files.
function before_read(path)
  if is_protected(path) then
    return path .. " holds secrets and is off limits. Ask the human for the value you need."
  end
end

-- Ask the human. Headless, ask() returns "" right away, so this blocks: failing closed.
function before_write(path, content)
  if is_protected(path) then
    return "Don't write secrets files."
  end
  if #content > 200000 then
    local answer = ask("Let the model write " .. #content .. " bytes to " .. path .. "?",
      { "yes", "no" })
    if answer ~= "yes" then
      return "The human declined writing a file this large. Split it up."
    end
  end
end

-- Use the file itself to decide.
function before_edit(path)
  local text = read(path)
  if text and text:find("Code generated .* DO NOT EDIT") then
    return path .. " is generated. Edit its source and regenerate instead."
  end
end

---------------------------------------------------------------------------------------
-- after_<tool>: runs after the call. Gets the tool's fields plus `output` (what the
-- model will see) and `ok` (false if the call failed). bash output ends with
-- "[exit N]" when the command failed.
--
--   return "text"            appended to the result the model sees, as [example.lua] text
--   return { output = "…" }  replaces the output the model sees
--   return nil               nothing to add
--
-- An error or a timeout shows a notice and passes the result through unchanged.
---------------------------------------------------------------------------------------

-- Check formatting after every edit of a Go file. run{...} with a table runs the program
-- directly, with no shell.
function after_edit(path, ok)
  edits_since_test = edits_since_test + 1
  if ok and path:match("%.go$") then
    local out, code = run{ "gofmt", "-l", path }
    if code ~= 0 then return "gofmt failed:\n" .. out end
    if out ~= "" then return path .. " isn't gofmt-formatted. Run gofmt -w " .. path end
  end
end

-- Keep secrets out of tool output, and count test runs.
function after_bash(command, output)
  if command:match("^cargo test") then
    edits_since_test = 0
  end

  local clean = output
    :gsub("sk%-[%w_%-]+", "sk-[redacted]")
    :gsub("AKIA%u%u%u%u%u%u%u%u%u%u%u%u%u%u%u%u", "AKIA[redacted]")
  if clean ~= output then
    print("redacted a key from the output of: " .. command) -- shown to the human only
    return { output = clean }
  end

  if output:find("%[exit 127%]$") then
    return "The command wasn't found. Check it's installed before trying alternatives."
  end
end

-- Plugin state at work: a nudge after many unverified edits.
function after_write()
  edits_since_test = edits_since_test + 1
  if edits_since_test >= 10 then
    edits_since_test = 0
    return "That's 10 file changes since the last test run. Run cargo test now."
  end
end

---------------------------------------------------------------------------------------
-- tool_<name>: a tool the model can call, next to read, bash and the rest.
--   * The --- lines right above the function describe it to the model. Required.
--   * ---@param name type [description] types a parameter: string (the default),
--     number, integer, boolean, array, object. A ? after the type makes it optional.
--   * Parameters named cwd, mode or headless are filled in by bit, not the model.
--   * Whatever it returns is the result: a string as is, a table as JSON. error("…")
--     becomes a failed result the model can read. Cyclic/deep tables (over 64 levels)
--     and oversized JSON conversions are rejected.
--   * In ask mode the human approves each call, like bash; auto mode runs it.
--   * before_<name> and after_<name> work on it like on any built-in tool.
---------------------------------------------------------------------------------------

--- Restart a Kubernetes deployment on the homelab cluster.
---@param service string Deployment name, e.g. "api"
---@param namespace string? Defaults to "default"
function tool_deploy(service, namespace)
  if not service:match("^[%w%-]+$") then
    error("bad service name: " .. service)
  end
  local out, code = run{ "kubectl", "rollout", "restart",
    "deployment/" .. service, "-n", namespace or "default" }
  if code ~= 0 then error("kubectl failed:\n" .. out) end
  return out
end

-- A guard on the plugin's own tool: never deploy with nobody watching.
function before_deploy(headless)
  if headless then return "Deploys need a human; none is here." end
end

--- List TODO and FIXME comments in the project, at most `limit` of them.
---@param limit integer? How many, default 20
function tool_todos(limit, cwd)
  local out = run{ "git", "-C", cwd, "grep", "-n", "-E", "TODO|FIXME" }
  local lines = {}
  for line in out:gmatch("[^\n]+") do
    if #lines >= (limit or 20) then break end
    lines[#lines + 1] = line
  end
  if #lines == 0 then return "No TODOs." end
  return table.concat(lines, "\n")
end

-- Last, two mistakes that bit catches at startup, before anything runs (left commented
-- out so this file loads):
--
-- function before_bash(comand) end
--   ✗ example.lua: before_bash: no field `comand`
--     (available: command, timeout, cwd, mode, headless)
--
-- function before_bsh(command) end
--   ✗ example.lua: before_bsh: no tool `bsh` (available: ask, bash, edit, …)
