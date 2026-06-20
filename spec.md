# Codex Desktop Ona Environment Integration Spec

## Context and Findings

The goal is to make Codex Desktop able to code through secure Ona environments instead of requiring the user's local machine to contain the repository, dependencies, secrets, and services.

Relevant existing Codex capabilities:

- The CLI already supports connecting the interactive TUI to a remote app-server with `--remote`, but this is app-server control, not a general SSH host picker, and `codex app` currently rejects the root `--remote` flag.
- `codex app` in this repo only opens or installs the platform Desktop app for a local workspace path. Desktop GUI changes are out of scope for this repository; this spec targets the backend/protocol support the existing app flow can consume.
- The app-server already owns an `EnvironmentManager` for local and remote execution/filesystem environments.
- `CODEX_HOME/environments.toml` already supports named environments backed by either a WebSocket exec-server URL or a stdio command such as `ssh <host> codex exec-server --listen stdio`.
- The app-server protocol already has experimental `environment/add`, but it only accepts `environmentId` plus `execServerUrl`; it cannot add a stdio/SSH-backed environment at runtime.
- `thread/start` and `turn/start` already accept environment selections with `environmentId` and environment-native `cwd`, so once an Ona environment is registered as a Codex execution environment, existing agent turns can run tools against it.

Relevant Ona capabilities from public docs:

- `ona environment list -o json` lists environments for machine parsing.
- `ona environment start <id-or-name>` starts a stopped environment.
- `ona environment ssh-config` configures local SSH aliases, after which `<environment-id>.ona.environment` can be used as an SSH host.
- `ona environment ssh <id-or-name>` supports interactive SSH or single commands.
- Running commands through `ona environment exec` uses EnvironmentOps, while persistent interactive access is through SSH.
- Ona Cloud environments may run Codex as a native agent service. In the current workspace that service downloads a `codex-exec-agent` under a service-managed shared path, while `codex` is not available on the interactive shell `PATH`; this path should be treated as an implementation detail unless Ona exposes it as metadata or a documented command.

The most compatible path is to connect Codex to Ona environments as remote exec-server environments over SSH. The Desktop app should not treat Ona environments as local paths, and should not try to mirror files locally.

## Requirements

1. Existing Codex Desktop environment-selection support can discover Ona environments through Codex's app-server/backend surface.
2. Users can connect a Codex thread to any running Ona environment they can access.
3. The backend can optionally include stopped environments in discovery results and start one on demand before connecting.
4. The selected Ona environment becomes the thread's primary execution/filesystem environment, so shell commands, file reads/writes, patch application, MCP stdio configured for that environment, and image/file tools operate inside the Ona environment.
5. The integration uses existing Ona authentication and SSH access. Codex must not ask users to paste tokens into Desktop, persist Ona credentials, or duplicate Ona's SSH key management.
6. Codex must discover and verify a compatible remote exec-server command before starting a coding session.
7. If no compatible remote Codex/exec-agent command can be discovered, the MVP must give a clear, actionable error. A follow-up can offer a user-confirmed bootstrap path.
8. Local Codex behavior remains unchanged when no Ona environment is selected.
9. Existing app-server/TUI remote-control behavior remains unchanged; the Ona integration is a separate execution-environment feature.

## Constraints

- Do not put this integration in `codex-core` unless a core type already owns the behavior being extended. Prefer a new small crate or app-server/request-processor module for Ona-specific discovery and orchestration.
- Use app-server v2 protocol for any new app-facing API surface.
- Do not require Desktop GUI changes in this repository. The implementation should expose backend capabilities that existing Desktop support can call.
- If app-server protocol shapes change, regenerate app-server schemas and TypeScript fixtures.
- Environment IDs registered with `EnvironmentManager` must satisfy the current constraints: ASCII letters, numbers, `-`, `_`, not `local`, not `none`, and at most 64 bytes. Use a stable id such as `ona-<environment-uuid-with-hyphens>`.
- Do not rely on Desktop's local bundled binary being executable inside Ona. Ona environments are Linux dev containers; Desktop may be macOS or Windows. The remote side needs its own compatible Codex CLI or exec-agent command that can speak the exec-server protocol.
- Avoid shell string construction with untrusted environment fields. Build argv arrays and use only validated environment IDs/SSH hosts.
- Do not store Ona tokens or SSH private keys in Codex config. Rely on the Ona CLI and generated SSH configuration.
- Prefer `ona` as the CLI binary name. During transition, allow a configured binary path or fallback to `gitpod` when `ona` is unavailable.
- Support macOS and Windows Desktop clients. Windows requires OpenSSH availability or a clear unsupported/preflight error.
- Preserve remote path semantics. Paths inside Ona are environment-native and must not be canonicalized against the Desktop host.
- Treat all Ona CLI JSON and environment metadata as untrusted input.

