#!/usr/bin/env python3
"""agent-usage: local cost/token dashboard for Claude Code (+ optional Omnigent panel).

  python cli.py scan [--force]        parse transcripts into the local DB
  python cli.py today | week | stats  terminal summaries
  python cli.py dashboard [--port N]  local web dashboard (default 127.0.0.1:8765)
"""
import argparse
import sys

import dashboard
import report
import scanner


def _money(v):
    return f"${v:,.2f}"


def _scan(conn, force=False):
    def prog(n, total):
        print(f"\r  scanned {n} files (of {total} seen)...", end="", file=sys.stderr, flush=True)
    n, total = scanner.scan(conn, force=force, progress=prog)
    print(f"\rScan done: {n} new/changed of {total} files.        ", file=sys.stderr)


def _print(data, title):
    t = data["totals"]
    print(f"\n{title}")
    print(f"  Estimated cost : {_money(t['cost'])}   (API-equivalent, not your invoice)")
    print(f"  Sessions / msgs: {t['sessions']} / {t['messages']}")
    print(f"  Tokens         : in {t['input']:,} | out {t['output']:,} | cache read {t['cache_read']:,} | cache write {t['cache_write']:,}")
    if data["models"]:
        print("\n  By model")
        for m in data["models"]:
            cost = _money(m["cost"]) if m["priced"] else "n/a"
            print(f"    {m['model']:<28} {cost:>10}   out {m['output']:>12,}")
    if data["projects"]:
        print("\n  Top projects")
        for p in data["projects"][:8]:
            print(f"    {p['project'][:28]:<28} {_money(p['cost']):>10}   {p['sessions']} sessions")
    if data["unpriced"]:
        print(f"\n  No price entry for: {', '.join(data['unpriced'])} (edit pricing.py)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("scan"); s.add_argument("--force", action="store_true")
    sub.add_parser("today"); sub.add_parser("week"); sub.add_parser("stats")
    d = sub.add_parser("dashboard"); d.add_argument("--host", default="127.0.0.1"); d.add_argument("--port", type=int, default=8765)
    a = ap.parse_args()

    conn = scanner.connect()
    _scan(conn, getattr(a, "force", False))
    if a.cmd == "today":
        _print(report.build(conn, days=1), "Today")
    elif a.cmd == "week":
        _print(report.build(conn, days=7), "Last 7 days")
    elif a.cmd == "stats":
        _print(report.build(conn), "All time")
    elif a.cmd == "dashboard":
        dashboard.serve(conn, a.host, a.port)


if __name__ == "__main__":
    main()
