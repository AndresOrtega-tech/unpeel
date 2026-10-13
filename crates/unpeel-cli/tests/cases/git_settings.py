"""Git setup over the real Host contract, with private Git config and a fake gh.

No provider login, credential store, browser, or real user configuration is used.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import mobile_request, plaintext_mobile_request, run  # noqa: E402


FAKE_GH = r'''
import json, os, sys, time
from pathlib import Path
root = Path(os.environ["UNPEEL_GIT_FIXTURE"])
args = sys.argv[1:]
with (root / "calls").open("a") as out:
    out.write(json.dumps(args) + "\n")
if args[:2] == ["auth", "status"]:
    if (root / "old-gh").exists():
        print("unknown flag: --json", file=sys.stderr)
        sys.exit(1)
    accounts = []
    if (root / "connected").exists():
        source = "/private/NEVER-EXPOSE-THIS-PATH/hosts.yml" if (root / "storage-file").exists() else "keyring"
        accounts = [{"active": True, "login": "fixture-user", "state": "success", "tokenSource": source, "token": "NEVER-EXPOSE-THIS-TOKEN"}]
    print(json.dumps({"hosts": {"github.com": accounts}}))
elif args[:2] == ["auth", "login"]:
    assert "--web" in args and "https" in args
    assert not sys.stdin.isatty()
    if (root / "fail-login").exists():
        print("NEVER-EXPOSE-THIS-ERROR-TOKEN", file=sys.stderr, flush=True)
        sys.exit(1)
    (root / "login-pid").write_text(str(os.getpid()))
    print("! First copy your one-time code: ABCD-1234", file=sys.stderr, flush=True)
    print("Open this URL to continue in your web browser: https://evil.example/", file=sys.stderr, flush=True)
    while not (root / "approve").exists():
        time.sleep(.05)
    (root / "connected").write_text("yes")
elif args[:2] == ["auth", "setup-git"]:
    import subprocess
    subprocess.run(["git", "config", "--global", "credential.https://github.com.helper", "!gh auth git-credential"], check=True)
elif args[:2] == ["auth", "logout"]:
    assert args[-2:] == ["--user", "fixture-user"]
    (root / "connected").unlink(missing_ok=True)
elif args[:1] == ["api"]:
    if not (root / "connected").exists():
        print("To get started with GitHub CLI, please run: gh auth login", file=sys.stderr)
        sys.exit(1)
    if (root / "network-error").exists():
        print("network failure", file=sys.stderr)
        sys.exit(1)
    print(json.dumps({"login": "fixture-user"}))
else:
    print("gh version fixture")
'''


def body(case):
    home = case.home
    fixture = Path(home.path("git-fixture"))
    fixture.mkdir()
    bin_dir = fixture / "bin"
    bin_dir.mkdir()
    gh = bin_dir / "gh"
    gh.write_text("#!" + sys.executable + "\n" + FAKE_GH)
    gh.chmod(0o755)
    # Warm this newly-created executable before measuring bounded Host
    # commands; macOS can hold a first execution in its loader for seconds.
    subprocess.run([str(gh), "--version"], env=dict(os.environ, UNPEEL_GIT_FIXTURE=str(fixture)),
                   check=True, capture_output=True, timeout=60)
    (fixture / "calls").unlink()
    token = home.pair_device()
    port = home.reserve_mobile_port()
    service = case.serve(env={
        "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
        "UNPEEL_GIT_FIXTURE": str(fixture),
        "GH_CONFIG_DIR": str(fixture / "gh-config"),
        "GH_TOKEN": "", "GITHUB_TOKEN": "",
        "GIT_CONFIG_GLOBAL": str(fixture / "gitconfig"),
        "GIT_CONFIG_NOSYSTEM": "1",
    })
    ready = service.ready()
    case.check("Git fixture Host starts", bool(ready), service.log())
    if not ready:
        return

    rejected_action = {"action": "connectGithub", "requestID": str(uuid.uuid4())}
    case.check("unpaired Controller cannot read Git settings", mobile_request(port, "/mobile/git-settings", "invalid")[0] == 401)
    case.check("unpaired Controller cannot start GitHub login", mobile_request(port, "/mobile/git-settings", "invalid", method="POST", body=rejected_action)[0] == 401)
    case.check("plaintext requests cannot read Git settings", plaintext_mobile_request(port, "/mobile/git-settings", token)[0] == 426)
    case.check("plaintext requests cannot start GitHub login", plaintext_mobile_request(port, "/mobile/git-settings", token, method="POST", body=rejected_action)[0] == 426)
    case.check("rejected requests execute no GitHub CLI command", not (fixture / "calls").exists())

    def read():
        status, value = mobile_request(port, "/mobile/git-settings", token)
        assert status == 200, value
        assert "NEVER-EXPOSE" not in json.dumps(value), value
        return value

    def write(action, **fields):
        fields.setdefault("requestID", str(uuid.uuid4()))
        return mobile_request(port, "/mobile/git-settings", token, method="POST", body={"action": action, **fields})

    def settled():
        value = read()
        op = value.get("operation") or {}
        return value if not value["checking"] and op.get("state") not in ("running", "awaitingAuthorization") else None

    def challenge():
        value = read()
        return value if (value.get("operation") or {}).get("state") == "awaitingAuthorization" else None

    status, boot = mobile_request(port, "/mobile/bootstrap", token)
    caps = boot.get("hostProtocol", {}).get("capabilities", [])
    case.check("all Hosts advertise Git read and write", status == 200 and all(cap in caps for cap in ["settings.git.read", "settings.git.set"]))
    # The first inventory runs several independently bounded probes. Allow
    # their combined budget, including cold Git startup on macOS.
    initial = service.wait_for(settled, timeout=75)
    case.check("Git inventory reflects the Host", bool(initial) and initial["githubCliInstalled"] and initial["github"]["state"] == "notConnected", str(initial))
    if not initial:
        return

    request_id = str(uuid.uuid4())
    status, _ = write("connectGithub", requestID=request_id)
    waiting = service.wait_for(challenge)
    case.check("device challenge reaches Controller without opening Host browser", status == 200 and bool(waiting) and waiting["operation"]["userCode"] == "ABCD-1234" and waiting["operation"]["verificationUrl"] == "https://github.com/login/device", str(waiting))
    case.check("duplicate login is idempotent", write("connectGithub", requestID=request_id)[0] == 200)
    case.check("concurrent mutation cannot replace login", write("setIdentity", name="Other", email="other@example.com")[0] == 409)
    write("cancelGithub", operationID=str(uuid.uuid4()))
    case.check("stale cancellation leaves current login alone", (read().get("operation") or {}).get("state") == "awaitingAuthorization")
    (fixture / "approve").write_text("yes")
    connected = service.wait_for(settled)
    case.check("login is verified and configures Git on the Host", bool(connected) and connected["github"]["state"] == "connected" and connected["github"]["gitConfigured"] and connected["operation"]["state"] == "complete", str(connected))
    if not connected:
        return
    calls = [json.loads(line) for line in (fixture / "calls").read_text().splitlines()]
    case.check("retry creates exactly one login process", sum(call[:2] == ["auth", "login"] for call in calls) == 1)
    case.check("system credential store is reported", connected["github"]["credentialStorage"] == "keyring")
    (fixture / "storage-file").touch()
    write("refresh")
    stored = service.wait_for(settled)
    case.check("plain-text storage is reported without exposing its path", bool(stored) and stored["github"]["credentialStorage"] == "file", str(stored))
    (fixture / "storage-file").unlink()

    name = "Test $(touch should-not-exist)"
    write("setIdentity", name=name, email="test@example.com")
    identity = service.wait_for(settled)
    case.check("identity is saved literally in private Host config", bool(identity) and identity["name"] == name and identity["email"] == "test@example.com", str(identity))
    write("disconnectGithub", login="fixture-user")
    disconnected = service.wait_for(settled)
    case.check("disconnect removes only the named Host account", bool(disconnected) and disconnected["github"]["state"] == "notConnected", str(disconnected))

    (fixture / "old-gh").touch()
    write("refresh")
    old_cli = service.wait_for(settled)
    case.check("older distribution gh detects missing login", bool(old_cli) and old_cli["github"]["state"] == "notConnected", str(old_cli))
    (fixture / "connected").touch()
    write("refresh")
    old_cli = service.wait_for(settled)
    case.check("older gh verifies account through the API", bool(old_cli) and old_cli["github"]["login"] == "fixture-user" and old_cli["github"]["state"] == "connected", str(old_cli))
    (fixture / "network-error").touch()
    write("refresh")
    network = service.wait_for(settled)
    case.check("network failures are not reported as signed out", bool(network) and network["github"]["state"] == "error", str(network))
    (fixture / "network-error").unlink()
    (fixture / "connected").unlink()

    (fixture / "approve").unlink()
    cancel_id = str(uuid.uuid4())
    write("connectGithub", requestID=cancel_id)
    service.wait_for(challenge)
    write("cancelGithub", operationID=cancel_id)
    cancelled = service.wait_for(settled)
    case.check("cancel stops the owned login and removes the challenge", bool(cancelled) and cancelled["operation"]["state"] == "cancelled" and cancelled["operation"]["userCode"] is None and cancelled["github"]["state"] == "notConnected", str(cancelled))

    (fixture / "fail-login").touch()
    write("connectGithub")
    failed = service.wait_for(settled)
    case.check("failed sign-in returns only a sanitized error", bool(failed) and failed["operation"]["state"] == "failed" and failed["operation"]["userCode"] is None, str(failed))
    case.check("Host logs contain no credential output or device code", "NEVER-EXPOSE" not in service.log() and "ABCD-1234" not in service.log())


run("git_settings", body)
