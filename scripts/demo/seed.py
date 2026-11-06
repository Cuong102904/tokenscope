#!/usr/bin/env python3
"""Fill a tokenscope database with made-up usage for screenshots and demos.

Usage: seed.py <path to tokenscope.db>

The database must already exist (run `tokenscope collect` with the demo HOME first,
which creates the schema). Everything here is invented: project names, sessions,
accounts, costs and limits. Output is deterministic for a given day.
"""

import random
import sqlite3
import sys
import time
from datetime import datetime, timedelta

DAYS = 45
NOW = int(time.time())
rnd = random.Random(7)

PROJECTS = [
    "/home/dev/code/acme-web",
    "/home/dev/code/billing-api",
    "/home/dev/code/mobile-app",
    "/home/dev/code/data-pipeline",
    "/home/dev/code/docs-site",
    "/home/dev/code/infra",
]
TITLES = [
    "Add Stripe webhook retries", "Fix flaky login test", "Migrate charts to the new API",
    "Refactor invoice PDF export", "Speed up nightly ETL job", "Dark mode for settings page",
    "Write onboarding guide", "Terraform module for the queue", "Paginate the orders endpoint",
    "Upgrade React to the latest major", "Investigate memory spike", "Add rate limiting middleware",
]
# vendor -> [(model, weight)], tokens are per message: (input, output, cache_read, cache_write)
VENDORS = {
    "claude": ([("claude-opus-5-5", 6), ("claude-sonnet-5-5", 3), ("claude-haiku-5-5", 1)],
               (3000, 900, 60000, 4000)),
    "codex": ([("gpt-6-astra", 3), ("gpt-6.1-sol", 2), ("gpt-5.4-mini", 1)],
              (9000, 1500, 30000, 0)),
    "opencode": ([("gpt-5.4-mini", 1)], (6000, 800, 10000, 0)),
}
CLAUDE_SKILLS = [
    ("superpowers:brainstorming", "superpowers"), ("superpowers:test-driven-development", "superpowers"),
    ("superpowers:writing-plans", "superpowers"), ("frontend-design", None),
    ("docs:release-notes", "docs"), ("docs:api-reference", "docs"),
]
CODEX_SKILLS = ["migrations", "release-checklist", "sql-style"]
TOOLS = {
    "claude": [("Read", 30), ("Edit", 18), ("Bash", 22), ("Grep", 12), ("Write", 6), ("TodoWrite", 5), ("WebFetch", 2)],
    "codex": [("exec_command", 25), ("apply_patch", 10), ("read_file", 8)],
    "opencode": [("bash", 5), ("edit", 3)],
}


def pick(pairs):
    return rnd.choices([p for p, _ in pairs], weights=[w for _, w in pairs])[0]


def jitter(n, spread=0.6):
    return max(1, int(n * rnd.uniform(1 - spread, 1 + spread)))


def local_day(ts):
    return datetime.fromtimestamp(ts).strftime("%Y-%m-%d")