## Architecture

### New Integration Layer

Add a small Ona integration layer outside `codex-core`, for example `codex-rs/ona-environments` or an app-server-local module if a new crate is unnecessary.

Responsibilities:

- Locate the Ona CLI binary (`ona`, configured override, then optional `gitpod` fallback).
- Run Ona CLI commands with JSON output and bounded timeouts.
- Parse environment list/start/get results into Codex-owned structs.
- Run `ona environment ssh-config` when SSH configuration is missing or stale.
- Discover a safe SSH stdio command for a selected environment:
  - Prefer an Ona-provided Codex agent/exec-server command or binary path if the Ona CLI/API exposes one.
  - Otherwise probe the remote shell for `codex` on `PATH` and use `codex exec-server --listen stdio`.
  - Only use service-managed paths such as `codex-exec-agent` when they are documented or reported by Ona metadata and expose an exec-server-compatible mode.
- Preflight remote readiness:
  - environment phase is running, or start was requested and completed.
  - SSH host resolves after `ona environment ssh-config`.
  - remote exec-server command discovery succeeds.
  - the discovered command can complete the exec-server initialize handshake when connected by `EnvironmentManager`.

### App-Server Protocol

Add app-facing app-server v2 APIs. Exact names can be adjusted during implementation, but the protocol should cover:

- `onaEnvironment/list`
  - params: include stopped/archived toggle, optional search text, optional limit/cursor if Ona supports pagination.
  - response: normalized environment records and integration status.
- `onaEnvironment/start`
  - params: environment id or exact name.
  - response: updated environment record once running or a clear error.
- `onaEnvironment/connect`
  - params: environment id, optional cwd, optional make default for current thread/session.
  - behavior: ensure SSH config, build stdio transport, register the environment with `EnvironmentManager`, and return the Codex environment id plus recommended cwd.
- Optional `onaEnvironment/preflight`
  - useful if the app wants to separate validation from connection.

Extend the generic environment API so app-server can add a stdio-backed environment at runtime. Either:

- extend `environment/add` with a tagged `transport` union supporting `websocket` and `stdio`, while preserving the existing `execServerUrl` shape for compatibility, or
- add a new experimental method such as `environment/upsert` with the tagged transport and leave `environment/add` untouched.

The stdio transport should map to the existing `ExecServerTransportParams::StdioCommand` machinery already used by `environments.toml`.

### Existing App Flow

1. User opens Codex Desktop.
2. User uses the app's existing environment-selection support.
3. Desktop calls `onaEnvironment/list`.
4. Desktop receives normalized Ona environment records, defaulting to running environments.
5. If the existing app flow permits starting stopped environments, Desktop calls `onaEnvironment/start`.
6. Desktop calls `onaEnvironment/connect`.
7. App-server registers `ona-<environment-id>` as a remote stdio exec environment.
8. Desktop starts a new thread or updates the current thread with:
   - `environments: [{ environmentId: "ona-<environment-id>", cwd: "<remote-workspace-path>" }]`
   - `cwd` only when needed for legacy fallback behavior.
9. Agent turns run against the Ona environment through the exec-server connection.

### Remote Codex Binary Strategy

MVP:

- Treat the remote binary location as discoverable, not fixed.
- Discovery order:
  1. Ask Ona for Codex agent metadata or an exec-server command if the Ona CLI/API exposes it.
  2. Probe the remote environment over SSH with `command -v codex`; if present, run `codex exec-server --listen stdio`.
  3. Probe explicitly configured command overrides from Codex config for organizations that install Codex in a known location.
  4. Reject with an actionable error if none of the above produces an exec-server-compatible command.
- Do not hard-code the current service-managed `codex-exec-agent` path. It is useful evidence that Ona provisions Codex separately from the interactive shell, but it should become an integration point only through documented Ona metadata or an explicit command override.
- Preflight the selected command by completing the exec-server initialize handshake before binding a thread to the environment.

