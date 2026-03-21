#!/bin/bash
# ============================================================
# Build script per il pacchetto .deb di AgentOS
# Eseguire sulla VM Ubuntu con Rust installato.
# ============================================================
set -e

VERSION="0.1.0"
ARCH="amd64"
PKG_NAME="agentos_${VERSION}_${ARCH}"
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "=== Build AgentOS v${VERSION} ==="

# 1. Compila in modalità release
echo "→ Compilazione release..."
cd "$PROJECT_DIR"
cargo build --release

# 2. Prepara la struttura del pacchetto
echo "→ Preparazione pacchetto..."
BUILD_DIR="/tmp/${PKG_NAME}"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/DEBIAN"
mkdir -p "$BUILD_DIR/usr/bin"
mkdir -p "$BUILD_DIR/usr/share/agentos"
mkdir -p "$BUILD_DIR/usr/share/wayland-sessions"
mkdir -p "$BUILD_DIR/usr/lib/systemd/user"
mkdir -p "$BUILD_DIR/etc/agentos"

# 3. Copia i binari
echo "→ Copia binari..."
cp target/release/agentd "$BUILD_DIR/usr/bin/"
cp target/release/agent-shell "$BUILD_DIR/usr/bin/"
cp target/release/agent-fs "$BUILD_DIR/usr/bin/"

# 4. Copia configurazione e file di supporto
echo "→ Copia configurazione..."
cp config.yaml "$BUILD_DIR/usr/share/agentos/"
cp debian/agentos.desktop "$BUILD_DIR/usr/share/agentos/"

# 5. Copia i servizi systemd (user-level)
echo "→ Copia servizi systemd..."
cp debian/agentd.service "$BUILD_DIR/usr/lib/systemd/user/"
cp debian/agent-fs.service "$BUILD_DIR/usr/lib/systemd/user/"

# 6. File DEBIAN
echo "→ Generazione metadati pacchetto..."
cp debian/control "$BUILD_DIR/DEBIAN/"
cp debian/postinst "$BUILD_DIR/DEBIAN/"
chmod 755 "$BUILD_DIR/DEBIAN/postinst"

# Calcola la dimensione installata
INSTALLED_SIZE=$(du -sk "$BUILD_DIR" | cut -f1)
echo "Installed-Size: ${INSTALLED_SIZE}" >> "$BUILD_DIR/DEBIAN/control"

# 7. Costruisci il .deb
echo "→ Build pacchetto .deb..."
dpkg-deb --build "$BUILD_DIR" "${PROJECT_DIR}/${PKG_NAME}.deb"

# 8. Cleanup
rm -rf "$BUILD_DIR"

echo ""
echo "=== Pacchetto creato: ${PKG_NAME}.deb ==="
echo "Installa con: sudo dpkg -i ${PKG_NAME}.deb"
echo "Poi: sudo apt-get install -f  (per le dipendenze)"
