"""Tiny local web server: serves static/index.html and a JSON API. Stdlib only."""
import json
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import report
import scanner

STATIC = Path(__file__).parent / "static"
_omni_cache = {"at": 0.0, "data": None}
_omni_lock = threading.Lock()


def omnigent_usage():
    """Optional panel: `omnigent usage --json` (includes non-Claude models such as GPT). Cached 60s."""
    with _omni_lock:
        if time.time() - _omni_cache["at"] < 60:
            return _omni_cache["data"]
        data = None
        try:
            out = subprocess.run(["omnigent", "usage", "--json", "--limit", "50"],
                                 capture_output=True, text=True, timeout=25)
            if out.returncode == 0:
                data = json.loads(out.stdout)
        except (OSError, ValueError, subprocess.TimeoutExpired):
            data = None
        _omni_cache.update(at=time.time(), data=data)
        return data


def make_handler(conn):
    class Handler(BaseHTTPRequestHandler):
        def _send(self, code, body, ctype):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            url = urlparse(self.path)
            if url.path == "/api/data":
                q = parse_qs(url.query)
                days = int(q.get("days", ["30"])[0] or 0) or None
                model = q.get("model", [""])[0] or None
                project = q.get("project", [""])[0] or None
                scanner.scan(conn)  # picks up new activity on every refresh
                data = report.build(conn, days, model, project)
                data["omnigent"] = omnigent_usage()
                self._send(200, json.dumps(data).encode(), "application/json")
            elif url.path in ("/", "/index.html"):
                self._send(200, (STATIC / "index.html").read_bytes(), "text/html; charset=utf-8")
            else:
                self._send(404, b"not found", "text/plain")

        def log_message(self, *args):  # keep the terminal quiet
            pass

    return Handler


def serve(conn, host="127.0.0.1", port=8765):
    server = ThreadingHTTPServer((host, port), make_handler(conn))
    print(f"Dashboard: http://{host}:{port}  (Ctrl-C to stop)")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
