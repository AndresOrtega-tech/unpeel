"""Version skew between the CLI/Host and the desktop app.

Users update the two independently — a newer `unpeel` (CLI + `unpeel serve`)
will meet an older app that has never heard of the state-bus ping or of
routes added since. Every verb must complete on the shared files and never
block on, or be broken by, an app that 404s what it does not know."""

import json
import sys, os

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.session("s-one", label="first session", project_id="p",
                 created_at=1_754_400_000_000, settled=True, running=True)
    case.host("s-one")
    home.session("s-two", label="second session", project_id="p",
                 created_at=1_754_300_000_000, settled=True)
    home.seed_resume_data("s-one", "s-two")

    # An older app: it answers the routes it shipped with and 404s the rest,
    # including the cross-frontend ping this version sends after every write.
    old_app = case.app(
        fail_routes=(
            "/state-changed",
            "/mcp/sidebar",
            "/mcp/mark-read",
        )
    )

    listed = run_cli(home, ["ls"])
    case.check(
        "listing reads the shared files, never the app",
        listed.returncode == 0
        and "first session" in listed.stdout
        and "second session" in listed.stdout
        and old_app.count("/mcp/sidebar") == 0,
        listed.stdout[:200],
    )

    stopped = run_cli(home, ["stop", "s-one"], timeout=45)
    output_path = home.path("app-sessions", "s-one", "output.bin")
    case.check(
        "stop completes on the shared files even though the app 404s the ping",
        stopped.returncode == 0
        and home.manifests().get("s-one", {}).get("state") == "exited"
        and os.path.exists(output_path)
        and os.path.getsize(output_path) == 0
        and not home.has_marker("s-one", "archived.json"),
        stopped.stderr[:200] + str(home.manifests().get("s-one", {}).get("state")),
    )
    case.check(
        "the older app was still told (and its 404 was harmless)",
        old_app.count("/state-changed") > 0,
        str(old_app.calls)[:200],
    )

    # The Host service beside an older app: it must come up, publish its
    # status, and keep serving with the app 404ing every ping.
    service = case.serve()
    ready = service.ready()
    case.check("the Host starts beside an older app", bool(ready), service.log()[-300:])
    home.marker("s-two", "title.json", {"title": "renamed while an old app runs"})
    run_cli(home, ["presets", "add", "skew", "echo skew"], expect_ok=True)
    run_cli(home, ["presets", "remove", "skew"], expect_ok=True)
    service.read_for(1.0)
    alive = not service.exited(timeout=0.2)
    case.check("the Host keeps running through the app's 404s", alive, service.log()[-300:])
    service.close()
    state = home.state()
    case.check(
        "shared state is intact after the skewed session",
        [p["id"] for p in state.get("projects", [])] == ["p"]
        and all(p.get("label") != "skew" for p in state.get("presets", [])),
        str(state)[:300],
    )


run("compat_bridge", body)
