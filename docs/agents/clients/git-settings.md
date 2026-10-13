# Git settings

Settings → Git follows the selected workspace through the ordinary Host
protocol on local, loopback, SSH, Direct, and Link connections. Both panels
use `GitSettingsPanel`; changing scope recreates its state, cancels presentation
tasks, and fences pending reads through `RemoteHostRuntime`'s connection
generation. An old Host's device challenge must never open on a new workspace.

The tab provides GitHub sign-in/status/disconnect, Git HTTPS credential-helper
setup, and global Git author name/email. It states that these belong to the
Host's OS user: separate workspaces under that user share the same Git and
GitHub configuration. Repository overrides retain their usual Git semantics.
Missing `gh` offers a Host-derived package-manager command in a normal setup
terminal, plus installation instructions. The terminal uses an existing
workspace project, and hiding it leaves the Session available in the workspace.

## Contract (protocol minor 22)

- `settings.git.read`: `GET /mobile/git-settings` returns cached metadata and
  operation progress. The first read starts a bounded background probe;
  bootstrap never starts a Git or GitHub process. Results refresh after 60
  seconds or through the explicit refresh action.
- `settings.git.set`: `POST /mobile/git-settings` accepts an `action` and a
  UUID `requestID`. Actions: `connectGithub`, `cancelGithub` (with the original
  `operationID`), `disconnectGithub` (with the exact `login`), `setupGit`,
  `setIdentity` (with `name` and `email`), and `refresh`.
- A successful POST acknowledges the job, not completed authorization. Poll
  GET for `operation.state`: `running`, `awaitingAuthorization`, `complete`,
  `failed`, or `cancelled`. Concurrent writes return 409. Repeating the current
  job's requestID does not start it twice; effects are never automatically
  replayed after an uncertain transport outcome.
- A device challenge contains only `userCode` and `verificationUrl`. The Mac
  copies the code and opens the fixed GitHub device page only for a sign-in
  that Controller initiated. A resumed view can explicitly open the challenge.
  Git settings responses never include tokens or raw subprocess output.
- Cancel names the original login job; a stale cancel cannot stop a newer
  sign-in. Cancellation rechecks account status because authorization may
  already have completed. Disconnect removes local `gh` credentials and does
  not revoke GitHub authorization.

The provider adapter lives in `runtimes/github/`; core owns command bounds,
pid verification, routes, and the Controller's transport/effect semantics.
Older Hosts show an update/connection explanation and receive no guessed
routes or local fallback commands. Shared wire types live in `UnpeelShared`;
the first settings UI is the Mac app.

## Security boundary

GitHub authorization runs between the selected Host and GitHub. The local
Controller opens GitHub's device page; Unpeel's operated services receive no
GitHub OAuth callback, credential, or credential escrow. No new remote access
key, support account, or service-side workspace access is created by this
feature. The routes use the existing authenticated Host connection: paired
Direct uses pinned TLS, Link carries end-to-end encrypted payloads, and SSH
uses the user's SSH connection. Local workspaces use the existing local
Controller channel. The Git endpoints do not introduce a public auth callback.

This feature returns only account metadata and a temporary user code. The
device code is sensitive authorization material and must not be logged or
sent to analytics. The Host keeps it in memory until the operation finishes;
raw subprocess output and credential paths are not returned. Account storage
is reported as `keyring`, `file`, `environment`, or `unknown`. Older CLI
versions cannot prove the storage type and must not be labeled secure.

GitHub CLI can fall back to a plain-text credential file when a system
credential store is unavailable; the UI reports that state, and Unpeel never
requests `--insecure-storage`. See [GitHub CLI's storage behavior](https://cli.github.com/manual/gh_auth_login).
The Host's OS user, privileged administrators, and agents running as that
user can access its credentials. Workspace selection is a routing boundary,
not isolation between processes sharing an OS user. Independent trust domains
require separate user-controlled machines or OS accounts.
An authorized user or agent can still expose credentials through other tools,
such as by printing a credential file in a terminal; Git settings do not
sandbox the rest of the workspace.

This implementation does not establish a zero-access managed hosting
guarantee. An operator with VM administration, disk/snapshot access, or an
execution API can still read a conventional running VM. End-to-end transport
encryption does not remove that access; neither does disk encryption when
the same operator can obtain the unlock keys. No claim that Unpeel can
"never access" a future managed workspace follows from these Git settings.
