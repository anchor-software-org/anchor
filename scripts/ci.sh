#!/usr/bin/env bash
# Local CI dispatcher — invoked as `ci <job>` inside either devenv shell, or
# directly as scripts/ci.sh from a plain shell. Each job runs inside the
# devenv environment that provides its tools, regardless of where you start:
#
#   check    fmt + clippy + test        (ci-rust.yml)                desktop env
#   sdk      SDK source conformance     (ci-rust.yml)                root env
#   android  unit tests + debug APK     (verification-artifacts.yml) root env + Android SDK
#   desktop  release binary             (verification-artifacts.yml) desktop env
#   appimage AppImage packaging         (appimage.yml)               Ubuntu 22.04 container
#
# `all` runs check, sdk, android, desktop. appimage is opt-in because it is a
# full release build inside the builder container.
#
# The job scripts under scripts/ci/ are the same ones the workflows call, so a
# green local run means green CI.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

die() { echo "ci: $*" >&2; exit 64; }

[[ $# -ge 1 ]] || die "usage: ci <job|all> [job...]"

jobs=()
for arg in "$@"; do
    case "${arg,,}" in
        all) jobs+=(check sdk android desktop) ;;
        check|sdk|android|desktop|appimage) jobs+=("${arg,,}") ;;
        *) die "unknown job: $arg" ;;
    esac
done

# Run $script inside the devenv rooted at $1 when we are not already there.
in_env() {
    command -v devenv >/dev/null \
        || die "not in the matching devenv shell and 'devenv' is not on PATH — run: devenv shell, then 'ci $*', or bash $2 directly"
    (cd "$1" && devenv shell -- bash "$2")
}

# Collect job outputs under one predictable, gitignored directory so artifacts
# are easy to find after a run (links, not copies).
artifacts_dir="$repo_root/target/ci-artifacts"
collect() {
    local linked=0 f
    for f in "$@"; do
        [[ -e "$f" ]] || continue
        mkdir -p "$artifacts_dir"
        ln -sfn "$f" "$artifacts_dir/$(basename "$f")"
        linked=1
    done
    [[ "$linked" == 1 ]] || echo "ci: warning: no artifacts found for $job" >&2
}

for job in "${jobs[@]}"; do
    echo "==> ci: $job"
    case "$job" in
        check)
            script="$repo_root/scripts/ci/check.sh"
            if [[ "${DEVENV_ROOT:-}" == "$repo_root/anchor-desktop" ]]; then
                bash "$script"
            else
                in_env "$repo_root/anchor-desktop" "$script"
            fi ;;
        desktop)
            script="$repo_root/scripts/ci/desktop-binary.sh"
            if [[ "${DEVENV_ROOT:-}" == "$repo_root/anchor-desktop" ]]; then
                bash "$script"
            else
                in_env "$repo_root/anchor-desktop" "$script"
            fi ;;
        sdk)
            script="$repo_root/scripts/ci/sdk-conformance.sh"
            if [[ "${DEVENV_ROOT:-}" == "$repo_root" ]]; then
                bash "$script"
            else
                in_env "$repo_root" "$script"
            fi ;;
        android)
            script="$repo_root/scripts/ci/android.sh"
            if [[ "${DEVENV_ROOT:-}" == "$repo_root" ]]; then
                bash "$script"
            else
                in_env "$repo_root" "$script"
            fi ;;
        appimage)
            # Env-agnostic: builds inside the Ubuntu 22.04 builder container,
            # which is also what makes it match CI bit-for-bit.
            "$repo_root/scripts/build-appimage-container.sh" ;;
    esac

    case "$job" in
        android)
            collect "$repo_root"/anchor/app/build/outputs/apk/debug/*.apk ;;
        desktop)
            collect "$repo_root/anchor-desktop/target/release/anchor" ;;
        appimage)
            collect "$repo_root"/anchor-desktop/target/appimage/Anchor-*.AppImage ;;
    esac
done

if [[ -d "$artifacts_dir" ]]; then
    echo "==> ci: artifacts in $artifacts_dir"
    ls -lh "$artifacts_dir"
fi
