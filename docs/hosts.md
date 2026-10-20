# Agent hosts

An agent host is the program an agent runs in: Claude Code, Codex CLI, OpenCode. dibs reaches each one in two ways:

- **MCP.** Every host loads MCP servers, so `dibs mcp` is the same for all of them: its tools, their descriptions and
  their answers name no host.
- **The guard.** A host's hooks call `dibs hook <host> <event>`. The host's *adapter* turns the hook's input into a
  neutral event and the guard's neutral outcome into the host's reply. The guard itself — the brief, the edit
  verdicts, the cache that keeps hooks off the network, the background sync, the unreachable circuit — is the same
  for every host.

| Host | Status | What its guard sees |
|---|---|---|
| Claude Code | supported: `crates/host-claude-code`, the plugin in `integrations/claude-code/` | session start, before each file edit and each `Bash` command, after every tool, each prompt; blocks edits and shell commands; carries text to the agent at start, after a tool and with a prompt; tells subagents apart |
| Codex CLI | planned | command hooks from its hook configuration (project hooks only in a trusted project) |
| OpenCode | planned | an in-process plugin; a small shim calls `dibs hook neutral <event>` |

## The rules every host gets

- **There is no "allow".** The guard either has no objection, which leaves the host's own permission system to
  decide as it would without dibs, or it denies. It never approves a tool call on the person's behalf.
- **Failure is never a verdict.** A configuration or credential that cannot be used, input that cannot be read, an
  unknown host or event, a server that cannot be reached: each is "no objection" with a one-line notice. A hook
  never exits with status `2`, which hosts read as a block.
- **A hook never waits for upkeep.** Heartbeats and refreshed caches run in a detached `dibs sync`; a hook answers
  from the cache and starts one when something is due. The only call a hook waits for is the verdict on a governed
  path in an area the member is not known to hold, and the state of the slots a shell command needs, each bounded to
  two seconds.
  On Windows the detached sync still inherits the hook's own output pipe (the standard library has no stable way
  yet to stop that), so a host that waits for the hook's output to close waits for the sync too: up to a few
  seconds, at most once per refresh interval.
- **A server that failed is left alone for a minute**, and the person is warned at most once a minute.
- **Nothing is produced that the host cannot carry**: each adapter declares its capabilities, and the guard
  computes no answer, and fetches nothing, for something the host would drop.
- **A checkout that governs no repository of the project** — its `.dibs.toml` names no `repo`, an unknown one, or
  one whose remote is not the checkout's `origin` — lets every edit through and says so at most once a minute per
  checkout.
- **A command line never leaves the machine.** A shell command is split into simple commands and judged locally,
  from the cached settings; the server is asked only where the slots it touches stand, and a line no slot governs asks
  nothing.
- **Delivery only where it lands.** A teammate's pressing message, or a slot offered to the member, is shown to the
  agent after a tool runs or with a prompt — only on a host that carries text there and says which thread an event
  comes from, and only on the main thread, since what reaches a subagent ends with it. The background sync claims
  each one from the server for its session (the first of a member's sessions to claim it wins, so a member hears of it
  once); the session's next main-thread event shows it, at most once a minute, never twice, never a member's own
  message. The text is framed as a named teammate's request and ends with the rule that it authorises nothing.
- **`DIBS_OFF=1`** makes every hook, on every host, a silent "no objection" with no network call.

## Capabilities

| Capability | Meaning |
|---|---|
| `deny_edit` | a refusal before a file-writing tool runs stops it |
| `deny_shell` | a refusal before a shell command runs stops it |
| `context_at_start` | text for the agent at the start of a session reaches it |
| `context_after_tool` | text for the agent after a tool ran reaches it |
| `context_at_prompt` | text for the agent when the person submits a prompt reaches it |
| `tells_subagent` | events say whether they come from the main agent or a subagent |
| `push_to_idle` | the host can wake an idle session with a pushed message |

What the guard does with each:

