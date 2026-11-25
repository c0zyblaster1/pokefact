#!/usr/bin/env bash
set -euo pipefail

REPO="c0zyblaster1/pokefact"
REPO_URL="https://github.com/${REPO}"

info() { printf "\033[1;36m==>\033[0m %s\n" "$1"; }
warn() { printf "\033[1;33m==>\033[0m %s\n" "$1"; }
ok()   { printf "\033[1;32m==>\033[0m %s\n" "$1"; }
err()  { printf "\033[1;31m==>\033[0m %s\n" "$1" >&2; }

ensure_path_entry() {
    local dir="$1"
    for rc in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile"; do
        if [ -f "$rc" ] && ! grep -Fq "$dir" "$rc"; then
            printf '\nexport PATH="%s:$PATH"\n' "$dir" >> "$rc"
        fi
    done
    export PATH="${dir}:$PATH"
}

is_termux() {
    [ -n "${TERMUX_VERSION:-}" ] || [ -d "/data/data/com.termux" ]
}

install_termux() {
    local prefix="${PREFIX:-/data/data/com.termux/files/usr}"
    info "Termux / Android detected — setting up dependencies automatically..."

    export DEBIAN_FRONTEND=noninteractive
    pkg install -y -o Dpkg::Options::="--force-confold" rust clang binutils git play-audio

    # Some Termux binutils builds place ld.bfd under libexec/binutils instead of bin/
    if ! command -v ld.bfd >/dev/null 2>&1 && [ -x "${prefix}/libexec/binutils/ld.bfd" ]; then
        export PATH="${prefix}/libexec/binutils:$PATH"
    fi

    local installed=0

    # Primary build path: GNU ld.bfd bypasses Android ARM64 tagged-pointer crashes in lld
    if command -v ld.bfd >/dev/null 2>&1; then
        info "Building pokefact with GNU ld.bfd linker (Android-safe)..."
        if RUSTFLAGS="-C link-arg=-fuse-ld=bfd" cargo install --force --git "$REPO_URL"; then
            installed=1
        else
            warn "ld.bfd build hit an issue — falling back to single-threaded lld..."
        fi
    fi

    # Fallback path: single-threaded lld with retry to mitigate intermittent Android MTE/Scudo aborts
    if [ "$installed" -eq 0 ]; then
        for attempt in 1 2; do
            info "Building pokefact with single-threaded lld (attempt ${attempt}/2)..."
            if RUSTFLAGS="-C link-arg=-Wl,--threads=1" CARGO_BUILD_JOBS=2 cargo install --force --git "$REPO_URL"; then
                installed=1
                break
            fi
            warn "Linker aborted on attempt ${attempt}, retrying..."
        done
    fi

    if [ "$installed" -eq 0 ]; then
        err "Build failed on Termux. Try running: pkg update -y && pkg upgrade -y, then re-run this installer."
        exit 1
    fi

    # Make pokefact available immediately in the current Termux session without restarting
    if [ -x "$HOME/.cargo/bin/pokefact" ] && [ -d "${prefix}/bin" ]; then
        ln -sf "$HOME/.cargo/bin/pokefact" "${prefix}/bin/pokefact"
    fi
    ensure_path_entry "$HOME/.cargo/bin"
}

install_unix() {
    local os arch asset="" bin_dir="$HOME/.local/bin"
    os="$(uname -s)"
    arch="$(uname -m)"

    case "${os}:${arch}" in
        Linux:x86_64)         asset="pokefact-linux-x86_64" ;;
        Darwin:arm64)         asset="pokefact-macos-arm64" ;;
        Darwin:x86_64)        asset="pokefact-macos-x86_64" ;;
    esac

    if [ -n "$asset" ]; then
        local url="https://github.com/${REPO}/releases/latest/download/${asset}"
        mkdir -p "$bin_dir"
        info "Checking for prebuilt binary (${asset})..."
        if curl -fsSL "$url" -o "${bin_dir}/pokefact.tmp" 2>/dev/null; then
            chmod +x "${bin_dir}/pokefact.tmp"
            mv "${bin_dir}/pokefact.tmp" "${bin_dir}/pokefact"
            ensure_path_entry "$bin_dir"
            return 0
        fi
        rm -f "${bin_dir}/pokefact.tmp"
        warn "No matching prebuilt binary found — falling back to Cargo source build..."
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        err "Rust/Cargo is not installed. Install it via https://rustup.rs and re-run this script."
        exit 1
    fi

    info "Building pokefact from source via Cargo..."
    cargo install --force --git "$REPO_URL"
    ensure_path_entry "$HOME/.cargo/bin"
}

if is_termux; then
    install_termux
else
    install_unix
fi

ok "pokefact installed successfully!"
printf "  Try: \033[1;33mpokefact -p gengar -a\033[0m  or  \033[1;33mpokefact --battle gengar\033[0m\n"
