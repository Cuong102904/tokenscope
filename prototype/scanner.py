"""Incrementally parse Claude Code JSONL transcripts into a local SQLite DB.

Why dedupe matters: Claude Code writes one record per content block, and a resumed
session copies earlier history into a new file, so the same API message appears many
times. We key on (message id, request id) and keep the copy with the most output tokens.
"""
import json
import os
import sqlite3
import threading
from datetime import datetime
from pathlib import Path

DB_PATH = Path(os.environ.get("AGENT_USAGE_DB", Path.home() / ".agent-usage" / "usage.db"))
DEFAULT_ROOTS = [Path.home() / ".claude" / "projects"]
_LOCK = threading.Lock()

SCHEMA = """
CREATE TABLE IF NOT EXISTS messages (
  msg_key TEXT PRIMARY KEY, session_id TEXT, project TEXT, ts INTEGER, day TEXT,
  model TEXT, input INTEGER, output INTEGER, cache_w5 INTEGER, cache_w1 INTEGER,
  cache_r INTEGER, sidechain INTEGER
);
CREATE INDEX IF NOT EXISTS idx_messages_day ON messages(day);
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, size INTEGER, mtime REAL);
"""

UPSERT = """
INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
ON CONFLICT(msg_key) DO UPDATE SET
  session_id=excluded.session_id, project=excluded.project, ts=excluded.ts,
  day=excluded.day, model=excluded.model, input=excluded.input,
  output=excluded.output, cache_w5=excluded.cache_w5, cache_w1=excluded.cache_w1,
  cache_r=excluded.cache_r, sidechain=excluded.sidechain
WHERE excluded.output > messages.output
"""


def connect():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB_PATH, check_same_thread=False)
    conn.executescript(SCHEMA)
    return conn


def _int(v):
    return int(v) if isinstance(v, (int, float)) else 0


def _parse_file(path):
    rows = {}
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            if '"usage"' not in line:  # cheap pre-filter: most lines have no usage
                continue
            try:
                d = json.loads(line)
            except ValueError:
                continue
            if d.get("type") != "assistant":
                continue
            msg = d.get("message") or {}
            u = msg.get("usage")
            model = msg.get("model") or ""
            if not isinstance(u, dict) or not model or model.startswith("<"):
                continue  # skip "<synthetic>" placeholder messages
            key = f"{msg.get('id')}|{d.get('requestId')}"
            if msg.get("id") is None and d.get("requestId") is None:
                key = d.get("uuid") or f"{path}:{len(rows)}"
            cc = u.get("cache_creation") or {}
            w5, w1 = _int(cc.get("ephemeral_5m_input_tokens")), _int(cc.get("ephemeral_1h_input_tokens"))
            if not cc:  # older logs only have the combined number
                w5 = _int(u.get("cache_creation_input_tokens"))
            try:
                dt = datetime.fromisoformat(str(d.get("timestamp")).replace("Z", "+00:00")).astimezone()
            except ValueError:
                continue
            project = Path(d.get("cwd") or "").name or "(unknown)"
            row = (
                key, d.get("sessionId") or Path(path).stem, project, int(dt.timestamp()),
                dt.strftime("%Y-%m-%d"), model, _int(u.get("input_tokens")),
                _int(u.get("output_tokens")), w5, w1, _int(u.get("cache_read_input_tokens")),
                1 if d.get("isSidechain") else 0,
            )
            if key not in rows or row[7] >= rows[key][7]:
                rows[key] = row
    return list(rows.values())


def scan(conn, roots=None, force=False, progress=None):
    """Scan new/changed transcript files. Returns (files_scanned, files_total)."""
    roots = [Path(r).expanduser() for r in (roots or DEFAULT_ROOTS)]
    with _LOCK:
        known = {} if force else {p: (s, m) for p, s, m in conn.execute("SELECT * FROM files")}
        scanned = total = 0
        for root in roots:
            if not root.exists():
                continue
            for path in root.rglob("*.jsonl"):
                total += 1
                try:
                    st = path.stat()
                except OSError:
                    continue
                if known.get(str(path)) == (st.st_size, st.st_mtime):
                    continue
                conn.executemany(UPSERT, _parse_file(path))
                conn.execute("INSERT OR REPLACE INTO files VALUES (?,?,?)", (str(path), st.st_size, st.st_mtime))
                scanned += 1
                if scanned % 25 == 0:
                    conn.commit()
                    if progress:
                        progress(scanned, total)
        conn.commit()
    return scanned, total
