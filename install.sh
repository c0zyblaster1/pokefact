#!/usr/bin/env bash
set -e

REPO="c0zyblaster1/pokefact"
INSTALL_DIR="$HOME/.local/bin"

if [ -n "$TERMUX_VERSION" ]; then
    echo "Termux detected. Installing Rust and build tools..."
    pkg update -y
    pkg install -y rust clang pkg-config git ndk-multilib
    NDK_LIB="$PREFIX/opt/ndk-multilib/aarch64-linux-android/lib"
    if [ -d "$NDK_LIB" ]; then
        export RUSTFLAGS="-L $NDK_LIB -C link-arg=-lc++abi"
    fi
    cargo install --force --git "https://github.com/$REPO"
    ln -sf "$HOME/.cargo/bin/pokefact" "$PREFIX/bin/pokefact"
    echo "Done! Try running: pokefact -p gengar"
    exit 0
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
ASSET=""

if [ "$OS" = "Darwin" ]; then
    if [ "$ARCH" = "arm64" ]; then
        ASSET="pokefact-macos-arm64"
    else
        ASSET="pokefact-macos-x64"
    fi
elif [ "$OS" = "Linux" ] && [ "$ARCH" = "x86_64" ]; then
    ASSET="pokefact-linux-x64"
fi

if [ -n "$ASSET" ]; then
    echo "Downloading prebuilt binary ($ASSET)..."
    mkdir -p "$INSTALL_DIR"
    curl -fsSL "https://github.com/$REPO/releases/latest/download/$ASSET" -o "$INSTALL_DIR/pokefact"
    chmod +x "$INSTALL_DIR/pokefact"

    if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
        SHELL_RC="$HOME/.bashrc"
        [ -n "$ZSH_VERSION" ] || [ "$(basename "$SHELL")" = "zsh" ] && SHELL_RC="$HOME/.zshrc"
        echo "export PATH=\"\$HOME/.local/bin:\$PATH\"" >> "$SHELL_RC"
        echo "Added $INSTALL_DIR to $SHELL_RC. Restart your terminal or run: source $SHELL_RC"
    fi
    echo "Done! Try running: pokefact -p gengar"
    exit 0
fi

echo "No prebuilt binary for $OS ($ARCH). Installing dependencies and building from source..."
if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update && sudo apt-get install -y curl build-essential pkg-config libasound2-dev git
elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y curl gcc pkg-config alsa-lib-devel git
elif command -v pacman >/dev/null 2>&1; then
    sudo pacman -Sy --noconfirm curl base-devel pkgconf alsa-lib git
fi

if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

cargo install --force --git "https://github.com/$REPO"
echo "Done! Try running: pokefact -p gengar"
