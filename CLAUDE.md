# VideoCuelist — consignes de travail

Régie vidéo de spectacle en Rust (mapping, ISF/GLSL, cues, MIDI/OSC/Art-Net).
Cibles : Windows, macOS, Ubuntu, Raspberry Pi 4/5.

## À lire avant de coder (dans cet ordre)

1. `docs/DECISIONS.md` — décisions actées, **source de vérité**. On ne re-débat pas sans élément nouveau.
2. `docs/SPEC.md` — spec fonctionnelle (concepts : slice, layer, cue, paramètre).
3. `docs/ARCHITECTURE.md` — crates, réutilisation Lanterne, modèle de threads.
4. `docs/PLAN.md` — phase en cours et critères de sortie.

## Règles

- Une décision prise → une ligne datée dans `DECISIONS.md` (avec le pourquoi).
- Le vocabulaire de la SPEC est normatif : slice, layer, output, paramètre, cue, deck A/B. Les identifiants du code sont en anglais, la doc et l'UI en français (UI bilingue FR/EN à terme).
- Réutilisation Lanterne (github.com/pymenvert/toolbox, MIT, même auteur) : emprunt de code ciblé documenté dans ARCHITECTURE.md — jamais de copie de code GPL d'ailleurs.
- Fiabilité spectacle non négociable : pas d'I/O disque, de compilation shader ni d'allocation lourde sur le thread de rendu en mode show ; chargement de show tolérant (média manquant = placeholder, jamais un refus) ; écritures atomiques.
- Tests unitaires sur le cœur (cues, paramètres, patch, transitions) — comme dans Lanterne.
- L'utilisateur (Pym) travaille en français : réponses et docs en français.
