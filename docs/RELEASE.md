# Checklist de release — Conduite

Concise et exécutable, dans l'ordre. Une étape rouge = on s'arrête, on
corrige, on recommence à l'étape 1. Rien ne se publie sans le soak 8 h.

## 1. Qualité du code

```powershell
cargo test --workspace                                  # tous verts, zéro ignoré
cargo clippy --workspace --all-targets -- -D warnings   # zéro warning
cargo deny check licenses advisories sources            # allowlist deny.toml
```

La CI (`.github/workflows/ci.yml`) rejoue tests (Windows/Ubuntu/macOS) et
cargo-deny sur le push : **attendre la CI verte** avant de continuer —
elle attrape ce qui est invisible en local.

## 2. Version et traçabilité

- [ ] Bumper `version` dans `Cargo.toml` (`[workspace.package]`) — SemVer.
- [ ] `CHANGELOG.md` : transformer « en préparation » en section datée,
      relire que chaque feature visible y est (format Keep a Changelog, fr).
- [ ] Si les dépendances ont changé :
      `cargo about generate about.hbs -o licenses/THIRD-PARTY-NOTICES.html`.
- [ ] Commit « Release X.Y.Z » + push, CI verte.

## 3. Packaging

### Linux (portable + paquet)

```bash
tools/package-linux.sh --deb
```

Produit `dist/Conduite-{version}-linux-{arch}.tar.gz` et
`dist/conduite_{version}_{arch}.deb`, chacun avec son `.sha256`. Pour un
player Raspberry Pi 64 bits : `--target aarch64-unknown-linux-gnu` (toolchain
de cross-compilation requise), ou construire sur le Pi lui-même.

- [ ] `dpkg-deb --info` : version, architecture et dépendances attendues.
- [ ] Installer sur une machine propre (`sudo apt install ./…deb`), vérifier
      que `/var/lib/conduite` est créé et appartient à l'utilisateur système,
      que `conduite --version` répond, et que le service démarre
      (`sudo systemctl enable --now conduite`) avec `/health` en `ok`.
- [ ] `systemd-analyze verify` sur les trois unités : silence exigé.
- [ ] Désinstaller (`sudo apt remove`) : `/var/lib/conduite` doit RESTER.

### Windows

```powershell
powershell -File tools/package.ps1
```

Produit `dist/Conduite-{version}-win64.zip` + `.sha256`. Le script vérifie
lui-même : FFmpeg **LGPL** uniquement (refus du GPL sans `-AllowGpl`),
notices présentes, LISEZMOI lisible, version injectée.

- [ ] Contrôler le SHA-256 affiché = fichier `.sha256`.
- [ ] **Signature Authenticode** (dès que le certificat est disponible) :
      signer `conduite.exe` AVANT le zip, horodatage obligatoire
      (`signtool sign /fd SHA256 /tr <serveur> /td SHA256 …`), puis
      re-packager. Tant que non signé : l'assumer dans les notes de
      version (SmartScreen avertira).

## 4. Endurance — LA gate

```powershell
cargo build --release -p conduite
powershell -File tools/soak.ps1          # 8 h par défaut
```

- [ ] Verdict **PLATE** exigé (mémoire et handles stables, zéro process
      résiduel). CSV archivé automatiquement dans `docs/bench/` — le
      commiter : c'est la preuve d'endurance de la version.
- [ ] Verdict CROISSANTE ou ECHEC → pas de release, ouvrir un ticket,
      retour étape 1.

## 5. Smoke test du zip livré

Dézipper `dist/Conduite-{version}-win64.zip` dans un dossier de test
(PAS dans %TEMP%), puis, depuis ce dossier :

- [ ] `conduite.exe` démarre, l'UI répond sur http://localhost:9820.
- [ ] `--version` affiche la bonne version (et le hash git).
- [ ] Show de démo : un GO produit une image ; Échap = panic ; B = DBO.
- [ ] **Bilingue** : Réglages → Langue → *English*, puis parcourir les
      10 onglets. Aucun mot français ne doit rester **hors du Journal**
      (le journal et `logs/` sont volontairement en français). Repasser en
      *Français* : tout revient, sans rechargement. Les tests bloquent déjà
      une chaîne non traduite — cette passe attrape ce qu'un test ne voit
      pas : une traduction qui déborde de son bouton.
- [ ] LISEZMOI.txt lisible (accents corrects), licenses/ complet
      (FFMPEG.txt, THIRD-PARTY-NOTICES.html), shaders/CREDITS.txt présent.
- [ ] Quitter proprement (bouton Quitter) : aucun process résiduel.

## 6. Publication

- [ ] Tag git : `git tag vX.Y.Z` puis `git push origin vX.Y.Z`.
      **Note : aucun workflow de release automatique n'existe à ce jour**
      (pas de `.github/workflows/release.yml`) — la release GitHub se crée
      à la main : page Releases → « Draft a new release » → tag vX.Y.Z →
      joindre le zip **et** le `.sha256` → coller la section du CHANGELOG.
- [ ] Mettre à jour `latest.json` à la racine du dépôt (servi par
      `https://raw.githubusercontent.com/pymenvert/videocuelist/main/latest.json`,
      lu par la vérification de mise à jour opt-in) :

```json
{
  "version": "X.Y.Z",
  "url": "https://github.com/pymenvert/videocuelist/releases/tag/vX.Y.Z",
  "notes": "Résumé d'une ou deux phrases des nouveautés."
}
```

      Commiter et pousser **après** que la release est en ligne (sinon le
      badge de mise à jour pointe dans le vide).

## 7. Après coup

- [ ] Vérifier le lien de téléchargement depuis un navigateur « neuf ».
- [ ] Archiver le CSV du soak et les notes de la release.
- [ ] Rouvrir `CHANGELOG.md` avec une section « en préparation » vide.
