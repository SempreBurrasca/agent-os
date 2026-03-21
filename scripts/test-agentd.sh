#!/bin/bash
# ============================================================
# Script di test per agentd — invia messaggi via socat
# Uso: ./scripts/test-agentd.sh "mostrami i file"
# ============================================================

SOCKET="/run/agentd.sock"
TEXT="${1:-ciao, come stai?}"

# Fallback per sviluppo (macOS/non-root)
if [ ! -S "$SOCKET" ]; then
    SOCKET="/tmp/agentd.sock"
fi

if [ ! -S "$SOCKET" ]; then
    echo "Errore: agentd non in esecuzione (socket non trovato)"
    echo "Avvia agentd con: cargo run -p agentd"
    exit 1
fi

# Costruisci il messaggio JSON-RPC
MSG=$(cat <<EOF
{"jsonrpc":"2.0","method":"user.input","params":{"type":"user.input","text":"${TEXT}"},"id":1}
EOF
)

echo "→ Invio: ${TEXT}"
echo ""

# Invia e ricevi
RESPONSE=$(echo "$MSG" | socat - UNIX-CONNECT:"$SOCKET")

echo "← Risposta:"
echo "$RESPONSE" | python3 -m json.tool 2>/dev/null || echo "$RESPONSE"
