#!/usr/bin/env bash
# ON the build VM: publishes this version's Linux and Windows packages
# (dist/, with their SHA256SUMS) as the GitHub release of its version tag.
# The macOS .dmg files are added to the same release by GitHub Actions
# (.github/workflows/release.yml), which runs when the tag is pushed.
#
# Usage: publish-github.sh           publish the packages of Cargo.toml's version
#        publish-github.sh --check   only check the token; prints the days
#                                    left before it expires
#
# Needs github.conf in the project folder (git-ignored, never committed):
#   GITHUB_TOKEN=github_pat_...   fine-grained token for this repository only,
#                                 "Contents: Read and write", nothing else
#   GITHUB_REPO=owner/repo        the repository
# The tag (v0.59.0) must be on GitHub and name the commit that was built. The
# release is filled as a draft and published at the end, so nobody sees a
# half-uploaded one. Running it again finishes or updates the release.
# Exit codes: 0 done, 3 no github.conf, 4 the commit has no version tag,
# others failed.
set -euo pipefail
umask 077
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
conf="$root/github.conf"
[[ -f "$conf" ]] || { echo "github.conf not found; not publishing to GitHub" >&2; exit 3; }
GITHUB_TOKEN="" GITHUB_REPO=""
# shellcheck source=/dev/null
source "$conf"
[[ -n "$GITHUB_TOKEN" ]] || { echo "ERROR: GITHUB_TOKEN is empty in github.conf" >&2; exit 1; }
[[ -n "$GITHUB_REPO" ]] || { echo "ERROR: GITHUB_REPO is empty in github.conf" >&2; exit 1; }
api="https://api.github.com/repos/$GITHUB_REPO"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# The token goes in a header file, so it never shows in the process list.
printf 'Authorization: Bearer %s\nX-GitHub-Api-Version: 2022-11-28\nAccept: application/vnd.github+json\n' \
    "$GITHUB_TOKEN" >"$work/headers"

# call <method> <url> [curl options...]: prints the body, fails on an HTTP
# error. Response headers go to $work/response-headers.
call() {
    local method="$1" url="$2"
    shift 2
    curl --silent --show-error --fail-with-body --location --retry 3 \
        -H @"$work/headers" -D "$work/response-headers" -X "$method" "$@" "$url"
}

if [[ "${1:-}" == --check ]]; then
    call GET "$api" >/dev/null || { echo "ERROR: GitHub refused the token" >&2; exit 1; }
    expiry=$(tr -d '\r' <"$work/response-headers" |
        sed -n 's/^github-authentication-token-expiration: //Ip' | head -n 1)
    if [[ -z "$expiry" ]]; then echo never
    else echo $(( ($(date -d "$expiry" +%s) - $(date +%s)) / 86400 )); fi
    exit 0
fi

version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
tag="v$version"
commit=$(git rev-parse HEAD)
git fetch --quiet --tags origin
tagged=$(git rev-parse -q --verify "refs/tags/$tag^{commit}" || true)
if [[ "$tagged" != "$commit" ]]; then
    echo "No $tag tag on the commit that was built ($(git rev-parse --short HEAD)); not publishing" >&2
    exit 4
fi
if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    echo "ERROR: the folder has changes that are not in $tag (copied over?); not publishing" >&2
    exit 1
fi

sums="dist/SHA256SUMS-$version"
[[ -f "$sums" ]] || { echo "ERROR: $sums missing: build first" >&2; exit 1; }
mapfile -t assets < <(sed 's/^[0-9a-f]*  /dist\//' "$sums")
assets+=("$sums")
for f in "${assets[@]}"; do [[ -f "$f" ]] || { echo "ERROR: $f missing" >&2; exit 1; }; done
(cd dist && sha256sum -c --quiet "SHA256SUMS-$version") || { echo "ERROR: checksums do not match" >&2; exit 1; }

echo "==> release $tag"
# Drafts have no tag yet, so look through the list (GitHub Actions may have
# made the release already, for the macOS files).
call GET "$api/releases?per_page=100" >"$work/releases.json"
jq --arg tag "$tag" '[.[] | select(.tag_name == $tag)][0] // empty' "$work/releases.json" >"$work/release.json"
if [[ ! -s "$work/release.json" ]]; then
    jq -n --arg tag "$tag" --arg commit "$commit" \
        '{tag_name: $tag, target_commitish: $commit, name: ("Super Media Player " + ($tag | ltrimstr("v"))), draft: true}' |
        call POST "$api/releases" --data-binary @- >"$work/release.json"
fi
id=$(jq -r .id "$work/release.json")
upload="https://uploads.github.com/repos/$GITHUB_REPO/releases/$id/assets"

for asset in "${assets[@]}"; do
    name=$(basename "$asset")
    size=$(stat -c %s "$asset")
    if jq -e --arg name "$name" --argjson size "$size" \
        '.assets[] | select(.name == $name and .state == "uploaded" and .size == $size)' \
        "$work/release.json" >/dev/null; then
        echo "    $name already uploaded"
        continue
    fi
    # A broken or older upload of the same name goes first.
    jq -r --arg name "$name" '.assets[] | select(.name == $name) | .id' "$work/release.json" |
        while IFS= read -r old; do call DELETE "$api/releases/assets/$old" >/dev/null; done
    echo "==> uploading $name ($(du -h "$asset" | cut -f1))"
    call POST "$upload?name=$name" -H "Content-Type: application/octet-stream" \
        --data-binary @"$asset" >/dev/null
done

pkg() { sed -n "s/^[0-9a-f]*  \($1\)$/\1/p" "$sums" | head -n 1; }
cat >"$work/body.md" <<EOF
Super Media Player $version. Each file below is the whole player: the engine,
disc playback and ripping, and the converters are inside.

| System | Download | Install |
|---|---|---|
| Ubuntu 22.04+, Debian 12+ | \`$(pkg '.*_amd64\.deb')\` | \`sudo apt install ./$(pkg '.*_amd64\.deb')\` |
| Fedora | \`$(pkg '.*\.rpm')\` | \`sudo dnf install ./$(pkg '.*\.rpm')\` |
| Any Linux | \`$(pkg '.*\.AppImage')\` | \`chmod +x\` it, then run it |
| Any Linux, no install | \`$(pkg '.*linux-x86_64\.tar\.gz')\` | unpack, run \`super-media-player\` |
| Windows 10/11 | \`$(pkg '.*setup\.exe')\` | run it |
| Windows, no install | \`$(pkg '.*windows.*\.zip')\` | unpack, run \`super-media-player.exe\` |
| macOS (Apple silicon, Intel) | \`.dmg\` | open it, drag the app to Applications |

Check a download with \`sha256sum -c --ignore-missing SHA256SUMS-$version\`.

Built from commit $commit.
EOF
jq -n --rawfile body "$work/body.md" '{body: $body, draft: false, make_latest: "true"}' |
    call PATCH "$api/releases/$id" --data-binary @- >"$work/release.json"
echo "Published $(jq -r .html_url "$work/release.json")"
