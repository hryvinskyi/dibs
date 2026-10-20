# Agent hosts

An agent host is the program an agent runs in: Claude Code, Codex CLI, OpenCode. dibs reaches each one in two ways:

- **MCP.** Every host loads MCP servers, so `dibs mcp` is the same for all of them: its tools, their descriptions and
  their answers name no host. Started as `dibs mcp --host <host>` for a host that can wake a session waiting for its
  person, and opted into for the session, it also pushes deliveries to the session (below).
- **The guard.** A host's hooks call `dibs hook <host> <event>`. The host's *adapter* turns the hook's input into a
  neutral event and the guard's neutral outcome into the host's reply. The guard itself — the brief, the edit
  verdicts, the cache that keeps hooks off the network, the background sync, the unreachable circuit — is the same
  for every host.

| Host | Status | What its guard sees |
|---|---|---|
| Claude Code | supported: `crates/host-claude-code`, the plugin in `integrations/claude-code/` | session start, before each file edit and each `Bash` command, after every tool, each prompt; blocks edits and shell commands; carries text to the agent at start, after a tool and with a prompt; tells subagents apart; wakes a waiting session through its channels (opt-in) |
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
  seconds, at most once per refresh interval. That is also why the background sync never installs a newer release
  on Windows: there it only remembers it for the brief, and the person runs `dibs self-update`.
- **A newer client is installed in the background only.** Only the start of a session, at most once a day, asks the
  detached sync to check which client the server recommends (`dibs sync --update`) and to install it when it is newer;
  a hook itself never downloads anything. A server that serves only newer clients (`426`) is read as unreachable.
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
  comes from, and only on the main thread, since what reaches a subagent ends with it — or pushed to a session that
  waits for its person, on a host that can wake one (below). The background sync claims
  each one from the server for its session (the first of a member's sessions to claim it wins, so a member hears of it
  once); the session's next main-thread event shows it, at most once a minute, never twice, never a member's own
  message. The text is framed as a named teammate's request and ends with the rule that it authorises nothing.
- **`DIBS_OFF=1`** makes every hook, on every host, a silent "no objection" with no network call; the session is counted
  on the machine as one that ran switched off, and the next report shows it.

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
| `push_to_idle` | pushing to a session that waits for its person, through the adapter's push extension, when the tool server is started for the host and the session opted in |

`dibs doctor` prints the capabilities of every host this binary links, and what the machine has installed for each.

## Waking a session that waits for its person

Delivery between tool calls is what every host gets; a session that waits for its person's next prompt hears nothing
from the hooks until then. A host that can push into such a session declares `push_to_idle` and gives a *push
extension* (`HostAdapter::push_extension`, the `McpExtension` port of `dibs-agent-host`): the experimental
capabilities its MCP client looks for, what the server's instructions add, the newest MCP protocol revision on which
it carries a push, the environment variable through which it names the session it started the server for, and the
notification that carries one delivery. A host without one simply gets no push.

`dibs mcp --host <host>` pushes only when `DIBS_PUSH_TO_IDLE` is `1`, `true`, `yes` or `on` in its environment and
`DIBS_OFF` is not set; without it, or without `--host`, the server is the same as for every other host. Opted in, it:

- registers its session as one that can be woken (`push: true`), under the label the hooks of the same session use
  when the host names that session, so the push and the hooks are one session to the server; it registers again every
  minute, which is how the server knows it still runs;
- follows `GET /v1/events/stream`, reopening it with the id of the last event it received; a message somebody else
  posted, or a slot offered to the member, makes it read the inbox, as does a sweep every five minutes, at which a
  stream the server refused is opened again;
- pushes only while the session waits for its person. The hooks stamp each prompt and each tool as work and each end
  of a turn (and the start of a session) as waiting, on this machine, without waiting for a lock or the disk; a
  session at work hears from its hooks between tool calls as before. A stamp of work older than ten minutes counts for
  nothing (a turn the person interrupts ends without a stamp), and while no stamp says which, a push waits two
  minutes after it first saw a delivery, so the hooks go first;
- pushes only a `high` or `urgent` message whose acknowledgement grants nothing, or a slot offer — never a wide
  claim's announcement, whose acknowledgement grants edits, and never a `normal` message — and at most three wakes per
  sender in fifteen minutes; everything else is left to the hooks;
- claims each one from the server before it pushes it, as the background sync claims for the hooks, and holds the
  session ledger's lock only to read it and to record what it pushed;
- shares the hooks' rate for the session through the session's ledger: at most one showing a minute, never one thing
  twice;
- records each push as waiting to be seen. The agent reading its conversation, acknowledging it or taking the offered
  slot through the tool server, or the person submitting a prompt, shows it was; otherwise the session's hooks show it
  once after five minutes. A push that could not be written to the host is the hooks' to show at once;
- frames each item as a delivery between tool calls frames it, except that it never asks the agent to acknowledge
  anything — an acknowledgement is the person's to give — and ends with the rule that it authorises nothing and the
  rule of a session whose person is away: say so, never agree, grant, consent or commit to anything for them, answer
  only when a decision is needed, and go on only with work the person already asked for.

