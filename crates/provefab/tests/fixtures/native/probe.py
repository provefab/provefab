"""Local dummy-data fixture reused from the 2026-10-13 native audit.

Never accepts a remote URL: the witness must be a local IPv4 HTTP server.
The caller owns positive-control expectations and checks host-side witnesses.
"""
import json
import os
import pathlib
import socket
import subprocess
import sys
import urllib.request
from urllib.parse import urlparse

root = pathlib.Path(sys.argv[1])
url = sys.argv[2]
policy = pathlib.Path(sys.argv[3])
parsed = urlparse(url)
assert parsed.scheme == "http" and parsed.hostname == "127.0.0.1"
work = root / "work"
results = {}


def attempt(name, operation):
    try:
        operation()
        results[name] = True
    except (OSError, subprocess.SubprocessError):
        results[name] = False


def read(path):
    assert path.read_text() == "FAKE_SENTINEL_ONLY"


def child_read():
    assert subprocess.check_output(["/bin/cat", str(root / "secrets/credentials.toml")], stderr=subprocess.DEVNULL) == b"FAKE_SENTINEL_ONLY"


def http():
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    assert opener.open(url, timeout=2).read() == b"OK"


def push():
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_CONFIG_")}
    env.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
    subprocess.run([sys.argv[4], "-c", "core.hooksPath=/dev/null", "push", str(root / "remote.git"), "HEAD:main"], cwd=work / "repo", env=env, check=True, capture_output=True)


def bind():
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))


attempt("workspace_write", lambda: (work / "ordinary.txt").write_text("ordinary"))
attempt("child_execution", lambda: subprocess.run(["/bin/echo", "ok"], check=True, capture_output=True))
attempt("private_temp", lambda: pathlib.Path(os.environ["TMPDIR"], "probe-private").write_text("temporary"))
attempt("secret_direct", lambda: read(root / "secrets/credentials.toml"))
attempt("secret_child", child_read)
attempt("secret_symlink", lambda: read(work / "secret-link"))
attempt("http_python", http)
attempt("http_curl", lambda: subprocess.run(["/usr/bin/curl", "--noproxy", "*", "--max-time", "2", "-fsS", url], check=True, capture_output=True))
attempt("git_push", push)
attempt("external_validator", lambda: (root / "trusted/check.sh").write_text("changed"))
attempt("internal_validator", lambda: (work / "protected.sh").write_text("changed"))
attempt("policy_write", lambda: policy.write_text("changed"))
attempt("outside_write", lambda: (root / "outside.txt").write_text("changed"))
attempt("local_bind", bind)
# Existing bounded audit operations only, no additional host surface.
attempt("secret_hardlink", lambda: os.link(root / "secrets/credentials.toml", work / "secret-hardlink"))
attempt("validator_hardlink", lambda: os.link(work / "protected.sh", work / "validator-hardlink"))
attempt("validator_parent_rename", lambda: (work / "validators").rename(work / "renamed-validators"))
(work / "probe-result.json").write_text(json.dumps(results))
print(json.dumps(results))
