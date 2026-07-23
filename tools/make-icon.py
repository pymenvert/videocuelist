# -*- coding: utf-8 -*-
"""Génère l'icône Conduite (crates/app/assets/conduite.ico + .png).

Motif : carré arrondi sombre (fond de régie), pastille « GO » à l'accent
bleu du produit, triangle GO net à l'encre sombre. Aucun dégradé.

Usage : python tools/make-icon.py
Dépendance : Pillow (pip install pillow)
"""
from pathlib import Path

from PIL import Image, ImageDraw

# Palette du produit (webui/style.css)
BG = (14, 16, 19, 255)         # --bg #0e1013
BORDER = (38, 44, 54, 255)     # liseré discret
ACCENT = (77, 163, 255, 255)   # --accent #4da3ff
INK = (7, 21, 39, 255)         # --accent-ink #071527

S = 1024  # taille de rendu maître (suréchantillonné)


def draw_master() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Carré arrondi sombre plein cadre
    radius = int(S * 0.22)
    d.rounded_rectangle([0, 0, S - 1, S - 1], radius=radius, fill=BG)
    # Liseré intérieur discret (lisibilité sur fond noir)
    inset = int(S * 0.012)
    d.rounded_rectangle(
        [inset, inset, S - 1 - inset, S - 1 - inset],
        radius=radius - inset,
        outline=BORDER,
        width=int(S * 0.012),
    )

    # Pastille GO
    cx, cy = S / 2, S / 2
    r = S * 0.335
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=ACCENT)

    # Triangle GO (pointe à droite), centré optiquement dans la pastille
    tw = r * 1.02   # largeur du triangle
    th = r * 1.14   # hauteur du triangle
    ox = r * 0.10   # décalage optique vers la droite
    left = cx - tw / 2 + ox
    d.polygon(
        [
            (left, cy - th / 2),
            (left, cy + th / 2),
            (left + tw, cy),
        ],
        fill=INK,
    )
    return img


def main() -> None:
    out_dir = Path(__file__).resolve().parent.parent / "crates" / "app" / "assets"
    out_dir.mkdir(parents=True, exist_ok=True)

    master = draw_master()
    master.resize((256, 256), Image.LANCZOS).save(out_dir / "conduite.png")

    sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)]
    master.resize((256, 256), Image.LANCZOS).save(
        out_dir / "conduite.ico", sizes=sizes
    )
    print(f"OK -> {out_dir / 'conduite.ico'} ({[s[0] for s in sizes]})")
    print(f"OK -> {out_dir / 'conduite.png'}")


if __name__ == "__main__":
    main()