def main(db):
    c = sqlite3.connect(db)
    for t in ("usage", "sessions", "skill_events", "tool_events", "limit_snapshots", "limit_events"):
        c.execute(f"DELETE FROM {t}")
    n_msg = 0
    sid = 0
    today0 = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)

    def add_session(vendor, start, msgs, project, active=False):
        nonlocal n_msg, sid
        sid += 1
        session = f"demo-{vendor}-{sid:04d}"
        models, (inp, out, cr, cw) = VENDORS[vendor]
        model = pick(models)
        ts = start
        skill, plugin = (rnd.choice(CLAUDE_SKILLS) if vendor == "claude" and rnd.random() < 0.55 else (None, None))
        if skill:
            c.execute("INSERT INTO skill_events VALUES(?,?,?,?,?,?)",
                      (vendor, session, start, skill, "invoked", f"{session}:skill"))
        if vendor == "codex" and rnd.random() < 0.4:
            c.execute("INSERT INTO skill_events VALUES(?,?,?,?,?,?)",
                      (vendor, session, start, rnd.choice(CODEX_SKILLS), "file_read", f"{session}:skill"))
        for i in range(msgs):
            ts += rnd.randint(20, 150)
            if not active and ts > NOW - 600:
                break
            ts = min(ts, NOW - 30)
            sub = vendor == "claude" and rnd.random() < 0.15
            c.execute(
                "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,"
                "cache_read,cache_write_5m,cache_write_1h,sidechain,skill,plugin) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (f"m{n_msg}", vendor, "", session, project, ts, local_day(ts), model,
                 jitter(inp), jitter(out), 0, jitter(cr), jitter(cw) if cw else 0, 0, int(sub),
                 skill if i < msgs * 0.6 else None, plugin if i < msgs * 0.6 else None))
            n_msg += 1
            if rnd.random() < 0.7:
                c.execute("INSERT INTO tool_events VALUES(?,?,?,?,?)",
                          (vendor, session, ts, pick(TOOLS[vendor]), f"t{n_msg}"))
        c.execute("INSERT INTO sessions VALUES(?,?,?,?,?,?)",
                  (vendor, session, project, rnd.choice(TITLES), f"/demo/{session}.jsonl", ts))

    for back in range(DAYS, -1, -1):
        day0 = today0 - timedelta(days=back)
        weekend = day0.weekday() >= 5
        busy = 0.35 if weekend else 1.0
        # a quieter stretch two weeks ago, a busier one recently
        if 12 <= back <= 16:
            busy *= 0.4
        if back <= 6:
            busy *= 1.3
        for vendor, per_day in (("claude", 6), ("codex", 3), ("opencode", 0.4)):
            for _ in range(int(per_day * busy + rnd.random())):
                start = int((day0 + timedelta(hours=rnd.uniform(9, 19))).timestamp())
                if start > NOW - 900:
                    continue
                add_session(vendor, start, jitter(45, 0.7), rnd.choice(PROJECTS))
    # sessions still running right now
    add_session("claude", NOW - 2400, 40, PROJECTS[0], active=True)
    add_session("claude", NOW - 1500, 25, PROJECTS[1], active=True)
    add_session("codex", NOW - 900, 12, PROJECTS[3], active=True)

    # rate limits: Claude rising towards its 5-hour limit (enough points for an ETA),
    # one Codex account out of weekly quota, the other fine, agy with two model groups
    snap = "INSERT INTO limit_snapshots VALUES(?,?,?,?,?,?,?,?,?)"
    five_reset = NOW + 2 * 3600 + 10 * 60
    for i, used in enumerate([31, 34, 37, 40, 42]):
        at = NOW - 3000 + i * 690
        c.execute(snap, ("claude", "work", "five_hour", 300, used, five_reset, None, at, "claude /usage"))
    for scope, used, reset in (("weekly", 35, NOW + 4 * 86400 + 5 * 3600), ("weekly:Fable", 12, NOW + 4 * 86400 + 5 * 3600)):
        c.execute(snap, ("claude", "work", scope, 10080, used, reset, None, NOW - 240, "claude /usage"))
    for acct, plan, five, week in (("a1b2c3d4", "plus", 68, 100), ("e5f6a7b8", "plus", 12, 41)):
        c.execute(snap, ("codex", acct, "five_hour", 300, five, NOW + 3 * 3600, plan, NOW - 180, "codex app-server"))
        c.execute(snap, ("codex", acct, "weekly", 10080, week, NOW + 3 * 86400 + 7200, plan, NOW - 180, "codex app-server"))
    for group, used in (("Gemini Models", 22), ("Claude and GPT models", 8)):
        c.execute(snap, ("agy", "9c8d7e6f", f"weekly:{group}", 10080, used, NOW + 5 * 86400, None, NOW - 600, "agy /usage"))
    for back, scope, acct, vendor in ((2, "weekly", "a1b2c3d4", "codex"), (9, "five_hour", "work", "claude"),
                                      (16, "five_hour", "work", "claude"), (23, "weekly", "e5f6a7b8", "codex")):
        ts = NOW - back * 86400
        c.execute("INSERT INTO limit_events VALUES(?,?,?,?,?)", (vendor, acct, ts, scope, ts + 3 * 3600))
    c.commit()
    print(f"seeded {n_msg} messages in {sid} sessions")


if __name__ == "__main__":
    main(sys.argv[1])
