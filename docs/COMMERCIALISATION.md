# Commercialisation — feuille de route « process »

Synthèse des items de l'audit 2026-07-24 qui ne se codent pas : ce sont
des actions à mener par Pym, dans cet ordre suggéré. Le produit, lui, est
prêt côté pipeline (zip versionné + SHA-256, CHANGELOG, licences,
`latest.json`, checklist [RELEASE.md](RELEASE.md)).

## 1. Canaux de vente

- **Démarrer avec itch.io et/ou Gumroad** : les deux sont *Merchant of
  Record* — ils encaissent, gèrent la TVA UE et la facturation à ta
  place. Zéro paperasse fiscale au lancement, commission raisonnable.
- **Paddle ou Lemon Squeezy plus tard**, seulement si le volume le
  justifie (meilleure image « pro », licences par clé, mais intégration
  plus lourde).
- Ce qu'on uploade à chaque release : le zip `Conduite-{version}-win64.zip`
  **et** son `.sha256` (déjà produits par `tools/package.ps1`), la
  section du CHANGELOG en description.

## 2. Support (soutenable en solo)

Écrire le cadre AVANT la première vente et l'afficher sur la page de
vente — il protège autant qu'il rassure :

- **Un canal unique** (une adresse mail dédiée), pas de hotline.
- Engagement réaliste : « réponse sous 2-3 jours ouvrés — pas
  d'assistance en direct pendant vos représentations ».
- Le **manuel publié en ligne** (le repo GitHub suffit au début) + une
  FAQ courte alimentée par les premiers mails.
- Toujours demander le **rapport de diagnostic** (Réglages → Rapport de
  diagnostic, voir MANUEL.md §13) : un zip local, chemins expurgés, qui
  évite trois allers-retours par ticket.

## 3. Nom, domaine, marque

- Le nom **« Conduite »** est validé (DECISIONS.md, 2026-07-24).
  Communiquer systématiquement « **Conduite — régie vidéo de
  spectacle** » (le mot seul est noyé dans les résultats « permis de
  conduire »).
- **Avant la première vente** : recherche d'antériorité **gratuite** sur
  la base INPI (classe 9, logiciels) — le dépôt de marque payant n'est
  pas prioritaire, la recherche si.
- Réserver le **domaine** et les handles réseaux sociaux dès que le
  produit est montrable. Le renommage du repo GitHub
  (`videocuelist` → `conduite`) reste possible, les redirections sont
  automatiques.

## 4. Signature Authenticode

- Tant que `conduite.exe` n'est pas signé, SmartScreen avertira les
  acheteurs au premier lancement : l'**assumer dans les notes de
  version** et publier le SHA-256 (déjà fait), le manuel documente
  l'exclusion antivirus (MANUEL.md §9).
- Acheter un **certificat de signature de code OV** (compter ~200-400 €/an ;
  l'EV lève la réputation SmartScreen immédiatement mais coûte plus cher —
  l'OV la construit avec les téléchargements).
- Une fois le certificat en main : signer **avant** le packaging, avec
  horodatage — l'étape est déjà écrite dans [RELEASE.md](RELEASE.md) §3.

## 5. Télémétrie et vie privée (position produit)

Règle non négociable, c'est un argument de vente en spectacle :

- **Rien n'est envoyé nulle part par défaut, jamais.** Aujourd'hui le
  produit n'a AUCUNE télémétrie réseau ; le rapport de diagnostic est un
  fichier local que l'utilisateur attache lui-même.
- La vérification de mise à jour est **opt-in** (case décochée par
  défaut), en mode Edit seulement, et ne télécharge rien.
- Si un jour un rapport de crash automatisé (type Sentry) se justifie :
  case explicite désactivée par défaut, dialogue post-crash avec envoi
  **manuel**, chemins personnels expurgés — jamais d'envoi silencieux
  (les minidumps sont des extraits mémoire).

## Prochaines actions concrètes, dans l'ordre

1. Recherche d'antériorité INPI (gratuite) + réservation du domaine.
2. Ouvrir l'adresse mail de support + écrire la page « support » (cadre §2).
3. Créer la page itch.io ou Gumroad (captures, vidéo 90 s quand elle existe).
4. Première release publique en suivant [RELEASE.md](RELEASE.md) (soak 8 h = gate).
5. Certificat Authenticode dès les premières ventes.