Follow-up:

- Add user-confirmed bootstrap using an official Codex install path, a project-defined Ona task/service, or a documented Ona-provided Codex agent executable.
- Cache bootstrap status per environment id only as non-secret metadata.
- Never silently install or update remote binaries without user consent.

### Non-Running Environments

MVP:

- List running environments by default.
- Support an `includeStopped` style discovery option; the app decides whether and how to expose it.
- Start stopped environments on demand only after user confirmation.

Follow-up:

- Expose backend support for creating new environments from Ona projects or repository URLs.
- Support selecting environment class, branch, timeout, and project through the existing app flow if needed.

## Implementation Steps

1. Add an Ona integration adapter.
   - Implement CLI discovery and JSON command helpers with timeouts.
   - Add typed parsing for `environment list`, `environment get`, `environment start`, and `environment ssh-config`.
   - Unit test parsing with representative Ona JSON and error output.

2. Extend runtime environment registration.
   - Add an app-server request path that can register a stdio exec-server transport at runtime.
   - Reuse `EnvironmentManager` and existing `ExecServerTransportParams::StdioCommand`.
   - Keep existing `environment/add` WebSocket behavior compatible.

3. Add Ona app-server APIs.
   - Define v2 protocol params/responses and export schemas/TypeScript.
   - Implement request processors for list, start, preflight/connect.
   - Return structured errors for missing Ona CLI, unauthenticated Ona CLI, missing SSH config, stopped environment, missing remote Codex/exec-agent command, and remote exec-server handshake failure.

4. Wire thread startup/selection.
   - Ensure the selected Ona environment id can be passed to `thread/start` and `turn/start`.
   - Choose or discover a remote cwd. Prefer Ona-provided workspace root if available; otherwise probe common workspace roots via SSH/exec and require the user to select when ambiguous.
   - Preserve sticky environment selection for subsequent turns.

5. Add app integration contract.
   - Document the new app-server calls and expected request/response sequence.
   - Provide protocol contract and sample request/response fixtures here for the existing Desktop integration.
   - Keep `codex app` as the launcher; do not make it responsible for direct Ona orchestration unless Desktop needs launch-time hints.

6. Add tests.
   - Protocol serialization/schema tests for new API shapes.
   - App-server tests with a fake Ona CLI binary in `PATH`.
   - Environment registration tests proving stdio transport can be added at runtime.
   - Integration test with a fake SSH command that starts `codex exec-server --listen stdio`.
   - Negative tests for unauthenticated Ona CLI, stopped environment without start permission, invalid environment id, missing remote Codex/exec-agent command, and a discovered command that fails the exec-server handshake.

7. Validate.
   - Run `just fmt` in `codex-rs`.
   - Run focused tests for changed crates, likely `just test -p codex-app-server-protocol`, `just test -p codex-app-server`, and `just test -p codex-exec-server`.
   - If shared execution or protocol crates are touched, ask before running the full `just test` suite.

## Success Criteria

- A Desktop client can list Ona environments through app-server without direct Ona API knowledge.
- Selecting a running Ona environment registers it as a Codex execution environment without restarting app-server.
- A new or existing Codex thread can run shell and filesystem tools inside the selected Ona environment.
- The integration uses Ona CLI/SSH authentication and does not persist Ona secrets in Codex.
- Stopped environments can be started on demand after explicit user confirmation.
- Missing prerequisites produce actionable errors instead of silent fallback to local execution.
- Existing local Desktop, CLI, TUI, app-server remote-control, and `environments.toml` behavior remains compatible.
- App-server schema and TypeScript fixtures reflect the new protocol.
- Focused Rust tests pass for the touched crates.

## Open Questions for Implementation

- What is the preferred long-term Ona dependency: shelling out to the Ona CLI, or adding a native Ona API client? The MVP should use the CLI because it reuses login and SSH setup, but a native API client may provide a smoother Desktop experience later.
- Should Codex provide a remote binary bootstrap flow in the first release, require projects to install Codex in the Dev Container initially, or rely on a documented Ona-provided Codex agent executable?
- Can Ona expose the canonical workspace root in `environment list/get -o json`? If not, Codex needs a bounded remote probe and a user selection fallback.
