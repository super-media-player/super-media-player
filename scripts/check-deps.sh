#!/usr/bin/env bash
# Check the Rust dependencies for newer versions and known problems.
#
#   scripts/check-deps.sh            # all direct dependencies
#   SMP_DEP_MIN_AGE=7 scripts/check-deps.sh
#
# For each crate the workspace uses directly, it shows the version in use,
# the newest release at least SMP_DEP_MIN_AGE days old (default 14, the same
# rule as pinned downloads) and the newest one overall. Crates in Cargo.lock
# that changed since the last commit and are younger than that are flagged.
# Then it runs `cargo audit` when installed (`cargo install cargo-audit`).
#
# Updating a dependency bumps the patch version (0.X.Y) in Cargo.toml.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cargo metadata --format-version 1 --locked >"$tmp/meta.json"
git show HEAD:Cargo.lock >"$tmp/old.lock" 2>/dev/null || : >"$tmp/old.lock"

python3 -I - "$tmp/meta.json" "$tmp/old.lock" <<'EOF'
import datetime, json, os, re, sys, time, urllib.request

MIN_AGE = int(os.environ.get("SMP_DEP_MIN_AGE", "14"))
NOW = datetime.datetime.now(datetime.timezone.utc)
UA = "smp-check-deps (github.com/super-media-player/super-media-player)"
meta = json.load(open(sys.argv[1]))

def versions(name, cache={}):
    if name not in cache:
        req = urllib.request.Request(
            f"https://crates.io/api/v1/crates/{name}/versions?per_page=100",
            headers={"User-Agent": UA})
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                data = json.load(r)["versions"]
        except Exception as e:
            print(f"  {name}: could not ask crates.io ({e})", file=sys.stderr)
            data = []
        time.sleep(0.25)  # crates.io asks for at most a few requests a second
        cache[name] = [v for v in data if not v["yanked"] and "-" not in v["num"]]
    return cache[name]

def age(v):
    t = datetime.datetime.fromisoformat(v["created_at"].replace("Z", "+00:00"))
    return (NOW - t).days

def key(num):
    return tuple(int(x) for x in re.findall(r"\d+", num)[:3])

pkgs = {p["id"]: p for p in meta["packages"]}
members = set(meta["workspace_members"])
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

# Direct crates.io dependencies of the workspace, with their locked versions.
direct = {}
for mid in members:
    for d in nodes[mid]["deps"]:
        p = pkgs[d["pkg"]]
        if p["source"] and "crates.io" in p["source"]:
            direct.setdefault(p["name"], set()).add(p["version"])

rows, behind = [], 0
for name in sorted(direct):
    used = max(direct[name], key=key)
    vs = versions(name)
    if not vs:
        continue
    newest = max(vs, key=lambda v: key(v["num"]))
    ripe = [v for v in vs if age(v) >= MIN_AGE]
    best = max(ripe, key=lambda v: key(v["num"]))["num"] if ripe else "-"
    if best != "-" and key(best) > key(used):
        status, behind = "update", behind + 1
    elif key(newest["num"]) > key(used):
        status = f"wait ({MIN_AGE - age(newest)}d)"
    else:
        status = "ok"
    rows.append((name, used, best, f"{newest['num']} ({age(newest)}d)", status))

head = ("crate", "in use", f"newest {MIN_AGE}d+", "newest (age)", "")
w = [max(len(r[i]) for r in rows + [head]) for i in range(5)]
for r in [head] + rows:
    print("  ".join(c.ljust(w[i]) for i, c in enumerate(r)).rstrip())
print(f"\n{behind} direct dependencies can be updated.")

# Lock entries new since the last commit that are younger than MIN_AGE.
old = set(re.findall(r'name = "([^"]+)"\nversion = "([^"]+)"', open(sys.argv[2]).read()))
if old:
    new = {(p["name"], p["version"]) for p in meta["packages"]
           if p["source"] and "crates.io" in p["source"]} - old
    young = []
    for name, ver in sorted(new):
        v = next((v for v in versions(name) if v["num"] == ver), None)
        if v and age(v) < MIN_AGE:
            young.append(f"{name} {ver} ({age(v)}d)")
    if young:
        print(f"\nChanged in Cargo.lock and under {MIN_AGE} days old"
              " (pin older with cargo update -p <crate> --precise <ver>):")
        for y in young:
            print("  " + y)
EOF

echo
if cargo audit --version >/dev/null 2>&1; then
    cargo audit || true
else
    echo "cargo-audit is not installed (cargo install cargo-audit): advisories not checked."
fi