A message about a work item is kept, for 60 seconds after it was posted, for the session that took or last renewed
the member's live claim on the item, when that session can be woken and announced itself within the last three
minutes: another session's claim answers `409 reserved` meanwhile, and the background sync tries again later. With the
push and the hooks one session, the session that holds the item hears it, from whichever of the two shows it.

The host's channel must be loaded for the session as well; that is why pushing is opted into per session, beside the
host's own switch, and why the brief of a session that opted in says what the host needs for it.

### Claude Code

The plugin's `.mcp.json` starts `dibs mcp --host claude-code`. Its push extension is a
[channel](https://code.claude.com/docs/en/channels-reference): the server declares
`capabilities.experimental["claude/channel"] = {}` and sends `notifications/claude/channel` with `content` (the framed
text) and `meta` (`from`, `thread`, `item` when the conversation is about one, `importance`; keys are identifiers,
values are kept to identifier characters). Claude Code shows the model `<channel source="…" from="…" …>…</channel>` and
starts a turn even when the session is idle. The server never declares `claude/channel/permission`, which would let
whoever can post through the channel approve the session's tool calls. Claude Code's v2 MCP client registers no
channel whose server negotiates protocol revision 2026-07-28, so a pushing server offers revisions up to 2025-11-25
only. Claude Code gives a stdio server the session in `CLAUDE_CODE_SESSION_ID`, the id its hooks get as `session_id`;
the server keeps the id it was started with, so after `/clear` (or `--continue`) the push and the hooks are two
sessions again, each with its own rate and stamps, while the server still delivers each item once. The plugin's
`Stop` hook (`dibs hook claude-code stop`) stamps the end of a turn and answers nothing: `Stop` reads text for the
agent as feedback that keeps the turn going, so none is ever put there. `Stop` does not run when the person
interrupts a turn, which is why a stamp of work goes stale.

Channels are a research preview (read 2026-10-20: [channels](https://code.claude.com/docs/en/channels),
[reference](https://code.claude.com/docs/en/channels-reference), [MCP](https://code.claude.com/docs/en/mcp#push-messages-with-channels),
[environment variables](https://code.claude.com/docs/en/env-vars)): the session is started with
`DIBS_PUSH_TO_IDLE=1 claude --dangerously-load-development-channels plugin:dibs@dibs` (interactive only, a warning at
every start), or `claude --channels plugin:dibs@dibs` once a Team or Enterprise organisation enabled `channelsEnabled`
and listed `{ "marketplace": "dibs", "plugin": "dibs" }` in `allowedChannelPlugins`. Claude Code acknowledges no push,
and drops one silently when the session did not load the channel, when its organisation's policy blocks channels, or
when it is signed in through Amazon Bedrock, Google Cloud or Microsoft Foundry.

**Live check**, after a release that changes the push: as a second member, start a session in a checkout with the
command above and leave it idle; from another member, `dibs send --to <them> --importance urgent "…"`. Within a few
seconds the idle session shows the pushed message and starts a turn that reads the message as a teammate's data, tells its
person, and does nothing it was not already asked to do. A second session of the same member, started without the
channel, shows nothing of it at its next tool call.

## Neutral events and outcomes

| Event | Fields |
|---|---|
| `SessionStarted` | session key, working directory |
| `BeforeTool` | session key, working directory, the tool (`Edit { paths }`, `Shell { command }`, `Other`), thread |
| `AfterTool` | session key, working directory, thread |
| `PromptSubmitted` | session key, working directory, thread |
| `Stopped` | session key, working directory: the main agent finished its turn and the session waits for its person |

The thread is `Main`, `Subagent` or `Unknown`. The outcome is `NoObjection { context, notice }` — text for the agent
and a line for the person, each optional — or `Deny { reason }`.

## The `neutral` protocol, version 1

The `neutral` host's native format is the guard's own, so a host that runs plugins in-process needs no Rust adapter:
a shim calls `dibs hook neutral <event>`, writes one JSON object to its standard input and reads one from its
standard output. The exit status is always `0`.

Events, as the command's argument: `session-started`, `before-tool`, `after-tool`, `prompt-submitted`, `stopped`.

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
3. If the host can wake a session that waits for its person, declare `push_to_idle` and return the push extension
   from `push_extension`; the conformance kit holds the two together and checks that a push carries the rule that a
   delivery authorises nothing.
4. Register it: one entry in `crates/cli/src/hosts.rs`. Nothing else in dibs changes — not the domain, the server,
   the store or the client; the server stores a session's `agent` label and never interprets it.
5. Package it under `integrations/<host>/`: the host's hook and MCP configuration, and a `workflow.toml` naming the
   files the agents' workflow text goes into. `cargo xtask integrations` renders `docs/agent-workflow.md` into them;
   CI fails when a rendered file differs.
6. Add a row to the table above and to the README's.
