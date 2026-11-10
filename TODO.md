## Done

- Skills: installable knowledge, marketplace-style. A skill is a folder with a SKILL.md:
  frontmatter (`name`, `description`) plus a body of instructions. Only the frontmatter
  goes in the prompt — one line per skill, with its path — and the model reads the body on
  demand with the existing `read` tool. No new tool, no loop changes, and the per-turn cost
  is one line per skill. Skills are read once at startup, like AGENTS.md, so the prefix
  stays byte-identical per session.
	- v1 scope: skills only, installed from any git repo. No central index (a curated one
	  is a README away, no code), no project-level skill dirs yet, and no `.lua`
	  distribution: that is arbitrary code on install and needs a signing/trust story
	  first. Skills are prompt injection with a friendly name — printing the description
	  at `add` is the review moment, and ask-mode still gates bash/write/edit.
	- Layout: `~/.config/bit/skills/<name>/SKILL.md`. Hand-parse the two frontmatter keys;
	  no YAML dependency.
	- CLI: `bit skills` lists name and description; `bit skills add <repo-or-url>` clones
	  with git, validates the frontmatter and prints the description; `bit skills remove
	  <name>`; `bit skills update` pulls. git on PATH is required, the ripgrep pattern.
	- In chat, `/skills` shows the same list — name, description, a count footer — as a
	  purely local render like `/clear`: one row in the TUI's COMMANDS table, one match
	  arm, no request, nothing in the session log. The model already has the list in its
	  prompt, so the command is for the human. Read-only: installing stays at the shell,
	  where the description review happens.
	- Code: `agent/skills.rs`, `cmd/skills.rs`, `agent/context.rs`, `tui/lib.rs`,
	  `prompt.md`, `README.md`, `ARCHITECTURE.md` §15.

- `/sessions`: list every session and jump to one, in chat. Sessions save continuously
  (`append` per turn), so leaving one loses nothing, and `/clear` already mutates live
  history — a switch is the same plumbing pointed at another file. Idle-only like
  `/clear`; picking replays the loaded messages and reuses the startup
  "resumed <id> (N messages)" note. The session id routes the provider cache, so a
  switch is cache-identical to quitting and `bit -r <id>`; the picked session's
  provider/model apply, the `bit -r` rule.
	- Shows, in a picker reused from `/model` (digits/`Enter` pick, `Esc` stays): the id
	  prefix `bit -r` takes, a preview — the first prompt, or the compact summary once
	  compacted — relative time, newest first, cwd when it isn't this one, the current
	  session marked. All sessions, not just this directory's; `-c` already covers
	  "latest here". Empty: "no other sessions — bit -c continues the last one here".
	- Code: `agent/session.rs::list`, `agent/proto.rs::Resume`, `agent/run.rs`,
	  `tui/lib.rs`, `README.md`, `ARCHITECTURE.md` §16.

## Next

- Enhance caching, partial caching, ways to improve model's cache hits
