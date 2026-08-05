#!/usr/bin/env bash
# Packaging Linux — pendant de tools/package.ps1.
#
#   tools/package-linux.sh [--deb] [--target <triple>]
#
# Produit dans dist/ :
#   Conduite-<version>-linux-<arch>.tar.gz  + .sha256   (dossier portable)
#   conduite_<version>_<arch>.deb           + .sha256   (avec --deb)
#
# Deux formes parce que deux usages :
#   - le TAR.GZ est le portable, comme sur Windows : on le dézippe où l'on
#     veut, tout est relatif au binaire, rien n'est installé ;
#   - le .DEB est l'installation d'un player fixe (Raspberry Pi, machine de
#     salle) : binaire système, données dans /var/lib/conduite, service
#     systemd. C'est `--home` qui rend cette séparation possible.
#
# ffmpeg n'est PAS embarqué ici, contrairement au portable Windows : les
# distributions le fournissent (le .deb en dépend), et redistribuer un build
# statique imposerait d'en assumer la chaîne LGPL pour chaque architecture.
# Le portable le cherche dans bin/ puis dans le PATH, comme ailleurs.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

make_deb=0
target=""
while [ $# -gt 0 ]; do
    case "$1" in
        --deb) make_deb=1 ;;
        --target) target="${2:?--target attend un triple}"; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "option inconnue : $1" >&2; exit 2 ;;
    esac
    shift
done

# --- Version (source de vérité : [workspace.package] de Cargo.toml) --------
version="$(awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version *=/{gsub(/[" ]/,"");sub(/version=/,"");print;exit}' Cargo.toml)"
[ -n "$version" ] || { echo "version introuvable dans Cargo.toml ([workspace.package])" >&2; exit 1; }

# Architecture Debian : c'est elle qui nomme le paquet et le tar.
case "${target:-$(uname -m)}" in
    x86_64*|amd64) arch=amd64 ;;
    aarch64*|arm64) arch=arm64 ;;   # Raspberry Pi 4/5 64 bits
    armv7l|armv7*) arch=armhf ;;
    *) echo "architecture non gérée : ${target:-$(uname -m)}" >&2; exit 1 ;;
esac

echo "Conduite v$version ($arch) — build release…"
if [ -n "$target" ]; then
    cargo build --release -p conduite --target "$target"
    exe="target/$target/release/conduite"
else
    cargo build --release -p conduite
    exe="target/release/conduite"
fi
[ -x "$exe" ] || { echo "binaire introuvable : $exe" >&2; exit 1; }

# --- Dossier portable -------------------------------------------------------
dist="dist/Conduite-portable-linux-$arch"
rm -rf "$dist"
mkdir -p "$dist"/{media,shows,shaders,logs,bin,licenses}
install -m 0755 "$exe" "$dist/conduite"

[ -d shaders/examples ] && cp -r shaders/examples "$dist/shaders/examples"
cp shaders/CREDITS.txt "$dist/shaders/CREDITS.txt"
# Médias de démo : le premier GO doit produire une image.
for f in media/demo-*; do [ -e "$f" ] && cp "$f" "$dist/media/"; done

# Licences : obligation d'attribution en redistribution binaire.
if [ ! -f licenses/THIRD-PARTY-NOTICES.html ]; then
    echo "licenses/THIRD-PARTY-NOTICES.html manquant — générer avec :" >&2
    echo "  cargo about generate about.hbs -o licenses/THIRD-PARTY-NOTICES.html" >&2
    exit 1
