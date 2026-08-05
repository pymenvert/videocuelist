#!/bin/sh
# Surveillance « vivant mais figé » d'un player Conduite.
#
# `Restart=on-failure` de systemd ne voit que les processus MORTS. Le cas qui
# fait rater un spectacle est l'autre : le process est bien là, la socket
# répond, mais le tick de rendu ne tourne plus. C'est exactement ce que
# l'endpoint /health rapporte (`status: stalled` quand le tick dépasse 2 s).
#
# Appelé par conduite-health.timer. Sort en 0 quand tout va bien ; redémarre
# le service et sort en 1 quand le moteur est figé ou muet.
set -eu

PORT="${CONDUITE_PORT:-9820}"
URL="http://127.0.0.1:${PORT}/health"
UNIT="${CONDUITE_UNIT:-conduite.service}"

log() { logger -t conduite-health "$*" 2>/dev/null || echo "conduite-health: $*" >&2; }

# Ne rien faire si l'opérateur a volontairement arrêté le service : une
# surveillance qui ressuscite un service arrêté à la main est un piège.
if ! systemctl is-active --quiet "$UNIT"; then
    exit 0
fi

body="$(curl -fsS --max-time 4 "$URL" 2>/dev/null || true)"

if [ -z "$body" ]; then
    log "aucune réponse de $URL — redémarrage de $UNIT"
    systemctl restart "$UNIT"
    exit 1
fi

# Espaces retirés avant de comparer : le contrat porte sur les VALEURS, pas
# sur la mise en forme du JSON. Sans ça, un jour où l'endpoint sortirait
# `{"status": "stalled"}` au lieu de `{"status":"stalled"}`, la surveillance
# se tairait — un silence indiscernable de « tout va bien ».
compact="$(printf '%s' "$body" | tr -d ' \t\r\n')"

case "$compact" in
    *'"status":"ok"'*)
        exit 0
        ;;
    *'"status":"stalled"'*)
        log "moteur figé (tick arrêté) d'après $URL — redémarrage de $UNIT : $body"
        systemctl restart "$UNIT"
        exit 1
        ;;
    *)
        # Réponse inattendue : on la journalise sans rien casser. Un
        # redémarrage sur un cas non compris ferait plus de dégâts qu'il
        # n'en répare.
        log "réponse inattendue de $URL (aucune action) : $body"
        exit 0
        ;;
esac
