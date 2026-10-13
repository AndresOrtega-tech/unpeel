# GitHub connection support

This support package owns GitHub CLI authentication for Settings → Git. It
has no `runtime.toml`: GitHub CLI is a workspace tool, not an agent runtime,
and must not appear in the agent catalog or create launch presets.

`adapter/git_settings.rs` is compiled into the native Host. It uses `gh`'s
normal credential store and Git's global user configuration. A workspace
selects the Host on which commands run; workspaces sharing an OS user also
share that user's Git configuration and GitHub credentials.

The adapter runs `gh auth login --web` without a TTY, reads only its strict
device-code notice, and publishes GitHub's fixed device authorization URL.
The Controller opens that page locally. No tokens, raw command output, or
arbitrary terminal URLs are returned. GitHub CLI remains responsible for
OAuth, credential storage, and verification; Unpeel does not register a second
OAuth application or copy credentials between machines.

Reads use `gh auth status --active --json hosts` and project account metadata
only. Older distribution packages use `gh api user` as a fallback; network
failures remain errors rather than being reported as signed out.
Sign-in is followed by `gh auth setup-git`. Disconnect removes the named
account with `gh auth logout`; it does not revoke the OAuth grant on GitHub.
Environment-provided tokens remain under the Host operator's control.
The wire response reports only a credential-storage category, never the
underlying path. A plain-text fallback is shown in the Controller; unknown
storage from an older CLI is not presented as a verified credential store.
See the [security boundary](../../docs/agents/clients/git-settings.md#security-boundary).

Commands are bounded and cancellable through core's `host_command` helper,
which verifies pid/start-time identity before signaling a process group.
Mutation jobs serialize across registered workspace workers using the
machine home's `git-settings.lock`; completion announces on the state bus.

Tests include strict challenge/account parsing, isolated locking, the portable
Controller's capability checks, and `crates/unpeel-cli/tests/cases/git_settings.py`,
which exercises the real Host with a fake GitHub CLI and private Git config.
