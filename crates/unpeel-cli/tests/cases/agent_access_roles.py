"""Per-session Agent access roles (sidebar right-click ▸ Agent access).

The role lives in `app-state.json` `mcp_orchestrators[<session id>]` and the
MCP host re-reads it on every call, so a running agent gains or loses it
without a restart. Read (the default) cannot create or close sessions; Write
sends without the approval prompt; Create & remove (`manage`) starts
sessions and removes only the ones it started. Driven through `unpeel mcp`,
which is the same dispatcher an agent's MCP server uses.
"""

import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, run, run_cli, wait_running  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="cat", command="cat")

    ids = []
    for _ in range(2):
        started = run_cli(home, ["new", "--preset", "cat", "--project", "p"])
        ids.extend(word for word in started.stdout.split() if len(word) == 36 and word.count("-") == 4)
    if not case.check("two user sessions start", len(ids) == 2 and all(wait_running(home, i) for i in ids), str(ids)):
        return
    caller, bystander = ids

    def mcp(*words):
        env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1", UNPEEL_SESSION_ID=caller)
        return subprocess.run(
            [BINARY, "mcp", "sessions", *words],
            capture_output=True, text=True, timeout=60, env=env, cwd=CRATES,
        )

    def set_role(role):
        state = home.state()
        grants = state.setdefault("mcp_orchestrators", {})
        if role is None:
            grants.pop(caller, None)
        else:
            grants[caller] = {"role": role, "reach": "project"}
        home.write_state(state)

    def screen(session_id):
        return run_cli(home, ["screen", session_id]).stdout

    # 1. Read (the default) cannot create or close.
    refused = mcp("create", "preset=cat")
    case.check("Read cannot create a session", refused.returncode != 0 and "Create & remove" in refused.stderr, refused.stderr[:200])
    refused = mcp("close", f"session_id={bystander}")
    case.check("Read cannot close a session", refused.returncode != 0 and "Create & remove" in refused.stderr, refused.stderr[:200])

    # A retired lineage-model grant stays inert.
    set_role("write")
    refused = mcp("create", "preset=cat")
    case.check("a stale 'write' grant does not unlock create", refused.returncode != 0, refused.stdout[:200])

    # 2. Create & remove: applies live, stamps the creator.
    set_role("manage")
    created = mcp("create", "preset=cat", "label=helper")
    try:
        result = json.loads(created.stdout)
    except ValueError:
        result = {}
    child = result.get("session_id", "")
    case.check("Create & remove creates a session", created.returncode == 0 and bool(child), (created.stderr or created.stdout)[:200])
    if not child:
        return
    case.check("the created session runs", wait_running(home, child))
    manifest = next((m for m in home.manifests().values() if m.get("session", {}).get("id") == child), {})
    session = manifest.get("session", {})
    case.check("it is stamped with its creator", session.get("spawned_by") == caller, str(session.get("spawned_by")))
    case.check("it lands in the caller's project", session.get("project_id") == "p", str(session.get("project_id")))
    case.check("the label is the requested title", session.get("label") == "helper" and session.get("custom_title") is True, str(session.get("label")))

    # 3. Elevated roles write without the approval prompt (no Controller
    # is running here, so an Ask prompt would fail the call).
    sent = mcp("send_text", f"session_id={child}", "text=from-manager")
    case.check("Create & remove writes without asking", sent.returncode == 0, sent.stderr[:200])
    case.check("the text lands", any("from-manager" in screen(child) for _ in _polls()), screen(child)[-200:])

    # 4. Removal is limited to sessions this caller created.
    refused = mcp("close", f"session_id={bystander}")
    case.check("a user-created session is not removable", refused.returncode != 0 and "not created by this session" in refused.stderr, refused.stderr[:200])
    case.check("the user-created session keeps running", wait_running(home, bystander))
    refused = mcp("close", f"session_id={caller}")
    case.check("a session cannot close itself", refused.returncode != 0, refused.stderr[:200])

    # 5. Downgrading applies to the next call.
    set_role("send")
    refused = mcp("close", f"session_id={child}")
    case.check("Write cannot close", refused.returncode != 0, refused.stderr[:200])
    set_role("manage")
    closed = mcp("close", f"session_id={child}")
    case.check("Create & remove removes its own session", closed.returncode == 0, closed.stderr[:200])
    case.check("the removed session's directory is gone", not os.path.exists(home.path("app-sessions", child)))

    set_role(None)
    for session_id in ids:
        run_cli(home, ["rm", session_id], timeout=30)


def _polls(count=20, delay=0.25):
    for _ in range(count):
        yield
        time.sleep(delay)


run("agent_access_roles", body)