fi
cp licenses/* "$dist/licenses/"
cp LICENSE "$dist/licenses/LICENSE-CONDUITE.txt"
cp docs/MANUEL.md "$dist/MANUEL.md"
sed "s/{VERSION}/$version/g" tools/LISEZMOI-linux.txt > "$dist/LISEZMOI.txt"

tar="dist/Conduite-$version-linux-$arch.tar.gz"
rm -f "$tar" "$tar.sha256"
tar -czf "$tar" -C dist "$(basename "$dist")"
( cd dist && sha256sum "$(basename "$tar")" > "$(basename "$tar").sha256" )

echo "OK -> $dist"
echo "OK -> $tar"
echo "SHA-256 : $(cut -d' ' -f1 < "$tar.sha256")"

[ "$make_deb" -eq 1 ] || exit 0

# --- Paquet .deb (player installé) ------------------------------------------
# Disposition FHS : binaire système, assets en lecture seule, données
# inscriptibles séparées — sans `--home`, tout cela serait impossible.
command -v dpkg-deb >/dev/null || { echo "dpkg-deb absent (paquet dpkg-dev)" >&2; exit 1; }

pkg="dist/deb-$arch"
rm -rf "$pkg"
mkdir -p "$pkg"/DEBIAN \
         "$pkg"/usr/bin \
         "$pkg"/usr/share/conduite/shaders \
         "$pkg"/usr/share/doc/conduite/licenses \
         "$pkg"/lib/systemd/system

install -m 0755 "$exe" "$pkg/usr/bin/conduite"
[ -d shaders/examples ] && cp -r shaders/examples "$pkg/usr/share/conduite/shaders/examples"
cp shaders/CREDITS.txt "$pkg/usr/share/conduite/shaders/CREDITS.txt"
cp licenses/* "$pkg/usr/share/doc/conduite/licenses/"
cp LICENSE "$pkg/usr/share/doc/conduite/licenses/LICENSE-CONDUITE.txt"
cp docs/MANUEL.md "$pkg/usr/share/doc/conduite/MANUEL.md"
sed "s/{VERSION}/$version/g" tools/LISEZMOI-linux.txt > "$pkg/usr/share/doc/conduite/LISEZMOI.txt"
install -m 0644 tools/systemd/conduite.service "$pkg/lib/systemd/system/conduite.service"
install -m 0644 tools/systemd/conduite-health.service "$pkg/lib/systemd/system/conduite-health.service"
install -m 0644 tools/systemd/conduite-health.timer "$pkg/lib/systemd/system/conduite-health.timer"
install -m 0755 tools/systemd/conduite-health-check.sh "$pkg/usr/bin/conduite-health-check"

installed_kb="$(du -sk "$pkg" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<EOF
Package: conduite
Version: $version
Section: video
Priority: optional
Architecture: $arch
Depends: ffmpeg, libasound2t64 | libasound2, curl
Maintainer: Pym <pymenvert@hotmail.com>
Installed-Size: $installed_kb
Homepage: https://github.com/pymenvert/videocuelist
Description: Regie video de spectacle (cuelist, mapping, ISF, MIDI/OSC/Art-Net)
 Conduite enchaine des cues (des scenes completes) au GO, mappe les images
 sur les surfaces reelles et se pilote en OSC, MIDI et Art-Net. L'interface
 de regie est une page web servie par le moteur lui-meme.
 .
 Le paquet installe un service systemd desactive par defaut, prevu pour un
 player fixe : donnees dans /var/lib/conduite, journaux dans
 /var/lib/conduite/logs.
EOF

cat > "$pkg/DEBIAN/postinst" <<'EOF'
#!/bin/sh
# Données inscriptibles hors du paquet : le binaire est en lecture seule
# dans /usr/bin, les shows de l'utilisateur vivent dans /var/lib/conduite.
set -e
HOME_DIR=/var/lib/conduite

if [ "$1" = configure ]; then
    if ! getent passwd conduite >/dev/null; then
        adduser --system --group --home "$HOME_DIR" \
                --gecos "Conduite (regie video)" conduite >/dev/null
    fi
    # Groupes utiles à un player : sortie vidéo (video/render) et MIDI/audio.
    for grp in video render audio; do
        getent group "$grp" >/dev/null && adduser conduite "$grp" >/dev/null || true
    done
    mkdir -p "$HOME_DIR"/media "$HOME_DIR"/shows "$HOME_DIR"/shaders "$HOME_DIR"/logs
    # Shaders d'exemple copiés au PREMIER déploiement seulement : ne jamais
    # écraser ceux que l'utilisateur a déposés ou modifiés.
    if [ -d /usr/share/conduite/shaders/examples ] && \
       [ ! -e "$HOME_DIR"/shaders/examples ]; then
        cp -r /usr/share/conduite/shaders/examples "$HOME_DIR"/shaders/
    fi
    [ -f "$HOME_DIR"/shaders/CREDITS.txt ] || \
        cp /usr/share/conduite/shaders/CREDITS.txt "$HOME_DIR"/shaders/ 2>/dev/null || true
    chown -R conduite:conduite "$HOME_DIR"
    chmod 0750 "$HOME_DIR"
fi

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi

exit 0
EOF
chmod 0755 "$pkg/DEBIAN/postinst"

cat > "$pkg/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
    systemctl stop conduite-health.timer >/dev/null 2>&1 || true
    systemctl stop conduite.service >/dev/null 2>&1 || true
fi
exit 0
EOF
chmod 0755 "$pkg/DEBIAN/prerm"

cat > "$pkg/DEBIAN/postrm" <<'EOF'
#!/bin/sh
# purge n'efface JAMAIS /var/lib/conduite : ce sont les conduites de
# l'utilisateur. Les supprimer sur un désinstall serait une perte de travail
# irréversible — on le dit, on ne le fait pas.
set -e
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
if [ "$1" = purge ]; then
    echo "Les shows et médias restent dans /var/lib/conduite (à supprimer à la main si voulu)."
fi
exit 0
EOF
chmod 0755 "$pkg/DEBIAN/postrm"

deb="dist/conduite_${version}_${arch}.deb"
rm -f "$deb" "$deb.sha256"
dpkg-deb --root-owner-group --build "$pkg" "$deb" >/dev/null
( cd dist && sha256sum "$(basename "$deb")" > "$(basename "$deb").sha256" )

echo "OK -> $deb"
echo "SHA-256 : $(cut -d' ' -f1 < "$deb.sha256")"
echo
echo "Installer :   sudo apt install ./$deb"
echo "Démarrer  :   sudo systemctl enable --now conduite"