| Capability | What the guard uses it for |
|---|---|
| `deny_edit` | the edit check before a file-writing tool; without it, no edit is checked |
| `deny_shell` | the command check of a shell command against the slots; without it, no command is checked |
| `context_at_start` | the brief |
| `context_after_tool` | delivery between tool calls |
| `context_at_prompt` | delivery with a prompt |
| `tells_subagent` | delivery at all: on a host whose thread is always `Unknown`, nothing is claimed or shown |
| `push_to_idle` | nothing yet |

`dibs doctor` prints the capabilities of every host this binary links, and what the machine has installed for each.

## Neutral events and outcomes

| Event | Fields |
|---|---|
| `SessionStarted` | session key, working directory |
| `BeforeTool` | session key, working directory, the tool (`Edit { paths }`, `Shell { command }`, `Other`), thread |
| `AfterTool` | session key, working directory, thread |
| `PromptSubmitted` | session key, working directory, thread |

The thread is `Main`, `Subagent` or `Unknown`. The outcome is `NoObjection { context, notice }` — text for the agent
and a line for the person, each optional — or `Deny { reason }`.

## The `neutral` protocol, version 1

The `neutral` host's native format is the guard's own, so a host that runs plugins in-process needs no Rust adapter:
a shim calls `dibs hook neutral <event>`, writes one JSON object to its standard input and reads one from its
standard output. The exit status is always `0`.

Events, as the command's argument: `session-started`, `before-tool`, `after-tool`, `prompt-submitted`.

Input:

```json
{"protocol": 1, "session": "s-1", "cwd": "/work/api", "thread": "main",
 "tool": {"kind": "edit", "paths": ["src/billing/a.rs"]}}
```

| Field | Required | Meaning |
|---|---|---|
| `protocol` | always | `1` |
| `session` | always | the host's id of the session, 1 to 256 characters without control characters |
| `cwd` | always | the session's working directory |
| `thread` | no | `main`, `subagent` or `unknown` (the default); read for `before-tool`, `after-tool` and `prompt-submitted`; only `main` is delivered to |
| `tool` | for `before-tool` | `{"kind": "edit", "paths": [...]}` (absolute, or relative to `cwd`), `{"kind": "shell", "command": "..."}`, or any other `kind`, which is a tool the guard has no say about |

Output:

```json
{"protocol": 1, "decision": "no_objection", "context": "...", "notice": "..."}
{"protocol": 1, "decision": "deny", "reason": "..."}
```

`context` and `notice` are left out when there is none. A shim shows `context` to the agent, `notice` to the
person, and on `deny` stops the tool with `reason`.

Within protocol 1 a field may be added to either side, and none is renamed or removed; readers ignore fields they do
not know. A different `protocol` number is answered with a notice and no decision.

## Adding a host

1. Write an adapter: a crate `crates/host-<host>` implementing `HostAdapter` from `dibs-agent-host` — `id`,
   `capabilities`, `parse` (the host's hook input to a neutral event) and `render` (a neutral outcome to the host's
   reply, given the event name `parse` was given, since some hosts name the event in their reply; a name the host
   does not send gets a reply every one of its events accepts). An adapter is pure: no file, clock or network. A
   host that can only run in-process code needs no crate: its shim speaks the `neutral` protocol.
2. Pass the conformance kit: the adapter's tests call `dibs_agent_host::conformance::check` (feature
   `conformance`) with fixtures of the host's native input and the events they parse to, the replies each outcome
   must give on each event, and a reader of the host's replies. A host is accepted when its fixtures pass. Tests
   that run `dibs hook <host> …` through the binary are a test target of `crates/cli` whose source lives in
   `crates/host-<host>/cli-tests/`, so that only the adapter's directory names the host.
3. Register it: one entry in `crates/cli/src/hosts.rs`. Nothing else in dibs changes — not the domain, the server,
   the store or the client; the server stores a session's `agent` label and never interprets it.
4. Package it under `integrations/<host>/`: the host's hook and MCP configuration, and a `workflow.toml` naming the
   files the agents' workflow text goes into. `cargo xtask integrations` renders `docs/agent-workflow.md` into them;
   CI fails when a rendered file differs.
5. Add a row to the table above and to the README's.
