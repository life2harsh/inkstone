#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

if [[ ! -f stonepen/Cargo.toml ]]; then
    echo "=== Initializing Stonepen submodules ==="
    git submodule update --init --recursive
fi
if [[ ! -f stonepen/Cargo.toml ]]; then
    echo "Stonepen is missing; run: git submodule update --init --recursive" >&2
    exit 1
fi

release=false
dist=false
run_checks=true
serverless_local=false

for argument in "$@"; do
    case "$argument" in
        --release) release=true ;;
        --dist) dist=true ;;
        --skip-checks) run_checks=false ;;
        --serverless-local) serverless_local=true ;;
        *)
            echo "Unknown argument: $argument" >&2
            exit 2
            ;;
    esac
done

if "$dist" && "$serverless_local"; then
    echo "Serverless local mode cannot be built as a production distribution" >&2
    exit 2
fi

if "$run_checks"; then
    echo "=== Running cargo fmt check ==="
    cargo fmt --all --check

    echo "=== Running core unit tests ==="
    cargo test --locked -p inkstone-core

    echo "=== Running client unit tests ==="
    cargo test --locked -p inkstone-client

    echo "=== Running web unit tests ==="
    cargo test --locked -p inkstone-web

    echo "=== Running workspace cargo check ==="
    cargo check --locked --workspace
fi

wasm_args=(build --target web)
cargo_args=()
if "$release"; then
    wasm_args+=(--release)
fi
if "$serverless_local"; then
    cargo_args+=(--features serverless-local)
fi

if ! "$dist"; then
    echo "=== Building web WASM artifact with wasm-pack ==="
    wasm-pack "${wasm_args[@]}" --out-dir ../../apps/web/pkg crates/inkstone-web --locked "${cargo_args[@]}"
    echo "=== Build completed successfully ==="
    exit 0
fi

echo "=== Building fingerprinted web distribution ==="
dist_dir="$PROJECT_DIR/apps/web/dist"
package_build_dir="$dist_dir/.pkg-build"
rm -rf "$dist_dir"
mkdir -p "$package_build_dir"
wasm-pack "${wasm_args[@]}" --out-dir "$package_build_dir" crates/inkstone-web --locked "${cargo_args[@]}"

bootstrap_build="$package_build_dir/inkstone_bootstrap.js"
in_module_script=false
while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" == *'<script type="module">'* ]]; then
        : > "$bootstrap_build"
        in_module_script=true
    elif "$in_module_script" && [[ "$line" == *'</script>'* ]]; then
        in_module_script=false
    elif "$in_module_script"; then
        line="${line//.\/pkg\/inkstone_web.js/.\/inkstone_web.js}"
        printf '%s\n' "${line#      }" >> "$bootstrap_build"
    fi
done < apps/web/index.html

if "$in_module_script" || [[ ! -s "$bootstrap_build" ]]; then
    echo "Failed to extract the web module bootstrap from apps/web/index.html" >&2
    exit 1
fi

package_hashes="$(
    for asset in inkstone_web_bg.wasm inkstone_web.js inkstone_bootstrap.js; do
        read -r digest _ < <(sha256sum "$package_build_dir/$asset")
        printf '%s\n' "$digest"
    done
)"
asset_hash="$(printf '%s' "$package_hashes" | sha256sum)"
asset_hash="${asset_hash%% *}"
package_dir="$dist_dir/pkg/$asset_hash"
mkdir -p "$package_dir"
cp -a "$package_build_dir/." "$package_dir/"
rm -rf "$package_build_dir"
cp apps/web/styles.css "$dist_dir/styles.css"
cp -r apps/web/vendor "$dist_dir/vendor"
for asset in sw.js manifest.webmanifest icon.svg icon-192.png icon-512.png icon-maskable-512.png apple-touch-icon.png; do
    cp "apps/web/$asset" "$dist_dir/$asset"
done

in_module_script=false
while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" == *'<script type="module">'* ]]; then
        printf '    <script type="module" src="./pkg/%s/inkstone_bootstrap.js"></script>\n' "$asset_hash"
        in_module_script=true
    elif "$in_module_script" && [[ "$line" == *'</script>'* ]]; then
        in_module_script=false
    elif ! "$in_module_script"; then
        printf '%s\n' "$line"
    fi
done < apps/web/index.html > "$dist_dir/index.html"

if "$in_module_script"; then
    echo "Unclosed module script in apps/web/index.html" >&2
    exit 1
fi

echo "=== Fingerprinted distribution built at apps/web/dist ==="
