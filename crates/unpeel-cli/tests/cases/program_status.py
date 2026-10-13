"""Program Status Protocol (OSC 7501): any program reports its own state.

A plain program (no runtime, no integration, no hooks) asks the terminal
whether it speaks OSC 7501 with the spec's detection idiom — the query, then
DA1 — and the Host must answer the query first. It then reports working,
blocked, done, and idle; the Host keeps the record and the worker publishes
it with activity_source "program": blocked is attention, done is a finished
turn (one "finished" activity), and an interrupted turn back to idle is not.
Finally the program is SIGKILLed mid-turn without clearing: the Host drops
the departed process's live record.
"""

import json
import os
import re
import shlex
import signal
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli  # noqa: E402
from hook_cancellation import control, fixture_started, read_json  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "programs", home.root)
    environment = {"HOME": home.root, "SHELL": "/bin/bash"}
    os.makedirs(home.path("bin"), exist_ok=True)
    detection = home.path("detection.txt")
    pid_file = home.path("program.pid")
    program = home.path("bin", "reporter")
    with open(program, "w") as handle:
        handle.write(f"#!{sys.executable}\n" + f'''
import base64, os, re, select, tty
tty.setraw(0)
open({pid_file!r}, "w").write(str(os.getpid()))
def report(pairs):
    os.write(1, b"\\x1b]7501;" + pairs.encode() + b"\\x1b\\\\")
def msg(text):
    return base64.b64encode(text.encode()).decode()
# Spec idiom: query, then DA1; the 7501 reply must arrive first.
os.write(1, b"\\x1b]7501;?\\x07\\x1b[c")
replies = b""
while not re.search(rb"\\x1b\\[\\?[0-9;]*c", replies):
    ready, _, _ = select.select([0], [], [], 5)
    if not ready:
        break
    replies += os.read(0, 4096)
first_7501 = replies.find(b"\\x1b]7501;?")
first_da1 = replies.find(b"\\x1b[?")
open({detection!r}, "w").write("supported" if 0 <= first_7501 < first_da1 else "unsupported:" + repr(replies))
print("HOOK_FIXTURE_PROCESS_STARTED", flush=True)
report("state=idle:app=reporter")
while True:
    data = os.read(0, 4096)
    if not data or data == b"q":
        break
    if data == b"w":
        report("state=working:app=reporter:progress=40:msg=" + msg("Compiling 3 crates"))
    elif data == b"b":
        report("state=blocked:kind=permission:app=reporter:msg=" + msg("Approve deploy?"))
    elif data == b"d":
        report("state=done:app=reporter:msg=" + msg("Built in 2s"))
    elif data == b"i":
        report("state=idle:app=reporter")
''')
    os.chmod(program, 0o755)
    home.preset(label="reporter", command=shlex.quote(program), preset_id="reporter")
    service = case.serve(env=environment)
    if not case.check("isolated Host starts", bool(service.ready(timeout=25)), service.log()):
        return
    launched = run_cli(home, ["new", "--preset", "reporter", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    if not case.check("the reporter launches", launched.returncode == 0 and bool(ids), launched.stderr):
        return
    sid = ids[0]
    session_dir = home.path("app-sessions", sid)
    if not fixture_started(case, service, session_dir):
        return

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(sid, {})

    def program_status():
        return read_json(os.path.join(session_dir, "manifest.json")).get("program_status") or {}

    def finished_count():
        count = 0
        try:
            with open(home.path("activity-log.jsonl")) as handle:
                for line in handle:
                    entry = json.loads(line)
                    if entry.get("session_id") == sid and entry.get("kind") in ("finished", "Finished"):
                        count += 1
        except (OSError, ValueError):
            pass
        return count

    def published(raw_status):
        return lambda: activity().get("raw_status") == raw_status \
            and activity().get("activity_source") == "program"

    try:
        with open(detection) as handle:
            answer = handle.read()
        case.check("the Host answers OSC 7501 detection before DA1", answer == "supported", answer)
        case.check("the initial idle report is published as the program source",
                   bool(service.wait_for(published("idle"), timeout=15)), str(activity()))

        control(home, sid, "w")
        case.check("working lands in the manifest with its decoded message",
                   bool(service.wait_for(lambda: program_status().get("state") == "working", timeout=10))
                   and program_status().get("msg") == "Compiling 3 crates"
                   and program_status().get("progress") == 40
                   and program_status().get("app") == "reporter", str(program_status()))
        case.check("the worker reports busy from the program",
                   bool(service.wait_for(published("busy"), timeout=10)), str(activity()))

        control(home, sid, "b")
        case.check("blocked is attention",
                   bool(service.wait_for(published("attention"), timeout=10))
                   and program_status().get("kind") == "permission", str(activity()))

        control(home, sid, "d")
        case.check("done is idle and a completed turn",
                   bool(service.wait_for(lambda: published("idle")() and activity().get("completed") is True,
                                         timeout=10)), str(activity()))
        case.check("done records one finished activity",
                   bool(service.wait_for(lambda: finished_count() == 1, timeout=10)), str(finished_count()))

        control(home, sid, "w")
        service.wait_for(published("busy"), timeout=10)
        control(home, sid, "i")
        case.check("an interrupted turn returns to idle",
                   bool(service.wait_for(published("idle"), timeout=10)), str(activity()))
        service.read_for(1.5)
        case.check("an interrupted turn is not a finished one", finished_count() == 1, str(finished_count()))

        control(home, sid, "w")
        case.check("a new turn is busy again",
                   bool(service.wait_for(published("busy"), timeout=10)), str(activity()))
        with open(pid_file) as handle:
            os.kill(int(handle.read().strip()), signal.SIGKILL)
        case.check("a SIGKILLed reporter's live record is dropped",
                   bool(service.wait_for(lambda: not program_status(), timeout=15)), str(program_status()))
        case.check("the worker stops publishing the program source",
                   bool(service.wait_for(lambda: activity().get("activity_source") != "program", timeout=10)),
                   str(activity()))
    finally:
        try:
            control(home, sid, "q")
        except OSError:
            pass
        run_cli(home, ["rm", sid], env=environment, timeout=45)


run("program_status", body)
