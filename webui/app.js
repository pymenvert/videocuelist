/* Conduite — web UI v1 (vanilla, aucune dépendance).
   Store d'état minimal : `hello` pose S.show/S.runtime, les events et les
   trames "dyn" font des mises à jour ciblées. Tout est défensif : état
   absent => placeholders, jamais d'exception bloquante. */
'use strict';

(function () {

  /* ============================================================= bilingue
     `tr` (chaîne complète) et `trf` (gabarit + valeurs) viennent de
     i18n.js. Repli identité si le fichier manque : l'UI reste en français
     plutôt que de planter — même doctrine que le chargement de show. */
  var I18N = (window.Conduite && window.Conduite.i18n) || null;
  var tr = I18N ? I18N.tr : function (s) { return s; };
  var trf = I18N ? I18N.trf : function (tpl) {
    var args = Array.prototype.slice.call(arguments, 1);
    return String(tpl).replace(/\{(\d+)\}/g, function (m, i) {
      var v = args[+i];
      return (v === undefined || v === null) ? '' : String(v);
    });
  };

  /* ============================================================== état */

  var RT0 = {
    mode: 'edit', active: null, standby: null, progress: 0, remaining_s: 0,
    transition_active: false, bpm: 120, master: 1, dbo: false, mod_levels: []
  };

  var S = {
    raw: null,          // état complet du dernier hello (specs éventuelles…)
    show: null,         // Show sérialisé
    runtime: null,      // RuntimeStatus
    health: null,       // HealthSnapshot
    fft: null,          // dernière trame FFT {bins:[0..1 ×64], device} ou null
    about: null,        // GET /about : version, licence, crédits (peut manquer)
    logs: [],           // {level,target,message,ts}
    logFilter: 'all',
    tab: 'live',
    sel: { output: null, slice: null, corner: null, cue: null, media: null, material: null },
    dragging: false,
    connected: false
  };

  function show() { return S.show || {}; }
  function rt() { return S.runtime || RT0; }
  function slices() { return show().slices || []; }
  function cues() { return show().cues || []; }
  function medias() { return show().media || []; }
  function materials() { return show().materials || []; }
  function outputs() { return show().outputs || []; }
  function modulators() { return show().modulators || []; }
  function routes() { return show().routes || []; }
  function settings() { return show().settings || {}; }
  function patch() { return show().patch || { artnet: [], midi: [], osc_out: null }; }
  function isShowMode() { return rt().mode === 'show'; }

  /* Specs de paramètres (ISF & co) si l'app les publie dans l'état. */
  function specs() {
    var r = S.raw || {};
    return r.specs || r.param_specs || (S.show && (S.show.specs || S.show.param_specs)) || [];
  }

  /* ========================================================== DOM utils */

  function el(tag, attrs) {
    var n = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (k) {
        var v = attrs[k];
        if (v === null || v === undefined) { return; }
        if (k === 'class') { n.className = v; }
        else if (k.slice(0, 2) === 'on' && typeof v === 'function') { n.addEventListener(k.slice(2), v); }
        else if (k === 'value') { n.value = v; }
        /* disabled/checked : PROPRIÉTÉS booléennes — setAttribute('disabled',
           'false') laisserait le contrôle désactivé (attribut présent =
           désactivé, quelle que soit sa valeur). Bug historique : tous les
           boutons conditionnels (« Assigner », « Dupliquer »…) restaient morts. */
        else if (k === 'checked') { n.checked = !!v; }
        else if (k === 'disabled') { n.disabled = !!v; }
        /* Attributs rendus TELS QUELS par le navigateur : traduits ici.
           `data-tip` ne l'est pas — les infobulles maison sont traduites à
           l'affichage (installTooltips), ce qui couvre aussi celles posées
           en dur dans index.html. */
        else if (TRANSLATED_ATTRS[k]) { n.setAttribute(k, tr(String(v))); }
        else { n.setAttribute(k, v); }
      });
    }
    for (var i = 2; i < arguments.length; i++) {
      appendChild(n, arguments[i]);
    }
    return n;
  }

  var TRANSLATED_ATTRS = { title: 1, placeholder: 1, 'aria-label': 1, alt: 1 };

  /* Point de passage UNIQUE du texte de l'interface : tout ce que `el()`
     reçoit en enfant textuel passe ici, donc par `tr()`. Une valeur absente
     du catalogue (nom de cue, chemin de média, nombre) ressort intacte. */
  function appendChild(n, c) {
    if (c === null || c === undefined || c === false) { return; }
    if (Array.isArray(c)) { c.forEach(function (x) { appendChild(n, x); }); return; }
    n.appendChild(typeof c === 'object' ? c : document.createTextNode(tr(String(c))));
  }

  function byId(id) { return document.getElementById(id); }
  function clamp(v, a, b) { return Math.min(b, Math.max(a, v)); }
  function fmtF(v, d) { return (typeof v === 'number' && isFinite(v)) ? v.toFixed(d === undefined ? 2 : d) : '—'; }

  /* Icônes SVG inline (statiques, aucune ressource externe). */
  var ICONS = {
    list: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/></svg>',
    film: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="4.5" width="19" height="15" rx="2"/><path d="M7 4.5v15M17 4.5v15M2.5 9.5H7M2.5 14.5H7M17 9.5h4.5M17 14.5h4.5"/></svg>',
    sparkle: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3.5l2 5.5 5.5 2-5.5 2-2 5.5-2-5.5L4.5 11 10 9z"/><path d="M19 16.5l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z"/></svg>',
    wave: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 12h4l3-8 5 16 3-8h3.5"/></svg>',
    plug: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9 7.5V3.5M15 7.5V3.5M7 7.5h10v3.5a5 5 0 0 1-10 0zM12 16v4.5"/></svg>',
    screen: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="4" width="19" height="13" rx="2"/><path d="M8 20.5h8M12 17v3.5"/></svg>',
    /* icône « animer ce paramètre » (flèche circulaire, façon Resolume) */
    anim: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 12a8 8 0 1 1-2.34-5.66"/><path d="M20 3v4h-4"/></svg>'
  };

  /* Mini-icônes des formes d'onde LFO (SVG inline, 26×14). */
  var WAVE_ICONS = {
    sine: '<svg viewBox="0 0 26 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M1 7 C3 1 5 1 7 7 C9 13 11 13 13 7 C15 1 17 1 19 7 C21 13 23 13 25 7"/></svg>',
    tri: '<svg viewBox="0 0 26 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1 11 L7 3 L13 11 L19 3 L25 11"/></svg>',
    square: '<svg viewBox="0 0 26 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1 11 H5 V3 H13 V11 H21 V3 H25"/></svg>',
    saw: '<svg viewBox="0 0 26 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1 11 L9 3 V11 L17 3 V11 L25 3"/></svg>',
    random_sh: '<svg viewBox="0 0 26 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1 8 H6 V4 H11 V12 H16 V6 H21 V9 H25"/></svg>',
    drift: '<svg viewBox="0 0 26 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M1 8 C3 4 5 11 8 8 C10 6 12 3 15 7 C17 10 20 4 22 7 C23 8.5 24 8 25 7"/></svg>'
  };

  /* État vide sympathique : icône + message + action directe optionnelle
     ({ label, onclick } — masquée en mode Show). */
  function emptyState(icon, text, action) {
    var ic = el('span', { class: 'empty-icon', 'aria-hidden': 'true' });
    ic.innerHTML = ICONS[icon] || '';
    return el('div', { class: 'empty-state' }, ic, el('span', null, text),
      (action && !isShowMode())
        ? el('button', { class: 'edit-only', onclick: action.onclick }, action.label)
        : null);
  }

  /* ================================================ numéros de cue (millièmes) */

  function cnStr(n) {
    if (n === null || n === undefined) { return '—'; }
    var i = Math.floor(n / 1000), f = n % 1000;
    if (!f) { return String(i); }
    return i + '.' + String(f).padStart(3, '0').replace(/0+$/, '');
  }

  function cnParse(s) {
    s = String(s === undefined ? '' : s).trim();
    if (!/^\d+(\.\d{1,3})?$/.test(s)) { return null; }
    var parts = s.split('.');
    var frac = (parts[1] || '') + '000';
    return parseInt(parts[0], 10) * 1000 + parseInt(frac.slice(0, 3), 10);
  }

  /* ============================================== helpers du modèle JSON */

  /* Timecode "HH:MM:SS:FF" — miroir de TcTime::from_str côté Rust :
     4 champs de 1-2 chiffres séparés par ':' (ou ';' drop-frame toléré),
     bornes 23/59/59/59. Retourne la forme normalisée "HH:MM:SS:FF",
     null si vide (= cue manuelle), undefined si invalide. */
  function tcParse(txt) {
    var raw = String(txt || '').trim();
    if (!raw) { return null; }
    var parts = raw.split(/[:;]/);
    if (parts.length !== 4) { return undefined; }
    var max = [23, 59, 59, 59];
    var out = [];
    for (var i = 0; i < 4; i++) {
      if (!/^\d{1,2}$/.test(parts[i])) { return undefined; }
      var v = parseInt(parts[i], 10);
      if (v > max[i]) { return undefined; }
      out.push((v < 10 ? '0' : '') + v);
    }
    return out.join(':');
  }

  /* Cadence TcRate (serde snake_case) → libellé humain (i/s). « 29,97 DF »
     passe par `tr` : la virgule décimale devient un point en anglais. */
  var TC_RATES = { fps24: '24', fps25: '25', fps2997_df: '29,97 DF', fps30: '30' };
  function tcRateLabel(r) { return tr(TC_RATES[r] || String(r || '?')); }

  function followKind(f) {
    if (typeof f === 'string') { return f; }
    if (f && typeof f === 'object' && 'wait' in f) { return 'wait'; }
    return 'manual';
  }
  function followWait(f) {
    return (f && typeof f === 'object' && typeof f.wait === 'number') ? f.wait : 2;
  }

  function contentLabel(c) {
    if (!c || c === 'none') { return '—'; }
    if (typeof c === 'object') {
      if ('media' in c) {
        var m = medias().find(function (x) { return x.id === c.media; });
        return trf('Média : {0}', m ? m.name : ('#' + c.media));
      }
      if ('material' in c) {
        var mt = materials().find(function (x) { return x.id === c.material; });
        return trf('Matériau : {0}', mt ? mt.name : ('#' + c.material));
      }
      if ('pattern' in c) { return trf('Mire : {0}', c.pattern); }
      if ('color' in c) { return 'Couleur'; }
    }
    return '?';
  }

  function defaultPlayback() { return { in_s: 0, out_s: null, speed: 1, end: 'loop' }; }

  /* Mires disponibles (PatternKind serde snake_case — variantes additives
     grid4/grid16/color_bars comprises). */
  var PATTERNS = [
    ['ident', 'Identification'],
    ['grid', 'Grille'],
    ['grid4', 'Grille 4'],
    ['grid16', 'Grille 16'],
    ['checker', 'Damier'],
    ['bars', 'Barres'],
    ['color_bars', 'Barres SMPTE']
  ];

  /* Palette de couleurs de cue (pastille dans la conduite) + « sans ». */
  var CUE_COLORS = ['#e5484d', '#f0b232', '#43c47f', '#4da3ff', '#c678dd', '#ff8fab', null];

  function newCue(number) {
    return {
      number: number, name: trf('Cue {0}', cnStr(number)), color: null, notes: '', armed: true,
      transition: { kind: 'crossfade', dur_s: 1.0, curve: 'linear' },
      follow: 'manual', goto_after: null, states: [], mod_routes: [],
      triggers: { midi_note: null, osc: null, timecode: null }
    };
  }

  function nextFreeCueNumber() {
    var list = cues();
    if (!list.length) { return 1000; }
    return list[list.length - 1].number + 1000;
  }

  /* Adresses de paramètres connues (cibles de modulation / patch). */
  function paramAddrs() {
    var a = ['master/intensity', 'master/dbo', 'bpm'];
    slices().forEach(function (s) {
      ['opacity', 'gain/r', 'gain/g', 'gain/b', 'gamma', 'blendmode',
        'media/speed', 'media/position'].forEach(function (p) {
          a.push('slice/' + s.id + '/' + p);
        });
    });
    specs().forEach(function (sp) {
      if (sp && sp.addr && a.indexOf(sp.addr) < 0) { a.push(sp.addr); }
    });
    return a;
  }

  /* ================================================= envoi de commandes */

  function sendCmd(cmd) {
    if (!Conduite.ws.send({ type: 'cmd', cmd: cmd })) {
      uiError('Hors ligne — commande perdue : ' + (cmd && cmd.cmd));
      return false;
    }
    return true;
  }

  function sendEdit(op) {
    if (isShowMode()) {
      uiWarn(trf('Mode Show verrouillé — édition refusée ({0})', op && op.op));
      return false;
    }
    var c = { cmd: 'edit' };
    Object.keys(op).forEach(function (k) { c[k] = op[k]; });
    var ok = sendCmd(c);
    if (ok) { trackEdit(op); }
    return ok;
  }

  function sendParam(addr, value, live) {
    return sendCmd({ cmd: 'param_set', addr: addr, value: value, source: 'ui' });
  }

  /* GO : anti double-GO — reflet UI du verrou de session
     (show.settings.min_go_interval_ms, défaut 300 ms). Pendant le délai :
     bouton grisé + jauge d'attente, Espace et clics ignorés, quel que soit
     le chemin (touche qui accroche, double événement). Le VRAI verrou est
     dans session (toutes sources : UI/OSC/MIDI/MSC) — ici on évite juste
     d'envoyer des commandes vouées au refus et on rend le délai visible. */
  var GO = { lockUntil: 0, timer: null };

  function goIntervalMs() {
    var v = settings().min_go_interval_ms;
    return (typeof v === 'number' && isFinite(v) && v >= 0) ? v : 300;
  }

  function goLocked() { return Date.now() < GO.lockUntil; }

  /* Applique/retire l'état visuel du délai sur le bouton GO (si présent). */
  function goCooldownVisual() {
    var btn = byId('go-btn');
    if (GO.timer) { clearTimeout(GO.timer); GO.timer = null; }
    var remaining = GO.lockUntil - Date.now();
    if (remaining <= 0) {
      if (btn) {
        btn.disabled = false;
        btn.classList.remove('cooldown');
      }
      return;
    }
    if (btn && !btn.classList.contains('cooldown')) {
      btn.disabled = true;
      btn.style.setProperty('--go-wait', remaining + 'ms');
      btn.classList.add('cooldown');
    }
    GO.timer = setTimeout(goCooldownVisual, remaining + 15);
  }

  function go() {
    if (goLocked()) { return; }
    GO.lockUntil = Date.now() + goIntervalMs();
    sendCmd({ cmd: 'cue_go' });
    goCooldownVisual();
  }
  function back() { sendCmd({ cmd: 'cue_back' }); }

  /* ============================================== panic universel (Échap)
     Convention QLab : Échap déclenche TOUJOURS le panic — simple appui =
     fondu (settings.panic_fade_s, défaut 2 s), double appui < 600 ms =
     arrêt sec. Jamais désactivé, même en mode Show. Dans un champ de
     saisie, le premier Échap sort du champ, le deuxième déclenche. */
  var ESC = { lastTs: 0 };

  function panicFadeS() {
    var v = settings().panic_fade_s;
    return (typeof v === 'number' && isFinite(v) && v >= 0) ? v : 2.0;
  }

  function panicFlash() {
    /* flash de bordure rouge plein écran — relançable sur double appui */
    document.body.classList.remove('panic-flash');
    void document.body.offsetWidth;   /* reflow : redémarre l'animation */
    document.body.classList.add('panic-flash');
    setTimeout(function () { document.body.classList.remove('panic-flash'); }, 700);
  }

  function escPanic() {
    var now = Date.now();
    var hard = now - ESC.lastTs < 600;
    ESC.lastTs = now;
    var fade = hard ? 0 : panicFadeS();
    sendCmd({ cmd: 'cue_panic', fade_s: fade });
    panicFlash();
    pushLog('warn', 'ui', hard
      ? 'PANIC — arrêt immédiat (double Échap)'
      : trf('PANIC — fondu {0} s (Échap)', fmtF(fade, 1)));
  }

  function dboToggle() {
    if (rt().dbo) { sendCmd({ cmd: 'dbo_release' }); }
    else { sendCmd({ cmd: 'dbo', fade_s: 0.0 }); }
  }

  /* Touche B : même garde-fou que le bouton DBO — jamais de blackout sur un
     simple keydown. Maintien 400 ms OU double frappe rapprochée, avec
     feedback visuel d'armement sur le bouton ; l'auto-repeat est ignoré. */
  var DBOKEY = { holdTimer: null, lastTap: 0, fired: false };

  function dboArmVisual(on) {
    var btn = byId('dbo-btn');
    if (btn) {
      btn.classList.toggle('arming', on);
      btn.classList.toggle('arming-key', on);
    }
  }

  function dboKeyDown() {
    if (DBOKEY.holdTimer) { return; }
    var now = Date.now();
    if (now - DBOKEY.lastTap < 400) {          /* double frappe : déclenche */
      DBOKEY.lastTap = 0;
      DBOKEY.fired = true;
      dboToggle();
      return;
    }
    DBOKEY.fired = false;
    dboArmVisual(true);
    DBOKEY.holdTimer = setTimeout(function () { /* maintien 400 ms : déclenche */
      DBOKEY.holdTimer = null;
      dboArmVisual(false);
      DBOKEY.fired = true;
      dboToggle();
    }, 400);
  }

  function dboKeyUp() {
    var wasArming = !!DBOKEY.holdTimer;
    if (DBOKEY.holdTimer) { clearTimeout(DBOKEY.holdTimer); DBOKEY.holdTimer = null; }
    dboArmVisual(false);
    /* un appui court (relâché avant 400 ms) compte comme première frappe */
    DBOKEY.lastTap = (wasArming && !DBOKEY.fired) ? Date.now() : 0;
    DBOKEY.fired = false;
  }

  /* ================================================================ toasts
     Retour transitoire pour TOUTES les erreurs et confirmations (plus
     d'échec silencieux relégué à l'onglet Journal). Empilés (4 max),
     4 s à l'écran, minuteur en pause au survol. Jamais bloquant. */

  function toast(msg, level) {
    var host = byId('toasts');
    if (!host) {
      host = el('div', { id: 'toasts', 'aria-live': 'polite' });
      document.body.appendChild(host);
    }
    var t = el('div', { class: 'toast' + (level ? ' ' + level : '') }, msg);
    host.appendChild(t);
    while (host.children.length > 4) { host.removeChild(host.firstChild); }
    var remaining = 4000, shownAt = Date.now(), timer = null;
    function close() {
      timer = null;
      t.classList.add('out');
      setTimeout(function () { if (t.parentNode) { t.parentNode.removeChild(t); } }, 350);
    }
    function arm() { shownAt = Date.now(); timer = setTimeout(close, remaining); }
    t.addEventListener('mouseenter', function () {
      if (timer) { clearTimeout(timer); timer = null; }
      remaining = Math.max(800, remaining - (Date.now() - shownAt));
    });
    t.addEventListener('mouseleave', function () {
      if (!timer && !t.classList.contains('out')) { arm(); }
    });
    arm();
  }

  /* Avertissement / erreur / info VISIBLES : journal + toast. */
  function uiWarn(msg) { pushLog('warn', 'ui', msg); toast(msg, 'warn'); }
  function uiError(msg) { pushLog('error', 'ui', msg); toast(msg, 'err'); }
  function uiInfo(msg) { pushLog('info', 'ui', msg); toast(msg); }

  /* ============================== micro-UX des curseurs (façon Resolume)
     Sur TOUT slider : clic sur la valeur = saisie clavier exacte (Entrée
     valide, Échap annule), double-clic = valeur par défaut, Maj+glisser =
     précision fine (course ×10). La nouvelle valeur repasse par les
     événements natifs input/change : chaque site garde sa plomberie. */

  var SLIDER_TIP = 'Clic sur la valeur : saisie exacte · Double-clic : valeur par défaut · Maj+glisser : précision fine';

  function sliderSet(input, v, fireChange) {
    var min = parseFloat(input.min), max = parseFloat(input.max);
    if (isFinite(min) && isFinite(max)) { v = clamp(v, min, max); }
    input.value = v;
    input.dispatchEvent(new Event('input', { bubbles: false }));
    if (fireChange !== false) { input.dispatchEvent(new Event('change', { bubbles: false })); }
    return v;
  }

  /* input : <input type=range> ; valEl : élément affichant la valeur (peut
     être null) ; def : valeur par défaut (double-clic) ; xf : conversion
     optionnelle valeur interne <-> valeur saisie ({ out, in }, ex. master
     affiché en %). */
  function enhanceSlider(input, valEl, def, xf) {
    var toEdit = (xf && xf.out) ? xf.out : function (v) { return v; };
    var fromEdit = (xf && xf['in']) ? xf['in'] : function (v) { return v; };
    var tip = input.getAttribute('data-tip');
    input.setAttribute('data-tip', (tip ? tr(tip) + ' — ' : '') + tr(SLIDER_TIP));

    function reset() {
      if (def === undefined || def === null) { return; }
      sliderSet(input, def);
    }
    input.addEventListener('dblclick', reset);

    /* Maj+glisser : on prend la main sur le drag natif (course ×10) */
    var fine = null;
    input.addEventListener('pointerdown', function (e) {
      if (!e.shiftKey || e.button !== 0) { return; }
      e.preventDefault();
      fine = { x: e.clientX, v: parseFloat(input.value) || 0 };
      try { input.setPointerCapture(e.pointerId); } catch (err) { /* indisponible */ }
    });
    input.addEventListener('pointermove', function (e) {
      if (!fine) { return; }
      var min = parseFloat(input.min), max = parseFloat(input.max);
      var span = (isFinite(min) && isFinite(max)) ? (max - min) : 1;
      var w = Math.max(40, input.getBoundingClientRect().width);
      sliderSet(input, fine.v + (e.clientX - fine.x) / w * span * 0.1, false);
    });
    function fineEnd() {
      if (!fine) { return; }
      fine = null;
      input.dispatchEvent(new Event('change', { bubbles: false }));
    }
    input.addEventListener('pointerup', fineEnd);
    input.addEventListener('pointercancel', fineEnd);

    if (!valEl) { return; }
    valEl.setAttribute('data-tip', tr(SLIDER_TIP));
    valEl.addEventListener('dblclick', reset);
    valEl.addEventListener('click', function () {
      if (valEl.getAttribute('data-editing')) { return; }
      valEl.setAttribute('data-editing', '1');
      var old = valEl.textContent;
      var cur = toEdit(parseFloat(input.value));
      var ed = el('input', {
        class: 'val-edit', type: 'text',
        value: String(Math.round(cur * 1000) / 1000)
      });
      valEl.textContent = '';
      valEl.appendChild(ed);
      var done = false;
      function finish(commit) {
        if (done) { return; }
        done = true;
        var v = parseFloat(String(ed.value).replace(',', '.'));
        valEl.removeAttribute('data-editing');
        if (ed.parentNode === valEl) { valEl.removeChild(ed); }
        valEl.textContent = old;   /* input/change re-mettent à jour derrière */
        if (commit && isFinite(v)) { sliderSet(input, fromEdit(v)); }
      }
      ed.addEventListener('keydown', function (e) {
        e.stopPropagation();   /* pas de GO/panic global pendant la saisie */
        if (e.key === 'Enter') { finish(true); }
        else if (e.key === 'Escape') { finish(false); }
      });
      ed.addEventListener('blur', function () { finish(true); });
      ed.focus();
      ed.select();
    });
  }

  /* ============================================ confirmation destructive
     Dialogue sobre : Entrée = confirmer, Échap = annuler (l'Échap y est
     consommé par le dialogue — pas de panic depuis un dialogue). Les
     actions destructives sont de toute façon refusées en mode Show. */

  var CONFIRM = { el: null, onConfirm: null, onCancel: null };

  function confirmOpen() { return !!CONFIRM.el; }

  function closeConfirm(ok) {
    var box = CONFIRM.el;
    if (!box) { return; }
    var cb = ok ? CONFIRM.onConfirm : CONFIRM.onCancel;
    CONFIRM.el = null;
    CONFIRM.onConfirm = null;
    CONFIRM.onCancel = null;
    if (box.parentNode) { box.parentNode.removeChild(box); }
    if (typeof cb === 'function') { cb(); }
  }

  /* opts : { title, message, confirm, cancel, danger=true, onConfirm, onCancel } */
  function confirmDialog(opts) {
    closeConfirm(false);
    var confirmBtn = el('button', {
      class: opts.danger === false ? 'primary' : 'danger',
      onclick: function () { closeConfirm(true); }
    }, opts.confirm || 'Confirmer', el('kbd', null, 'Entrée'));
    var cancelBtn = el('button', {
      onclick: function () { closeConfirm(false); }
    }, opts.cancel || 'Annuler', el('kbd', null, 'Échap'));
    var overlay = el('div', { id: 'confirm-overlay' },
      el('div', { class: 'confirm-box', role: 'dialog', 'aria-modal': 'true' },
        el('div', { class: 'confirm-title' }, opts.title || 'Confirmer ?'),
        opts.message ? el('div', { class: 'confirm-msg' }, opts.message) : null,
        el('div', { class: 'confirm-actions' }, cancelBtn, confirmBtn)));
    overlay.addEventListener('pointerdown', function (e) {
      if (e.target === overlay) { closeConfirm(false); }
    });
    CONFIRM.el = overlay;
    CONFIRM.onConfirm = opts.onConfirm || null;
    CONFIRM.onCancel = opts.onCancel || null;
    document.body.appendChild(overlay);
    confirmBtn.focus();
  }

  /* ============================================== undo/redo (Ctrl+Z/Maj+Z)
     Les commandes Undo/Redo vivent dans session (pile de snapshots, mode
     Edit uniquement) ; on garde ici un historique local des LIBELLÉS pour
     nommer l'action dans le toast (« Annulé : suppression de cue »). */

  var EDITS = { done: [], undone: [] };
  var EDITS_CAP = 100;   /* même cap que la pile session (UNDO_CAP) */

  var OP_LABELS = {
    slice_add: 'ajout de slice', slice_remove: 'suppression de slice',
    slice_update: 'modification de slice', corner_set: 'déplacement de coin',
    output_add: 'ajout de sortie', output_remove: 'suppression de sortie',
    output_update: 'modification de sortie',
    cue_add: 'ajout de cue', cue_remove: 'suppression de cue',
    cue_update: 'modification de cue', cue_update_state: 'assignation de contenu',
    media_add: 'ajout de média', media_remove: 'suppression de média',
    media_update: 'modification de média',
    material_add: 'ajout de matériau', material_remove: 'suppression de matériau',
    material_update: 'modification de matériau',
    modulator_add: 'ajout de modulateur', modulator_remove: 'suppression de modulateur',
    modulator_update: 'modification de modulateur',
    route_add: 'ajout d’animation', route_remove: 'suppression d’animation',
    route_update: 'modification d’animation',
    patch_artnet_add: 'ajout de patch Art-Net', patch_artnet_remove: 'suppression de patch Art-Net',
    patch_artnet_update: 'modification de patch Art-Net',
    patch_midi_add: 'ajout de binding MIDI', patch_midi_remove: 'suppression de binding MIDI',
    patch_midi_update: 'modification de binding MIDI',
    patch_osc_out_set: 'réglage OSC sortant',
    key_binding_add: 'ajout de raccourci clavier', key_binding_remove: 'suppression de raccourci clavier',
    show_rename: 'renommage du show', settings_update: 'modification des réglages'
  };

  function opLabel(op) {
    return (op && op.op && OP_LABELS[op.op]) || (op && op.op) || 'édition';
  }

  function trackEdit(op) {
    EDITS.done.push(opLabel(op));
    if (EDITS.done.length > EDITS_CAP) { EDITS.done.shift(); }
    EDITS.undone.length = 0;   /* toute édition invalide le redo (comme session) */
  }

  function uiUndo() {
    if (isShowMode()) {
      uiWarn('Mode Show verrouillé — annulation désactivée.');
      return;
    }
    if (!sendCmd({ cmd: 'undo' })) { return; }
    var label = EDITS.done.pop();
    if (label) { EDITS.undone.push(label); }
    toast(label ? trf('Annulé : {0}', tr(label)) : 'Annulation demandée');
  }

  function uiRedo() {
    if (isShowMode()) {
      uiWarn('Mode Show verrouillé — rétablissement désactivé.');
      return;
    }
    if (!sendCmd({ cmd: 'redo' })) { return; }
    var label = EDITS.undone.pop();
    if (label) { EDITS.done.push(label); }
    toast(label ? trf('Rétabli : {0}', tr(label)) : 'Rétablissement demandé');
  }

  /* Cue cible pour les assignations de contenu : standby, sinon active. */
  function targetCueNumber() {
    var r = rt();
    if (r.standby !== null && r.standby !== undefined) { return r.standby; }
    if (r.active !== null && r.active !== undefined) { return r.active; }
    var list = cues();
    return list.length ? list[0].number : null;
  }

  /* `quiet` : les appels en rafale (mire globale, éteindre, identifier)
     affichent UN toast récapitulatif côté appelant au lieu d'un par slice. */
  function assignContent(sliceId, content, playback, quiet) {
    var n = targetCueNumber();
    if (n === null) {
      uiWarn('Aucune cue cible (standby/active) pour l’assignation.');
      return false;
    }
    var ok = sendEdit({
      op: 'cue_update_state', number: n,
      state: { slice: sliceId, content: content, playback: playback || null, params: {} }
    });
    if (ok && !quiet) {
      var slice = slices().find(function (s) { return s.id === sliceId; });
      var sname = slice ? slice.name : trf('slice {0}', sliceId);
      var what = (!content || content === 'none') ? tr('Contenu retiré') : contentLabel(content);
      toast(trf('{0} → {1} (cue {2})', what, sname, cnStr(n)), 'ok');
    }
    return ok;
  }

  /* ============================================ application locale des EditOp
     Miroir de EditOp::apply côté Rust — garde S.show en phase sans re-hello. */

  function applyOp(o) {
    var sh = S.show;
    if (!sh || !o || !o.op) { return; }
    function upd(list, key, item) {
      if (!list) { return; }
      for (var i = 0; i < list.length; i++) {
        if (list[i][key] === item[key]) { list[i] = item; return; }
      }
    }
    function rm(list, key, id) {
      if (!list) { return; }
      for (var i = list.length - 1; i >= 0; i--) {
        if (list[i][key] === id) { list.splice(i, 1); }
      }
    }
    switch (o.op) {
      case 'slice_add': (sh.slices = sh.slices || []).push(o.slice); break;
      case 'slice_remove': rm(sh.slices, 'id', o.id); break;
      case 'slice_update': upd(sh.slices, 'id', o.slice); break;
      case 'corner_set': {
        var s = slices().find(function (x) { return x.id === o.slice; });
        if (s && s.corners && s.corners[o.index]) { s.corners[o.index] = [o.x, o.y]; }
        break;
      }
      case 'output_add': (sh.outputs = sh.outputs || []).push(o.output); break;
      case 'output_remove': rm(sh.outputs, 'id', o.id); break;
      case 'output_update': upd(sh.outputs, 'id', o.output); break;
      case 'cue_add': {
        var list = sh.cues = sh.cues || [];
        var i = list.findIndex(function (c) { return c.number >= o.cue.number; });
        if (i < 0) { list.push(o.cue); }
        else if (list[i].number === o.cue.number) { list[i] = o.cue; }
        else { list.splice(i, 0, o.cue); }
        break;
      }
      case 'cue_remove': rm(sh.cues, 'number', o.number); break;
      case 'cue_update': upd(sh.cues, 'number', o.cue); break;
      case 'cue_update_state': {
        var c = cues().find(function (x) { return x.number === o.number; });
        if (c) {
          c.states = c.states || [];
          var j = c.states.findIndex(function (st) { return st.slice === o.state.slice; });
          if (j >= 0) { c.states[j] = o.state; } else { c.states.push(o.state); }
        }
        break;
      }
      case 'media_add': (sh.media = sh.media || []).push(o.media); break;
      case 'media_remove': rm(sh.media, 'id', o.id); break;
      case 'media_update': upd(sh.media, 'id', o.media); break;
      case 'material_add': (sh.materials = sh.materials || []).push(o.material); break;
      case 'material_remove': rm(sh.materials, 'id', o.id); break;
      case 'material_update': upd(sh.materials, 'id', o.material); break;
      case 'modulator_add': (sh.modulators = sh.modulators || []).push(o.modulator); break;
      case 'modulator_remove': rm(sh.modulators, 'id', o.id); break;
      case 'modulator_update': upd(sh.modulators, 'id', o.modulator); break;
      case 'route_add': (sh.routes = sh.routes || []).push(o.route); break;
      case 'route_remove': rm(sh.routes, 'id', o.id); break;
      case 'route_update': upd(sh.routes, 'id', o.route); break;
      case 'patch_artnet_add': (patch().artnet = patch().artnet || []).push(o.entry); break;
      case 'patch_artnet_remove': if (patch().artnet) { patch().artnet.splice(o.index, 1); } break;
      case 'patch_artnet_update': if (patch().artnet && patch().artnet[o.index]) { patch().artnet[o.index] = o.entry; } break;
      case 'patch_midi_add': (patch().midi = patch().midi || []).push(o.binding); break;
      case 'patch_midi_remove': if (patch().midi) { patch().midi.splice(o.index, 1); } break;
      case 'patch_midi_update': if (patch().midi && patch().midi[o.index]) { patch().midi[o.index] = o.binding; } break;
      case 'patch_osc_out_set': patch().osc_out = o.cfg; break;
      case 'key_binding_add': (patch().keys = patch().keys || []).push(o.binding); break;
      case 'key_binding_remove': if (patch().keys) { patch().keys.splice(o.index, 1); } break;
      case 'show_rename': sh.name = o.name; break;
      case 'settings_update': sh.settings = o.settings; break;
      default: break;
    }
  }

  /* ================================================= système d'infobulles */

  var tipTimer = null;

  function hideTip() {
    var t = byId('tooltip');
    if (t) { t.classList.add('hidden'); }
  }

  function installTooltips() {
    document.addEventListener('mouseover', function (e) {
      var target = e.target && e.target.closest ? e.target.closest('[data-tip]') : null;
      if (tipTimer) { clearTimeout(tipTimer); tipTimer = null; }
      if (!target) { hideTip(); return; }
      tipTimer = setTimeout(function () {
        var tip = byId('tooltip');
        if (!tip || !document.contains(target)) { return; }
        tip.textContent = tr(target.getAttribute('data-tip') || '');
        tip.classList.remove('hidden', 'above', 'below');
        tip.style.left = '0px'; tip.style.top = '0px';
        var r = target.getBoundingClientRect();
        var tw = tip.offsetWidth, th = tip.offsetHeight;
        var x = clamp(r.left, 8, window.innerWidth - tw - 8);
        var y = r.bottom + 8;
        var side = 'below';
        if (y + th > window.innerHeight - 8) { y = r.top - th - 8; side = 'above'; }
        tip.classList.add(side);
        tip.style.left = x + 'px';
        tip.style.top = Math.max(8, y) + 'px';
      }, 400);
    });
    document.addEventListener('mouseout', function () {
      if (tipTimer) { clearTimeout(tipTimer); tipTimer = null; }
      hideTip();
    });
    document.addEventListener('mousedown', hideTip);
  }

  /* ======================================================== menu contextuel
     UN composant réutilisable pour tous les clics droits (cue, slice, média,
     paramètre). Entrées : {kind:'head'|'sep'|'swatches'|item}. Un item
     désactivé est grisé avec la RAISON en infobulle. Fermeture : Échap,
     clic ailleurs, action choisie. Position clampée au viewport. */

  var CTX = { el: null, x: 0, y: 0 };

  function closeCtxMenu() {
    if (!CTX.el) { return false; }
    if (CTX.el.parentNode) { CTX.el.parentNode.removeChild(CTX.el); }
    CTX.el = null;
    return true;
  }

  function openCtxMenu(x, y, items) {
    closeCtxMenu();
    CTX.x = x; CTX.y = y;
    var menu = el('div', { class: 'ctx-menu', role: 'menu' });
    (items || []).forEach(function (it) {
      if (!it) { return; }
      if (it.kind === 'sep') { menu.appendChild(el('div', { class: 'ctx-sep' })); return; }
      if (it.kind === 'head') { menu.appendChild(el('div', { class: 'ctx-head' }, it.label)); return; }
      if (it.kind === 'swatches') {
        var row = el('div', { class: 'ctx-swatches' });
        (it.colors || []).forEach(function (c) {
          var sw = el('span', {
            class: 'ctx-swatch' + (c ? '' : ' none'),
            role: 'menuitem',
            style: c ? 'background:' + c : null,
            'data-tip': c || 'Sans couleur'
          });
          sw.addEventListener('click', function () {
            closeCtxMenu();
            if (it.pick) { it.pick(c); }
          });
          row.appendChild(sw);
        });
        menu.appendChild(row);
        return;
      }
      var node = el('div', {
        class: 'ctx-item' + (it.danger ? ' danger' : '') + (it.disabled ? ' disabled' : ''),
        role: 'menuitem',
        'aria-disabled': it.disabled ? 'true' : null,
        'data-tip': it.disabled ? (it.reason || 'Indisponible') : (it.tip || null)
      }, it.label, it.sub ? el('span', { class: 'ctx-sub-arrow' }, '▸') : null);
      if (!it.disabled) {
        node.addEventListener('click', function () {
          if (it.sub) {
            /* sous-menu : remplace le menu courant, même position */
            openCtxMenu(CTX.x, CTX.y, it.sub());
            return;
          }
          closeCtxMenu();
          if (it.action) { it.action(); }
        });
      }
      menu.appendChild(node);
    });
    document.body.appendChild(menu);
    var mw = menu.offsetWidth, mh = menu.offsetHeight;
    menu.style.left = clamp(x, 8, Math.max(8, window.innerWidth - mw - 8)) + 'px';
    menu.style.top = clamp(y, 8, Math.max(8, window.innerHeight - mh - 8)) + 'px';
    CTX.el = menu;
  }

  /* Branche un menu contextuel sur un nœud. `build(e)` retourne la liste
     d'entrées (ou null/[] pour laisser le menu natif). */
  function onCtx(node, build) {
    node.addEventListener('contextmenu', function (e) {
      var items = build(e);
      if (!items || !items.length) { return; }
      e.preventDefault();
      e.stopPropagation();
      openCtxMenu(e.clientX, e.clientY, items);
    });
  }

  /* Marque une entrée « édition » : grisée avec raison en mode Show. */
  function ctxEdit(it) {
    if (isShowMode()) {
      it.disabled = true;
      it.reason = 'Mode Show — édition verrouillée';
    }
    return it;
  }

  function installCtxMenuClose() {
    document.addEventListener('pointerdown', function (e) {
      if (CTX.el && !CTX.el.contains(e.target)) { closeCtxMenu(); }
    });
    window.addEventListener('resize', closeCtxMenu);
  }

  /* ======================================================== onglets & rendu */

  /* `short` : libellé condensé pour la nav tablette (≤ 900 px). */
  var TABS = [
    { id: 'live', label: 'Live', short: 'Live', tip: 'Conduite en jeu : cuelist, GO, préviews, master, DBO. Raccourci : 1' },
    { id: 'cues', label: 'Cues', short: 'Cues', tip: 'Édition de la cuelist : numéros, transitions, follow, notes. Raccourci : 2' },
    { id: 'mapping', label: 'Mapping', short: 'Map', tip: 'Calage des slices : coins, nudge clavier, mires. Raccourci : 3' },
    { id: 'medias', label: 'Médias', short: 'Méd', tip: 'Pool de médias : vignettes, assignation, re-scan. Raccourci : 4' },
    { id: 'materiaux', label: 'Matériaux', short: 'Mat', tip: 'Shaders ISF/GLSL : assignation et paramètres. Raccourci : 5' },
    { id: 'modulation', label: 'Modulation', short: 'Mod', tip: 'LFO, bandes audio, routes, tap tempo. Raccourci : 6' },
    { id: 'patch', label: 'Patch', short: 'Patch', tip: 'OSC, MIDI, Art-Net : bindings et adresses. Raccourci : 7' },
    { id: 'sorties', label: 'Sorties', short: 'Sort', tip: 'Écrans / projecteurs : résolution, plein écran, identification. Raccourci : 8' },
    { id: 'journal', label: 'Journal', short: 'Jrnl', tip: 'Logs du moteur en direct. Raccourci : 9' },
    { id: 'reglages', label: 'Réglages', short: 'Régl', tip: 'Show, ports, langue, mode Édition/Show. Raccourci : 0' }
  ];

  function visibleTabs() {
    return isShowMode() ? TABS.filter(function (t) { return t.id === 'live'; }) : TABS;
  }

  function setTab(id) {
    S.tab = id;
    renderTabs();
    renderMain();
  }

  function renderTabs() {
    var nav = byId('tabs');
    if (!nav) { return; }
    nav.textContent = '';
    visibleTabs().forEach(function (t, i) {
      var key = i === 9 ? '0' : String(i + 1);
      nav.appendChild(el('button', {
        class: t.id === S.tab ? 'active' : '', 'data-tip': t.tip,
        onclick: function () { setTab(t.id); }
      }, el('span', { class: 'tab-key' }, key),
        el('span', { class: 'tab-label-full' }, t.label),
        el('span', { class: 'tab-label-short' }, t.short || t.label)));
    });
  }

  var RENDERERS = {};

  function renderMain() {
    var main = byId('main');
    if (!main) { return; }
    main.textContent = '';
    var fn = RENDERERS[S.tab] || RENDERERS.live;
    try {
      main.appendChild(fn());
    } catch (e) {
      console.error('render', e);
      main.appendChild(el('div', { class: 'panel danger-text' }, trf('Erreur de rendu : {0}', e.message)));
    }
    updateDyn();
    updateHealth();
  }

  /* Re-render différé : si l'opérateur est en train de taper dans un champ
     (ici ou depuis un autre poste — portable + tablette en calage), un
     renderMain() immédiat détruirait le focus et la saisie. S.show est déjà
     à jour (applyOp) ; on re-rend au blur. */
  var renderPending = false;

  function requestRenderMain() {
    var main = byId('main');
    var ae = document.activeElement;
    var editing = ae && main && main.contains(ae) &&
      (ae.tagName === 'INPUT' || ae.tagName === 'TEXTAREA' || ae.tagName === 'SELECT' || ae.isContentEditable);
    if (editing) { renderPending = true; return; }
    renderPending = false;
    renderMain();
  }

  function installDeferredRender() {
    document.addEventListener('focusout', function () {
      if (!renderPending) { return; }
      /* laisser le focus se poser (Tab -> champ suivant) avant de re-tester */
      setTimeout(function () {
        if (renderPending) { requestRenderMain(); }
      }, 0);
    });
  }

  /* Aligne la langue de l'UI sur `settings.language`. Renvoie true si elle a
     changé — l'appelant doit alors re-rendre TOUTE la page (onglets, pied de
     page et chrome compris), pas seulement le panneau courant. */
  function syncLang() {
    if (!I18N) { return false; }
    var changed = I18N.setLang(settings().language || 'fr');
    if (!changed) { return false; }
    document.documentElement.setAttribute('lang', I18N.lang());
    /* Titre de repli seulement : dès la première trame `dyn`, updateTitle
       reprend la main (« ● Cue 12 — MonShow »). */
    document.title = tr('Conduite — régie vidéo');
    var banner = byId('banner');
    if (banner) { banner.textContent = tr('Reconnexion au moteur…'); }
    return true;
  }

  function renderAll() {
    syncLang();
    var name = byId('show-name');
    if (name) { name.textContent = show().name || ''; }
    document.body.classList.toggle('mode-show', isShowMode());
    var badge = byId('mode-badge');
    if (badge) { badge.textContent = isShowMode() ? tr('SHOW') : tr('ÉDITION'); }
    if (isShowMode() && S.tab !== 'live') { S.tab = 'live'; }
    renderTabs();
    renderMain();
  }

  /* ================================================================= LIVE */

  RENDERERS.live = function () {
    var root = el('section', { class: 'tab-panel', id: 'live' });

    /* --- cuelist --- */
    var listPanel = el('div', { class: 'panel', id: 'cuelist-panel' },
      el('h2', null, 'Conduite'),
      el('div', { id: 'rt-line', 'data-tip': 'Cue active / standby et temps restant' }, ''),
      cuelistDom());
    root.appendChild(listPanel);

    /* --- colonne droite --- */
    var right = el('div', { id: 'live-right' });

    right.appendChild(previewMonitor('PROGRAM', '/preview.mjpeg',
      'Préview program : ce qui sort réellement', true));
    right.appendChild(previewMonitor('PRÉVIEW (STANDBY)', '/preview-b.mjpeg',
      'Préview de la cue en standby, rendue à blanc', false));

    var gotoInput = el('input', {
      type: 'text', placeholder: 'n° de cue (ex. 12.5)',
      'data-tip': 'Numéro de cue pour GOTO — Entrée pour lancer'
    });
    gotoInput.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') { doGoto(gotoInput); }
    });

    right.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Transport'),
      el('div', { id: 'transport' },
        /* temps restant de la cue active, en grand : le chiffre que le
           régisseur surveille avant le prochain GO */
        el('div', {
          id: 'live-remaining', class: 'idle',
          'data-tip': 'Temps restant de la cue active (média ou attente) — ambre sous 10 s, rouge sous 5 s'
        },
          el('span', { class: 'lr-label' }, 'RESTE'),
          el('span', { class: 'lr-val' }, '—')),
        el('button', {
          id: 'go-btn', 'data-tip': 'GO : lance la cue en standby. Raccourci : Espace',
          onclick: go
        }, el('span', { class: 'go-label' }, 'GO'), el('kbd', null, 'Espace')),
        el('button', { 'data-tip': 'Back : revient à la cue précédente', onclick: back }, 'BACK'),
        el('button', {
          'data-tip': 'Standby sur la cue suivante sans la lancer',
          onclick: function () {
            var next = nextCueAfter(rt().standby !== null ? rt().standby : rt().active);
            if (next !== null) { sendCmd({ cmd: 'cue_standby', cue: next }); }
          }
        }, 'STANDBY +1'),
        el('div', { id: 'goto-row' },
          gotoInput,
          el('button', { 'data-tip': 'GOTO : saute directement à ce numéro de cue', onclick: function () { doGoto(gotoInput); } }, 'GOTO'))),
      /* notes de régie de la cue en standby — l'outil n°1 du régisseur
         remplaçant (« attendre le noir complet avant GO ») */
      el('div', {
        id: 'standby-notes', class: 'standby-notes empty',
        'data-tip': 'Notes de régie de la cue en standby. Touche O : éditer (mode édition).'
      })));

    /* panneau replié « Cues actives » : temps écoulé / restant, progression */
    right.appendChild(el('details', { id: 'active-cues' },
      el('summary', { 'data-tip': 'Cue(s) en cours de lecture : progression et temps restant' }, 'Cues actives'),
      el('div', { id: 'active-cues-list' })));

    var master = el('input', {
      type: 'range', min: 0, max: 1, step: 0.001, id: 'master-range',
      'data-tip': 'Master intensité : atténue toutes les sorties (0 = noir)'
    });
    master.value = rt().master;
    master.addEventListener('input', function () {
      var v = parseFloat(master.value);
      var lbl = byId('master-val');
      if (lbl) { lbl.textContent = Math.round(v * 100) + ' %'; }
      MASTER.until = Date.now() + 250;
      sendParam('master/intensity', { f: v });
    });
    /* interaction réelle uniquement : un <input type=range> garde le focus
       après un drag, tester activeElement figeait le slider face aux
       changements MIDI/OSC/DBO. */
    master.addEventListener('pointerdown', function () { MASTER.held = true; });
    master.addEventListener('pointerup', function () { MASTER.held = false; MASTER.until = Date.now() + 250; });
    master.addEventListener('pointercancel', function () { MASTER.held = false; MASTER.until = Date.now() + 250; });

    var dbo = el('button', {
      id: 'dbo-btn',
      'data-tip': 'DBO — blackout d’urgence. Double-clic ou maintien 600 ms (touche B). Re-déclencher pour relever.'
    }, el('span', { class: 'dbo-label' }, 'DBO'));
    installDbo(dbo);

    var masterVal = el('span', { id: 'master-val' }, Math.round(rt().master * 100) + ' %');
    /* saisie exacte en % (Entrée), double-clic = 100 %, Maj+glisser fin */
    enhanceSlider(master, masterVal, 1, {
      out: function (v) { return Math.round(v * 100); },
      'in': function (d) { return d / 100; }
    });

    right.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Master'),
      el('div', { id: 'master-row' },
        master,
        masterVal,
        animButton('master/intensity')),
      dbo));

    right.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Santé'),
      el('div', { id: 'health-line', 'data-tip': 'FPS par sortie, frames perdues, CPU, mémoire, température' }, 'En attente de données…')));

    root.appendChild(right);
    return root;
  };

  function previewImg(src, tip) {
    var img = el('img', {
      src: src, alt: 'préview', 'data-tip': tip,
      'data-src': src, class: 'preview-stream'
    });
    var fresh = { loads: 0, last: Date.now() };
    img.addEventListener('load', function () {
      /* en flux multipart, chaque frame déclenche 'load' (Chrome/Firefox) */
      fresh.loads++;
      fresh.last = Date.now();
      if (img.parentNode && img.parentNode.classList) {
        img.parentNode.classList.remove('stale');
      }
    });
    img.addEventListener('error', function () {
      setTimeout(function () {
        if (document.contains(img)) { img.src = src + '?r=' + Date.now(); }
      }, 2000);
    });
    /* Watchdog de fraîcheur : une fin de flux « propre » (moteur qui coupe
       le canal, proxy qui ferme) ne déclenche PAS 'error' — l'image gèle
       sur la dernière frame. Si le navigateur émet bien un 'load' par frame
       (>= 2 vus) et que plus rien n'arrive depuis 6 s : voile « préview
       perdue » + rechargement cache-busté. */
    var wd = setInterval(function () {
      if (!document.contains(img)) { clearInterval(wd); return; }
      if (fresh.loads >= 2 && Date.now() - fresh.last > 6000) {
        if (img.parentNode && img.parentNode.classList) {
          img.parentNode.classList.add('stale');
        }
        fresh.loads = 0;
        fresh.last = Date.now();
        img.src = src + '?r=' + Date.now();
      }
    }, 3000);
    return img;
  }

  /* ------------------------------------------- moniteur de préview complet
     Placeholder élégant tant qu'aucune frame n'est arrivée, puis flux MJPEG ;
     si `tryH264` et WebCodecs disponibles : tentative de flux H.264
     (WS /preview.h264), repli MJPEG automatique. Le mode réel du flux est
     indiqué dans l'infobulle du moniteur. */
  function previewMonitor(label, src, tip, tryH264) {
    var wrap = el('div', { class: 'preview-wrap empty' },
      el('span', { class: 'preview-label' }, label));
    var img = previewImg(src, tr(tip) + tr(' — flux MJPEG'));
    img.addEventListener('load', function () { wrap.classList.remove('empty'); });
    wrap.appendChild(img);
    var ph = el('div', { class: 'preview-empty' });
    var ic = el('span', { 'aria-hidden': 'true' });
    ic.innerHTML = ICONS.screen;
    ph.appendChild(ic);
    ph.appendChild(el('span', null, 'En attente du flux vidéo…'));
    wrap.appendChild(ph);
    if (tryH264) { attachH264(wrap, img, src, tip); }
    return wrap;
  }

  /* Frame Annex-B : true si elle contient un NAL IDR (keyframe H.264). */
  function annexbIsKey(u8) {
    for (var i = 0; i + 3 < u8.length; i++) {
      if (u8[i] === 0 && u8[i + 1] === 0 &&
          (u8[i + 2] === 1 || (u8[i + 2] === 0 && u8[i + 3] === 1))) {
        var off = u8[i + 2] === 1 ? i + 3 : i + 4;
        if (off >= u8.length) { break; }
        var t = u8[off] & 0x1f;
        if (t === 5) { return true; }   /* IDR */
        if (t === 1) { return false; }  /* premier VCL non-IDR : delta */
        i = off;                        /* SPS/PPS/SEI : NAL suivant */
      }
    }
    return false;
  }

  /* Un seul toast de repli H.264 par chargement de page (discret). */
  var H264 = { toasted: false };

  /* Préview H.264/WebCodecs : 1er message JSON {codec,format,width,height,
     fps} puis frames binaires Annex-B. Décodage vers un canvas qui remplace
     l'img MJPEG ; toute panne (pas de config, pas de frame décodée en 3 s,
     socket fermée, erreur décodeur) rebranche le MJPEG. */
  function attachH264(wrap, img, src, tip) {
    if (typeof window.VideoDecoder !== 'function' ||
        typeof window.EncodedVideoChunk !== 'function') { return; }
    var proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
    var ws;
    try { ws = new WebSocket(proto + location.host + '/preview.h264'); }
    catch (e) { return; }
    ws.binaryType = 'arraybuffer';
    var dec = null, cfg = null, canvas = null, cctx = null;
    var n = 0, gotKey = false, gotFrame = false, dead = false;
    var watchdog = null;

    function cleanup() {
      dead = true;
      if (watchdog) { clearTimeout(watchdog); watchdog = null; }
      try { ws.close(); } catch (e) { /* déjà fermée */ }
      if (dec) { try { dec.close(); } catch (e2) { /* déjà fermé */ } dec = null; }
    }

    /* Repli MJPEG. `msg` : toast discret (une seule fois par page). */
    function fail(msg) {
      if (dead) { return; }
      cleanup();
      if (canvas && canvas.parentNode) { canvas.parentNode.removeChild(canvas); }
      if (!img.parentNode) {
        /* l'img avait été retirée au profit du canvas : on la remet */
        wrap.classList.add('empty');
        img = previewImg(src, tr(tip) + tr(' — flux MJPEG'));
        img.addEventListener('load', function () { wrap.classList.remove('empty'); });
        wrap.insertBefore(img, wrap.querySelector('.preview-empty'));
      }
      img.setAttribute('data-tip', tr(tip) + tr(' — flux MJPEG'));
      if (msg && !H264.toasted) {
        H264.toasted = true;
        toast(msg);
        pushLog('info', 'ui', msg);
      }
    }

    /* le moniteur a quitté le DOM (changement d'onglet) : tout couper */
    var alive = setInterval(function () {
      if (!document.contains(wrap)) {
        clearInterval(alive);
        cleanup();
      }
    }, 2000);

    ws.addEventListener('error', function () { fail(null); });
    ws.addEventListener('close', function () {
      if (dead) { return; }
      fail(gotFrame ? 'Flux H.264 interrompu — retour au MJPEG.' : null);
    });
    ws.addEventListener('open', function () {
      /* aucune frame décodée en 3 s => repli + toast discret (contrat) */
      watchdog = setTimeout(function () {
        if (!gotFrame) { fail('Préview H.264 indisponible — affichage MJPEG.'); }
      }, 3000);
    });
    ws.addEventListener('message', function (ev) {
      if (dead) { return; }
      if (typeof ev.data === 'string') {
        /* 1er message : configuration du flux */
        try { cfg = JSON.parse(ev.data); } catch (e) { fail(null); return; }
        try {
          dec = new VideoDecoder({
            output: function (frame) {
              if (dead) { frame.close(); return; }
              if (!canvas) {
                canvas = el('canvas', {
                  class: 'preview-stream',
                  'data-tip': tr(tip) + tr(' — flux H.264 (WebCodecs)')
                });
                canvas.width = frame.displayWidth || (cfg && cfg.width) || 640;
                canvas.height = frame.displayHeight || (cfg && cfg.height) || 360;
                cctx = canvas.getContext('2d');
                wrap.insertBefore(canvas, img);
                /* l'img MJPEG est retirée : son flux HTTP s'arrête */
                if (img.parentNode === wrap) { wrap.removeChild(img); }
              }
              gotFrame = true;
              wrap.classList.remove('empty');
              try { cctx.drawImage(frame, 0, 0, canvas.width, canvas.height); }
              catch (e) { /* frame fermée entre-temps */ }
              frame.close();
            },
            error: function () { fail('Décodeur H.264 en erreur — retour au MJPEG.'); }
          });
          dec.configure({
            codec: (cfg && cfg.codec) || 'avc1.42E01E',
            optimizeForLatency: true
          });
        } catch (e2) { fail(null); }
        return;
      }
      /* frames binaires Annex-B */
      if (!dec || dec.state !== 'configured') { return; }
      var u8 = new Uint8Array(ev.data);
      var key = annexbIsKey(u8);
      if (!gotKey && !key) { return; }   /* on attend la 1re keyframe */
      gotKey = true;
      try {
        dec.decode(new EncodedVideoChunk({
          type: key ? 'key' : 'delta',
          timestamp: Math.round(n++ * 1e6 / ((cfg && cfg.fps) || 30)),
          data: ev.data
        }));
      } catch (e3) { fail('Décodage H.264 impossible — retour au MJPEG.'); }
    });
  }

  /* Recharge tous les flux MJPEG (cache-bust) — à chaque (re)connexion WS,
     car un flux mort sans coupure WS ne se répare pas tout seul. */
  function refreshPreviews() {
    var imgs = document.querySelectorAll('img.preview-stream');
    for (var i = 0; i < imgs.length; i++) {
      var src = imgs[i].getAttribute('data-src');
      if (src) { imgs[i].src = src + '?r=' + Date.now(); }
    }
  }

  function doGoto(input) {
    var n = cnParse(input.value);
    if (n === null) {
      input.classList.add('danger-text');
      uiWarn(trf('Numéro de cue invalide : {0}', input.value));
      return;
    }
    input.classList.remove('danger-text');
    sendCmd({ cmd: 'cue_goto', cue: n });
  }

  function nextCueAfter(n) {
    var list = cues();
    if (!list.length) { return null; }
    if (n === null || n === undefined) { return list[0].number; }
    for (var i = 0; i < list.length; i++) {
      if (list[i].number > n) { return list[i].number; }
    }
    return null;
  }

  /* ------------------------------------------ actions de cue (partagées
     entre la barre d'outils de l'onglet Cues et les menus contextuels). */

  function commitCue(c, mut) {
    var copy = JSON.parse(JSON.stringify(c));
    mut(copy);
    sendEdit({ op: 'cue_update', cue: copy });
  }

  function duplicateCue(c) {
    var copy = JSON.parse(JSON.stringify(c));
    var next = nextCueAfter(c.number);
    copy.number = next === null ? c.number + 1000 : Math.floor((c.number + next) / 2);
    if (copy.number === c.number) {
      uiWarn(trf('Pas de place entre {0} et la suivante.', cnStr(c.number)));
      return;
    }
    copy.name += tr(' (copie)');
    if (sendEdit({ op: 'cue_add', cue: copy })) {
      toast(trf('Cue {0} dupliquée en {1}', cnStr(c.number), cnStr(copy.number)), 'ok');
    }
  }

  function renumberCue(c, n) {
    if (n === null) { uiWarn('Numéro invalide (ex. 12.5).'); return false; }
    if (n === c.number) { return false; }
    if (cues().some(function (x) { return x.number === n; })) {
      uiWarn(trf('Le numéro {0} existe déjà.', cnStr(n)));
      return false;
    }
    var copy = JSON.parse(JSON.stringify(c));
    copy.number = n;
    sendEdit({ op: 'cue_remove', number: c.number });
    sendEdit({ op: 'cue_add', cue: copy });
    if (S.sel.cue === c.number) { S.sel.cue = n; }
    return true;
  }

  function deleteCueConfirm(c) {
    confirmDialog({
      title: trf('Supprimer la cue {0} ?', cnStr(c.number)),
      message: trf('{0} — annulable ensuite avec Ctrl+Z.',
        c.name ? ('« ' + c.name + ' »') : tr('Cue sans nom')),
      confirm: 'Supprimer',
      onConfirm: function () {
        sendEdit({ op: 'cue_remove', number: c.number });
        if (S.sel.cue === c.number) { S.sel.cue = null; }
      }
    });
  }

  /* Menu contextuel d'une cue (cuelist Live + table de l'onglet Cues). */
  function cueCtxItems(c) {
    var r = rt();
    var isStandby = r.standby === c.number;
    var items = [
      { kind: 'head', label: trf('Cue {0}', cnStr(c.number)) + (c.name ? ' — ' + c.name : '') },
      {
        label: 'Armer (standby)', disabled: isStandby, reason: 'Déjà en standby',
        tip: 'Place cette cue en standby — le prochain GO la lance',
        action: function () { sendCmd({ cmd: 'cue_standby', cue: c.number }); }
      },
      {
        label: 'Lancer (GOTO)', tip: 'Saute immédiatement à cette cue',
        action: function () { sendCmd({ cmd: 'cue_goto', cue: c.number }); }
      },
      { kind: 'sep' },
      ctxEdit({
        label: (c.armed === false) ? 'Ré-armer la cue' : 'Désarmer la cue',
        tip: 'Désarmée : sautée par GO/follow (GOTO la joue quand même)',
        action: function () { commitCue(c, function (x) { x.armed = !(c.armed !== false); }); }
      }),
      ctxEdit({ label: 'Dupliquer', action: function () { duplicateCue(c); } }),
      ctxEdit({
        label: 'Renuméroter…',
        action: function () {
          var v = window.prompt(trf('Nouveau numéro pour la cue {0} :', cnStr(c.number)), cnStr(c.number));
          if (v === null) { return; }
          renumberCue(c, cnParse(v.trim()));
        }
      }),
      ctxEdit({
        label: 'Notes…', tip: 'Notes de régie, visibles en Live sous le transport',
        action: function () {
          var v = window.prompt(trf('Notes de régie de la cue {0} :', cnStr(c.number)), c.notes || '');
          if (v !== null) { commitCue(c, function (x) { x.notes = v; }); }
        }
      })
    ];
    if (!isShowMode()) {
      items.push({ kind: 'head', label: 'Couleur' });
      items.push({
        kind: 'swatches', colors: CUE_COLORS,
        pick: function (col) { commitCue(c, function (x) { x.color = col; }); }
      });
    }
    items.push({ kind: 'sep' });
    items.push(ctxEdit({
      label: 'Supprimer', danger: true,
      action: function () { deleteCueConfirm(c); }
    }));
    return items;
  }

  function cuelistDom() {
    var wrap = el('div', {
      id: 'cuelist', role: 'list',
      'data-tip': 'Clic ou Entrée : met la cue en standby. Double-clic : GOTO immédiat.'
    });
    var list = cues();
    if (!list.length) {
      wrap.appendChild(el('div', { style: 'padding:12px' },
        emptyState('list', 'Aucune cue — créez la conduite dans l’onglet Cues.',
          { label: 'Ouvrir l’onglet Cues', onclick: function () { setTab('cues'); } })));
      return wrap;
    }
    list.forEach(function (c) {
      var disarmed = c.armed === false;
      var row = el('div', {
        class: 'cue-row' + (disarmed ? ' disarmed' : ''), 'data-cn': c.number,
        role: 'button', tabindex: '0',
        'aria-label': trf('Cue {0}', cnStr(c.number)) + (c.name ? ' — ' + c.name : ''),
        'data-tip': disarmed ? 'Cue désarmée : sautée par GO/follow (GOTO la joue quand même).' : null
      },
        el('span', { class: 'cue-num' },
          c.color ? el('span', { class: 'cue-color-dot', style: 'background:' + c.color }) : null,
          cnStr(c.number)),
        el('span', { class: 'cue-name' }, c.name || ''),
        el('span', { class: 'cue-trans' }, transitionLabel(c.transition), cueBadges(c),
          disarmed ? el('span', { class: 'cue-badge off' }, 'désarmée') : null),
        c.notes ? el('span', { class: 'cue-notes' }, c.notes) : null,
        el('div', { class: 'cue-progress' }, el('div')));
      row.addEventListener('click', function () { sendCmd({ cmd: 'cue_standby', cue: c.number }); });
      row.addEventListener('dblclick', function () { sendCmd({ cmd: 'cue_goto', cue: c.number }); });
      onCtx(row, function () { return cueCtxItems(c); });
      /* Entrée = standby (Espace reste le GO global, jamais intercepté) */
      row.addEventListener('keydown', function (e) {
        if (e.key === 'Enter' && !e.repeat) {
          e.preventDefault();
          sendCmd({ cmd: 'cue_standby', cue: c.number });
        }
      });
      wrap.appendChild(row);
    });
    return wrap;
  }

  function transitionLabel(t) {
    if (!t) { return ''; }
    var k = { cut: 'Cut', crossfade: 'Fondu', through_black: 'Par le noir' }[t.kind] || t.kind;
    return t.kind === 'cut' ? tr(k) : trf('{0} {1} s', tr(k), fmtF(t.dur_s, 1));
  }

  /* Badges compacts d'une cue : follow, boucle de conduite (goto_after),
     mode de fin des médias posés (boucle / aller-retour). */
  function cueBadges(c) {
    var out = [];
    var k = followKind(c.follow);
    if (k === 'after_media') {
      out.push(el('span', { class: 'cue-badge', 'data-tip': 'Follow : enchaîne à la fin du média' }, 'suit média'));
    }
    if (k === 'wait') {
      out.push(el('span', { class: 'cue-badge', 'data-tip': 'Follow : enchaîne après l’attente' },
        trf('attente {0} s', fmtF(followWait(c.follow), 1))));
    }
    var tct = c.triggers && c.triggers.timecode;
    if (tct) {
      out.push(el('span', {
        class: 'cue-badge tc',
        'data-tip': trf('Déclenchée par timecode à {0}', tct) +
          (settings().timecode_chase ? '' : tr(' — chase inactif (Réglages → Chase timecode)'))
      }, 'TC ' + tct));
    }
    if (c.goto_after !== null && c.goto_after !== undefined) {
      out.push(el('span', { class: 'cue-badge loop', 'data-tip': trf('À la fin, saute à la cue {0}', cnStr(c.goto_after)) },
        trf('⟳ vers {0}', cnStr(c.goto_after))));
    }
    var states = c.states || [];
    if (states.some(function (st) { return st.playback && st.playback.end === 'loop'; })) {
      out.push(el('span', { class: 'cue-badge loop', 'data-tip': 'Au moins un média de cette cue joue en boucle' }, '⟳ boucle'));
    }
    if (states.some(function (st) {
      return st.playback && (st.playback.end === 'ping_pong' || st.playback.end === 'palindrome');
    })) {
      out.push(el('span', { class: 'cue-badge loop', 'data-tip': 'Au moins un média joue en aller-retour' }, '⇄ aller-retour'));
    }
    return out;
  }

  function installDbo(btn) {
    var holdTimer = null, fired = false;
    btn.addEventListener('dblclick', function () { if (!fired) { dboToggle(); } fired = false; });
    btn.addEventListener('pointerdown', function () {
      fired = false;
      btn.classList.add('arming');
      holdTimer = setTimeout(function () {
        fired = true;
        btn.classList.remove('arming');
        dboToggle();
      }, 600);
    });
    function cancel() {
      btn.classList.remove('arming');
      if (holdTimer) { clearTimeout(holdTimer); holdTimer = null; }
    }
    btn.addEventListener('pointerup', cancel);
    btn.addEventListener('pointerleave', cancel);
    /* Tactile : un scroll ou un long-press système émet pointercancel —
       sans ce listener, le timer de 600 ms partait quand même (blackout
       fantôme pendant un simple défilement de page). */
    btn.addEventListener('pointercancel', cancel);
    btn.addEventListener('lostpointercapture', cancel);
  }

  function rtItem(label, val, cls) {
    return el('span', null,
      el('span', { class: 'rt-label' }, label),
      el('span', { class: 'rt-val' + (cls ? ' ' + cls : '') }, val));
  }

  /* Chip de santé : vert = OK, ambre = à surveiller, rouge = problème. */
  function chip(cls, text, tip) {
    return el('span', { class: 'chip' + (cls ? ' ' + cls : ''), 'data-tip': tip || null }, text);
  }

  /* État d'interaction du slider master (survit aux re-renders). */
  var MASTER = { held: false, until: 0 };

  /* Mises à jour dynamiques (10 Hz) sans re-render. */
  function updateDyn() {
    /* re-render différé en attente : re-tenter dès que la saisie est finie
       (ceinture au focusout, qui peut ne pas être émis fenêtre non focusée) */
    if (renderPending) { requestRenderMain(); if (!renderPending) { return; } }
    var r = rt();
    /* cuelist : surlignage + progression */
    var rows = document.querySelectorAll('.cue-row');
    rows.forEach(function (row) {
      var n = parseInt(row.getAttribute('data-cn'), 10);
      row.classList.toggle('active', r.active === n);
      row.classList.toggle('standby', r.standby === n);
      if (r.active === n) {
        var bar = row.querySelector('.cue-progress > div');
        if (bar) { bar.style.width = Math.round(clamp(r.progress || 0, 0, 1) * 100) + '%'; }
      }
    });
    var line = byId('rt-line');
    if (line) {
      line.textContent = '';
      line.appendChild(rtItem('ACTIVE', cnStr(r.active), ''));
      line.appendChild(rtItem('STANDBY', cnStr(r.standby), 'rt-standby'));
      if (r.remaining_s > 0) { line.appendChild(rtItem('RESTE', trf('{0} s', fmtF(r.remaining_s, 1)), '')); }
      if (r.transition_active) { line.appendChild(rtItem('TRANSITION', 'en cours…', '')); }
    }
    var master = byId('master-range');
    if (master && !MASTER.held && Date.now() >= MASTER.until) {
      master.value = r.master;
      var lbl = byId('master-val');
      /* pas d'écrasement pendant la saisie exacte (clic sur la valeur) */
      if (lbl && !lbl.getAttribute('data-editing')) {
        lbl.textContent = Math.round(r.master * 100) + ' %';
      }
    }
    var dbo = byId('dbo-btn');
    if (dbo) {
      dbo.classList.toggle('engaged', !!r.dbo);
      var dlbl = dbo.querySelector('.dbo-label');
      if (dlbl) { dlbl.textContent = r.dbo ? tr('DBO ACTIF — relâcher') : 'DBO'; }
    }
    /* anti double-GO : ré-applique l'état d'attente si le bouton GO vient
       d'être recréé par un re-render en plein délai */
    if (goLocked()) { goCooldownVisual(); }
    updateStandbyNotes(r);
    updateProtoChips();
    updateTitle(r);
    updateRemaining(r);
    updateActiveCues(r);
    updateStatusBar(r);
    updateUpdateBadge();
    var bpm = byId('bpm-val');
    if (bpm) { bpm.textContent = fmtF(r.bpm, 1); }
    (r.mod_levels || []).forEach(function (pair) {
      var m = byId('mod-meter-' + pair[0]);
      if (m) { m.style.width = Math.round(clamp(pair[1], 0, 1) * 100) + '%'; }
    });
    /* liste des périphériques audio : rafraîchie si le moteur en publie de
       nouveaux (branchement à chaud), sans casser une sélection en cours */
    var devSel = byId('audio-dev-sel');
    if (devSel && document.activeElement !== devSel &&
        devSel.getAttribute('data-count') !== String(audioDevices().length)) {
      fillDeviceSel(devSel);
    }
  }

  function updateHealth() {
    var line = byId('health-line');
    if (!line) { return; }
    var h = S.health;
    line.textContent = '';
    if (!h) {
      line.appendChild(chip(S.connected ? '' : 'bad',
        tr(S.connected ? 'En attente de données…' : 'Hors ligne')));
      return;
    }
    function grade(v, warnAt, badAt) {
      if (typeof v !== 'number' || !isFinite(v)) { return ''; }
      if (v >= badAt) { return 'bad'; }
      if (v >= warnAt) { return 'warn'; }
      return 'ok';
    }
    (h.fps || []).forEach(function (p) {
      var v = p[1];
      var cls = (typeof v === 'number' && isFinite(v))
        ? (v >= 30 ? 'ok' : (v >= 15 ? 'warn' : 'bad')) : '';
      line.appendChild(chip(cls, 'S' + p[0] + ' ' + fmtF(v, 0) + ' fps', trf('Cadence de rendu de la sortie {0}', p[0])));
    });
    var drops = 0;
    (h.drops || []).forEach(function (p) { drops += p[1]; });
    line.appendChild(chip(grade(drops, 1, 100), 'drops ' + drops, 'Frames perdues (cumul)'));
    line.appendChild(chip(grade(h.cpu_pct, 70, 90), 'CPU ' + fmtF(h.cpu_pct, 0) + ' %', 'Charge processeur du moteur'));
    line.appendChild(chip('', trf('{0} Mo', fmtF(h.mem_mb, 0)), 'Mémoire utilisée par le moteur'));
    if (h.temp_c !== null && h.temp_c !== undefined) {
      line.appendChild(chip(grade(h.temp_c, 70, 80), fmtF(h.temp_c, 0) + ' °C', 'Température (Raspberry Pi)'));
    }
    line.appendChild(chip(S.connected ? 'ok' : 'bad', 'WS ' + tr(S.connected ? 'OK' : 'coupé'),
      'Liaison WebSocket avec le moteur'));
  }

  /* -------------------------------------- notes de régie (cue en standby) */

  function updateStandbyNotes(r) {
    var sn = byId('standby-notes');
    if (!sn || sn.classList.contains('editing')) { return; }
    var n = (r.standby !== null && r.standby !== undefined) ? r.standby : null;
    var cue = n !== null ? cues().find(function (c) { return c.number === n; }) : null;
    var txt = (cue && cue.notes) || '';
    var key = (cue ? cue.number : 'none') + '|' + txt;
    if (sn.getAttribute('data-key') === key) { return; }
    sn.setAttribute('data-key', key);
    sn.textContent = '';
    if (txt) {
      sn.classList.remove('empty');
      sn.appendChild(el('span', { class: 'standby-notes-label' },
        trf('NOTES — CUE {0}', cnStr(cue.number))));
      sn.appendChild(el('span', { class: 'standby-notes-text' }, txt));
    } else {
      sn.classList.add('empty');
      sn.textContent = cue
        ? tr(isShowMode() ? 'Aucune note de régie.' : 'Aucune note de régie — touche O pour en ajouter.')
        : '';
    }
  }

  /* Touche O : édite les notes de la cue en standby (mode édition). */
  function editStandbyNotes() {
    if (isShowMode()) { return; }
    var r = rt();
    var n = (r.standby !== null && r.standby !== undefined) ? r.standby : r.active;
    if (n === null || n === undefined) {
      uiWarn('Aucune cue en standby — pas de notes à éditer.');
      return;
    }
    var cue = cues().find(function (c) { return c.number === n; });
    if (!cue) { return; }
    if (S.tab !== 'live') { setTab('live'); }
    var sn = byId('standby-notes');
    if (!sn || sn.classList.contains('editing')) { return; }
    sn.classList.add('editing');
    sn.classList.remove('empty');
    sn.setAttribute('data-key', '');   /* force le rafraîchissement au retour */
    sn.textContent = '';
    var ta = el('textarea', { class: 'standby-notes-input', rows: 3 });
    ta.value = cue.notes || '';
    var done = false;
    ta.addEventListener('blur', function () {
      if (done) { return; }
      done = true;
      sn.classList.remove('editing');
      if (ta.value !== (cue.notes || '')) {
        var copy = JSON.parse(JSON.stringify(cue));
        copy.notes = ta.value;
        sendEdit({ op: 'cue_update', cue: copy });
      }
      updateStandbyNotes(rt());
    });
    sn.appendChild(el('span', { class: 'standby-notes-label' },
      trf('NOTES — CUE {0} (Échap ou clic ailleurs : terminer)', cnStr(n))));
    sn.appendChild(ta);
    ta.focus();
  }

  /* --------------------------- état des protocoles (runtime.protocols) */

  var PROTO_LABELS = { osc_in: 'OSC entrée', osc_out: 'OSC sortie', artnet: 'Art-Net', midi: 'MIDI' };

  function protoChips() {
    var p = rt().protocols;
    if (!p || typeof p !== 'object') {
      return [el('span', { class: 'muted' }, 'Statut des protocoles non publié par le moteur.')];
    }
    return Object.keys(PROTO_LABELS)
      .filter(function (k) { return p[k] !== undefined && p[k] !== null; })
      .map(function (k) {
        var v = String(p[k]);
        var cls = v === 'ok' ? 'ok' : (v === 'inactif' ? '' : 'bad');
        var text = v === 'ok' ? 'OK' : tr(v);
        return chip(cls, tr(PROTO_LABELS[k]) + ' — ' + text,
          v.indexOf('erreur') === 0 ? v : trf('État du protocole {0}', tr(PROTO_LABELS[k])));
      });
  }

  function updateProtoChips() {
    var pl = byId('proto-line');
    if (!pl) { return; }
    var sig = JSON.stringify(rt().protocols || null);
    if (pl.getAttribute('data-sig') === sig) { return; }
    pl.setAttribute('data-sig', sig);
    pl.textContent = '';
    protoChips().forEach(function (c) { pl.appendChild(c); });
  }

  /* ------------------------------------------------- signature de fenêtre
     Titre dynamique : « ● Cue 12 — MonShow » en jeu, sinon le show. */

  function updateTitle(r) {
    var name = show().name || '';
    var t;
    if (r.active !== null && r.active !== undefined) {
      t = trf('● Cue {0}', cnStr(r.active)) + (name ? ' — ' + name : ' — Conduite');
    } else {
      t = (name ? name + ' — ' : '') + 'Conduite';
    }
    if (document.title !== t) { document.title = t; }
  }

  /* ------------------------------ temps restant en grand (près du GO) */

  function updateRemaining(r) {
    var lr = byId('live-remaining');
    if (!lr) { return; }
    var v = lr.querySelector('.lr-val');
    if (!v) { return; }
    var active = r.active !== null && r.active !== undefined;
    var rem = (typeof r.remaining_s === 'number' && isFinite(r.remaining_s)) ? r.remaining_s : 0;
    if (!active || rem <= 0) {
      lr.className = 'idle';
      var txt = active ? (r.transition_active ? 'transition…' : 'en cours') : '—';
      if (v.textContent !== txt) { v.textContent = txt; }
      return;
    }
    lr.className = rem < 5 ? 'urgent' : (rem < 10 ? 'soon' : '');
    v.textContent = fmtF(rem, 1) + ' s';
  }

  /* --------------------------------------- panneau replié « Cues actives » */

  function updateActiveCues(r) {
    var det = byId('active-cues');
    var list = byId('active-cues-list');
    if (!det || !list || !det.open) { return; }   /* replié : zéro travail DOM */
    var active = (r.active !== null && r.active !== undefined)
      ? cues().find(function (c) { return c.number === r.active; })
      : null;
    if (!active) {
      if (list.getAttribute('data-key') !== 'none') {
        list.setAttribute('data-key', 'none');
        list.textContent = '';
        list.appendChild(el('div', { class: 'acue-empty' }, 'Aucune cue en cours.'));
      }
      return;
    }
    var key = String(active.number);
    if (list.getAttribute('data-key') !== key) {
      list.setAttribute('data-key', key);
      list.textContent = '';
      list.appendChild(el('div', { class: 'acue-row' },
        el('span', { class: 'acue-num' }, cnStr(active.number)),
        el('span', null, active.name || ''),
        el('span', { class: 'acue-time', id: 'acue-time' }, ''),
        el('div', { class: 'acue-bar' }, el('div', { id: 'acue-bar' }))));
    }
    var t = byId('acue-time');
    if (t) {
      t.textContent = r.remaining_s > 0
        ? trf('reste {0} s', fmtF(r.remaining_s, 1))
        : tr(r.transition_active ? 'transition…' : 'en cours');
    }
    var bar = byId('acue-bar');
    if (bar) { bar.style.width = Math.round(clamp(r.progress || 0, 0, 1) * 100) + '%'; }
  }

  /* --------------------------- barre d'état (footer) + « État du show »
     Protocoles compacts, version, et icône d'avertissements qui n'apparaît
     que si runtime.warnings est non vide — clic : panneau listant chaque
     avertissement avec son action (relink / output / midi). */

  var SB = { protoSig: null, warnSig: null, panel: null };

  var WARN_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3.5 22 20H2z"/><path d="M12 10v4M12 17h.01"/></svg>';

  var SB_PROTO_SHORT = { osc_in: 'OSC in', osc_out: 'OSC out', artnet: 'Art-Net', midi: 'MIDI' };

  var WARN_ACTIONS = {
    relink: { label: 'Voir les médias', tab: 'medias' },
    output: { label: 'Voir les sorties', tab: 'sorties' },
    midi: { label: 'Ouvrir le patch', tab: 'patch' }
  };

  /* Texte d'un avertissement moteur. Le moteur envoie la phrase française
     toute faite (`msg`) ET sa forme démontée (`key` = gabarit, `args` =
     valeurs) : on recompose depuis le gabarit pour l'avoir dans la langue de
     l'opérateur, et on retombe sur `msg` si un moteur plus ancien ne publie
     pas encore `key` (rétro-compatibilité du contrat). */
  function warnText(w) {
    if (w && typeof w.key === 'string' && w.key) {
      return trf.apply(null, [w.key].concat(Array.isArray(w.args) ? w.args : []));
    }
    return tr(String((w && w.msg) || ''));
  }

  function updateStatusBar(r) {
    var host = byId('sb-protos');
    if (host) {
      var p = r.protocols;
      var sig = JSON.stringify(p || null);
      if (sig !== SB.protoSig) {
        SB.protoSig = sig;
        host.textContent = '';
        if (p && typeof p === 'object') {
          Object.keys(SB_PROTO_SHORT).forEach(function (k) {
            if (p[k] === undefined || p[k] === null) { return; }
            var v = String(p[k]);
            var cls = v === 'ok' ? 'ok' : (v === 'inactif' ? '' : 'bad');
            host.appendChild(el('span', {
              class: 'sb-proto' + (cls ? ' ' + cls : ''),
              'data-tip': tr(PROTO_LABELS[k]) + ' — ' + (v === 'ok' ? 'OK' : tr(v))
            }, SB_PROTO_SHORT[k]));
          });
        }
      }
    }
    /* Timecode entrant (runtime.timecode, contrat {time, rate, locked,
       chasing}) : affiché près de l'horloge UNIQUEMENT si le chase est
       activé dans les Réglages. Vert = verrouillé (roue libre de 2 s
       comprise), orange = signal perdu (dernier timecode figé), gris =
       aucun signal jamais reçu. */
    var tcEl = byId('sb-timecode');
    if (tcEl) {
      var chase = !!settings().timecode_chase;
      var t = r.timecode || null;
      tcEl.classList.toggle('hidden', !chase);
      if (chase) {
        tcEl.textContent = 'TC ' + (t ? t.time : '--:--:--:--');
        tcEl.classList.toggle('ok', !!(t && t.locked));
        tcEl.classList.toggle('lost', !!(t && !t.locked));
        var tip = !t
          ? 'Chase timecode actif — aucun signal MTC reçu (brancher une source timecode sur un port MIDI).'
          : (t.locked
            ? trf('Timecode MTC verrouillé ({0} i/s) — le chase suit.', tcRateLabel(t.rate))
            : trf('Signal timecode perdu ({0} i/s) — dernier timecode figé, les cues actives continuent.', tcRateLabel(t.rate)));
        if (tcEl.getAttribute('data-tip') !== tip) { tcEl.setAttribute('data-tip', tip); }
      }
    }
    var chip = byId('warn-chip');
    if (chip) {
      var warns = Array.isArray(r.warnings) ? r.warnings : [];
      var sig2 = JSON.stringify(warns);
      if (sig2 !== SB.warnSig) {
        SB.warnSig = sig2;
        var hasErr = warns.some(function (w) { return w && w.level === 'err'; });
        chip.classList.toggle('hidden', !warns.length);
        chip.classList.toggle('err', hasErr);
        chip.innerHTML = WARN_ICON;
        chip.appendChild(document.createTextNode(
          trf(' {0} — État du show', warns.length)));
        if (SB.panel) { renderStatusPanel(); }   /* panneau ouvert : rafraîchi */
      }
    }
  }

  function toggleStatusPanel() {
    if (SB.panel) { closeStatusPanel(); return; }
    SB.panel = el('div', { id: 'status-panel', role: 'dialog', 'aria-label': 'État du show' });
    document.body.appendChild(SB.panel);
    renderStatusPanel();
  }

  function closeStatusPanel() {
    if (SB.panel && SB.panel.parentNode) { SB.panel.parentNode.removeChild(SB.panel); }
    SB.panel = null;
  }

  function renderStatusPanel() {
    var p = SB.panel;
    if (!p) { return; }
    p.textContent = '';
    p.appendChild(el('h3', null, 'État du show',
      el('span', { class: 'spacer' }),
      el('button', { class: 'ghost', 'data-tip': 'Fermer', onclick: closeStatusPanel }, '✕')));
    var warns = Array.isArray(rt().warnings) ? rt().warnings : [];
    if (!warns.length) {
      p.appendChild(el('div', { class: 'warn-empty' }, 'Aucun avertissement — tout est en ordre.'));
      return;
    }
    warns.forEach(function (w) {
      if (!w || typeof w !== 'object') { return; }
      var act = WARN_ACTIONS[w.action];
      p.appendChild(el('div', { class: 'warn-item' + (w.level === 'err' ? ' err' : '') },
        el('span', { class: 'warn-dot' }),
        el('span', { class: 'warn-msg' }, warnText(w)),
        (act && !isShowMode()) ? el('button', {
          'data-tip': 'Ouvre l’onglet concerné',
          onclick: function () { setTab(act.tab); closeStatusPanel(); }
        }, act.label) : null));
    });
  }

  /* ----------------------- mise à jour disponible (runtime.update, opt-in)
     Le moteur ne vérifie qu'avec settings.update_check = true (une requête
     au démarrage, mode Édition, jamais de téléchargement). S'il publie
     runtime.update = {version, url, notes} : badge discret dans le footer,
     clic = panneau détaillé avec lien. */

  var UPD = { pop: null, sig: null };

  function updateUpdateBadge() {
    var b = byId('update-badge');
    if (!b) { return; }
    var u = rt().update || null;
    var sig = u ? (u.version + '|' + u.url) : null;
    if (sig === UPD.sig) { return; }
    UPD.sig = sig;
    b.classList.toggle('hidden', !u);
    if (u) {
      b.textContent = trf('Mise à jour {0}', u.version || '?');
    } else {
      closeUpdatePop();
    }
  }

  function closeUpdatePop() {
    if (!UPD.pop) { return false; }
    if (UPD.pop.parentNode) { UPD.pop.parentNode.removeChild(UPD.pop); }
    UPD.pop = null;
    return true;
  }

  function toggleUpdatePop() {
    if (UPD.pop) { closeUpdatePop(); return; }
    var u = rt().update;
    if (!u) { return; }
    UPD.pop = el('div', { id: 'update-pop', role: 'dialog', 'aria-label': 'Mise à jour disponible' },
      el('h3', null, trf('Conduite {0} disponible', u.version || '?')),
      el('div', { class: 'muted' },
        'Rien n’est téléchargé automatiquement — la mise à jour reste à votre main, après le spectacle.'),
      u.notes ? el('div', { class: 'update-notes' }, u.notes) : null,
      el('div', { class: 'toolbar', style: 'margin:10px 0 0' },
        u.url ? el('a', { href: u.url, target: '_blank', rel: 'noopener noreferrer' }, 'Page de téléchargement') : null,
        el('span', { class: 'spacer' }),
        el('button', { class: 'ghost', onclick: closeUpdatePop }, 'Fermer')));
    document.body.appendChild(UPD.pop);
  }

  /* GET /about : version + crédits pour le footer, l'infobulle du wordmark
     et le panneau « À propos » des Réglages. Best-effort, jamais bloquant. */
  function loadAbout() {
    if (typeof fetch !== 'function') { return; }
    fetch('/about').then(function (resp) {
      return resp.ok ? resp.json() : null;
    }).then(function (a) {
      if (!a || typeof a !== 'object') { return; }
      S.about = a;
      var v = byId('sb-version');
      if (v) { v.textContent = 'Conduite v' + (a.version || '?'); }
      var brand = byId('brand');
      if (brand) {
        brand.setAttribute('data-tip', 'Conduite v' + (a.version || '?') +
          (a.git ? ' (' + a.git + ')' : '') + tr(' — régie vidéo de spectacle'));
      }
      if (S.tab === 'reglages') { requestRenderMain(); }
    }).catch(function () { /* hors ligne : pas de version affichée */ });
  }

  /* ================================================================= CUES */

  RENDERERS.cues = function () {
    var root = el('section', { class: 'tab-panel' });
    var panel = el('div', { class: 'panel' }, el('h2', null, 'Cuelist — édition'));

    var selCue = cues().find(function (c) { return c.number === S.sel.cue; }) || null;

    panel.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Ajoute une cue à la fin de la conduite',
        onclick: function () {
          var c = newCue(nextFreeCueNumber());
          sendEdit({ op: 'cue_add', cue: c });
          S.sel.cue = c.number;
        }
      }, 'Ajouter'),
      el('button', {
        disabled: !selCue, 'data-tip': 'Duplique la cue sélectionnée (numéro intercalé)',
        onclick: function () { if (selCue) { duplicateCue(selCue); } }
      }, 'Dupliquer'),
      el('button', {
        class: 'danger', disabled: !selCue, 'data-tip': 'Supprime la cue sélectionnée (confirmation demandée)',
        onclick: function () { if (selCue) { deleteCueConfirm(selCue); } }
      }, 'Supprimer'),
      el('span', { class: 'spacer' }),
      el('button', {
        class: 'primary', disabled: !selCue,
        'data-tip': 'Recopie l’état des slices de la cue active dans la cue sélectionnée (snapshot)',
        onclick: function () {
          if (!selCue) { return; }
          var active = cues().find(function (c) { return c.number === rt().active; });
          if (!active || !active.states || !active.states.length) {
            uiWarn('Aucun état courant à enregistrer (pas de cue active).');
            return;
          }
          active.states.forEach(function (st) {
            sendEdit({ op: 'cue_update_state', number: selCue.number, state: JSON.parse(JSON.stringify(st)) });
          });
          uiInfo(trf('État courant enregistré dans la cue {0}', cnStr(selCue.number)));
        }
      }, 'Enregistrer l’état courant dans la cue')));

    var table = el('table', { class: 'grid' },
      el('tr', null,
        el('th', { 'data-tip': 'Numéro décimal (1, 2, 2.5…) — insertion sans renumérotation' }, 'N°'),
        el('th', { 'data-tip': 'Cue armée : jouée normalement. Désarmée : grisée, sautée par GO/follow (GOTO la joue quand même). Pour retirer un tableau en répétition sans casser la conduite.' }, 'Armée'),
        el('th', null, 'Nom'),
        el('th', { 'data-tip': 'Type de transition d’entrée' }, 'Transition'),
        el('th', { 'data-tip': 'Durée de la transition (secondes)' }, 'Durée'),
        el('th', { 'data-tip': 'Courbe d’interpolation' }, 'Courbe'),
        el('th', { 'data-tip': 'Enchaînement : GO manuel, fin de média, ou attente chronométrée' }, 'Follow'),
        el('th', { 'data-tip': 'Déclenchement par timecode : « HH:MM:SS:FF », vide = cue manuelle. Actif quand « Chase timecode » est coché dans Réglages et qu’un signal MTC est verrouillé.' }, 'Timecode'),
        el('th', { 'data-tip': 'Notes de régie (visibles en Live)' }, 'Notes'),
        el('th', { 'data-tip': 'Contenus posés par cette cue' }, 'Contenus')));

    cues().forEach(function (c) {
      table.appendChild(cueRow(c));
    });
    panel.appendChild(el('div', { style: 'overflow-x:auto' }, table));
    if (!cues().length) {
      panel.appendChild(el('div', { style: 'padding:8px 0 0' },
        emptyState('list', 'Aucune cue — la conduite commence ici.', {
          label: 'Ajouter une cue',
          onclick: function () {
            var c = newCue(nextFreeCueNumber());
            sendEdit({ op: 'cue_add', cue: c });
            S.sel.cue = c.number;
          }
        })));
    }
    root.appendChild(panel);
    return root;
  };

  function cueRow(c) {
    var row = el('tr', {
      class: (c.number === S.sel.cue ? 'selected' : '') + (c.armed === false ? ' disarmed' : '')
    });
    row.addEventListener('click', function (e) {
      if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT' || e.target.tagName === 'BUTTON') { return; }
      S.sel.cue = c.number;
      renderMain();
    });
    onCtx(row, function () { return cueCtxItems(c); });

    function commit(mut) { commitCue(c, mut); }

    var num = el('input', { type: 'text', value: cnStr(c.number), style: 'width:60px', 'data-tip': 'Renuméroter la cue (déplace dans la liste)' });
    num.addEventListener('change', function () {
      var n = cnParse(num.value);
      if (n === null || n === c.number || !renumberCue(c, n)) {
        num.value = cnStr(c.number);
      }
    });
    row.appendChild(el('td', null, num));

    var armed = el('input', {
      type: 'checkbox', checked: c.armed !== false,
      'data-tip': 'Armée = jouée. Désarmée = sautée par GO/follow (GOTO la joue quand même).'
    });
    armed.addEventListener('change', function () {
      commit(function (x) { x.armed = armed.checked; });
    });
    row.appendChild(el('td', null, armed));

    var name = el('input', { type: 'text', value: c.name || '', 'data-tip': 'Nom de la cue' });
    name.addEventListener('change', function () { commit(function (x) { x.name = name.value; }); });
    row.appendChild(el('td', null, name));

    var kind = sel(['cut', 'crossfade', 'through_black'], ['Cut', 'Fondu', 'Par le noir'],
      (c.transition || {}).kind || 'cut',
      function (v) { commit(function (x) { x.transition = x.transition || {}; x.transition.kind = v; }); });
    kind.setAttribute('data-tip', 'Cut : bascule sèche. Fondu : crossfade A/B. Par le noir : descente puis montée.');
    row.appendChild(el('td', null, kind));

    var dur = el('input', { type: 'number', min: 0, step: 0.1, value: (c.transition || {}).dur_s || 0 });
    dur.addEventListener('change', function () {
      commit(function (x) { x.transition = x.transition || {}; x.transition.dur_s = parseFloat(dur.value) || 0; });
    });
    row.appendChild(el('td', null, dur));

    var curve = sel(['linear', 'ease_in', 'ease_out', 'ease_in_out', 's_curve'],
      ['Linéaire', 'Ease in', 'Ease out', 'Ease in-out', 'Courbe S'],
      (c.transition || {}).curve || 'linear',
      function (v) { commit(function (x) { x.transition = x.transition || {}; x.transition.curve = v; }); });
    row.appendChild(el('td', null, curve));

    var fkind = followKind(c.follow);
    var fsel = sel(['manual', 'after_media', 'wait'], ['GO manuel', 'Fin de média', 'Attente (s)'], fkind, applyFollow);
    var fwait = el('input', {
      type: 'number', min: 0, step: 0.1, value: followWait(c.follow),
      style: fkind === 'wait' ? '' : 'display:none', 'data-tip': 'Secondes après le début de la cue'
    });
    fwait.addEventListener('change', applyFollow);
    function applyFollow() {
      var k = fsel.value;
      fwait.style.display = k === 'wait' ? '' : 'none';
      commit(function (x) {
        x.follow = k === 'wait' ? { wait: parseFloat(fwait.value) || 0 } : k;
      });
    }
    row.appendChild(el('td', null, fsel, fwait));

    var tcIn = el('input', {
      type: 'text', value: (c.triggers && c.triggers.timecode) || '',
      placeholder: '—', style: 'width:96px', class: 'tc-input',
      'data-tip': 'Position « HH:MM:SS:FF » qui déclenche la cue quand le chase timecode est actif (Réglages). Vide = cue manuelle, jamais touchée par le chase.'
    });
    tcIn.addEventListener('change', function () {
      var v = tcParse(tcIn.value);
      if (v === undefined) {
        uiWarn('Timecode invalide — format HH:MM:SS:FF (ex. 00:05:30:00).');
        tcIn.value = (c.triggers && c.triggers.timecode) || '';
        return;
      }
      tcIn.value = v || '';
      commit(function (x) {
        x.triggers = x.triggers || {};
        x.triggers.timecode = v;
      });
    });
    row.appendChild(el('td', null, tcIn));

    var notes = el('input', { type: 'text', value: c.notes || '', 'data-tip': 'Notes de régie' });
    notes.addEventListener('change', function () { commit(function (x) { x.notes = notes.value; }); });
    row.appendChild(el('td', null, notes));

    var contents = (c.states || []).map(function (st) {
      var slice = slices().find(function (s) { return s.id === st.slice; });
      return (slice ? slice.name : trf('slice {0}', st.slice)) + ' : ' + contentLabel(st.content);
    }).join(' | ') || '—';
    row.appendChild(el('td', { class: 'muted' }, contents));

    return row;
  }

  function sel(values, labels, current, onchange) {
    var s = el('select');
    values.forEach(function (v, i) {
      var o = el('option', { value: v }, labels[i] || v);
      if (v === current) { o.selected = true; }
      s.appendChild(o);
    });
    if (onchange) { s.addEventListener('change', function () { onchange(s.value); }); }
    return s;
  }

  /* ============================================================== MAPPING */

  var MAP = { throttleTs: 0 };

  /* Mire choisie dans les sélecteurs (survit aux re-renders). */
  var MIRE = { kind: 'ident' };

  function patternLabel(kind) {
    for (var i = 0; i < PATTERNS.length; i++) {
      if (PATTERNS[i][0] === kind) { return PATTERNS[i][1]; }
    }
    return kind;
  }

  function resetCorners(s) {
    [[0, 0], [1, 0], [1, 1], [0, 1]].forEach(function (c, i) {
      sendCornerSet(s.id, i, c[0], c[1]);
    });
    drawMapping();
    toast(trf('Coins de « {0} » réinitialisés', s.name || trf('slice {0}', s.id)), 'ok');
  }

  /* Sous-menu « Mire » d'un slice : toutes les mires + extinction. */
  function slicePatternSub(s) {
    return function () {
      var items = [{ kind: 'head', label: trf('Mire sur « {0} » (cue standby/active)', s.name || s.id) }];
      PATTERNS.forEach(function (p) {
        items.push(ctxEdit({
          label: p[1],
          action: function () { assignContent(s.id, { pattern: p[0] }); }
        }));
      });
      items.push({ kind: 'sep' });
      items.push(ctxEdit({
        label: 'Éteindre (aucun contenu)',
        action: function () { assignContent(s.id, 'none'); }
      }));
      return items;
    };
  }

  /* Menu contextuel d'un slice (liste Mapping + canvas). */
  function sliceCtxItems(s) {
    return [
      { kind: 'head', label: s.name || trf('Slice {0}', s.id) },
      ctxEdit({
        label: 'Réinitialiser les coins',
        tip: 'Repose les 4 coins plein cadre (0,0 → 1,1)',
        action: function () { resetCorners(s); }
      }),
      ctxEdit({ label: 'Mire', sub: slicePatternSub(s) }),
      ctxEdit({
        label: 'Renommer…',
        action: function () {
          var v = window.prompt(tr('Nom du slice :'), s.name || '');
          if (v === null || !v.trim()) { return; }
          var copy = JSON.parse(JSON.stringify(s));
          copy.name = v.trim();
          sendEdit({ op: 'slice_update', slice: copy });
        }
      }),
      { kind: 'sep' },
      ctxEdit({
        label: 'Supprimer', danger: true,
        action: function () {
          confirmDialog({
            title: trf('Supprimer le slice « {0} » ?', s.name || trf('slice {0}', s.id)),
            message: 'Son calage et ses contenus assignés dans les cues seront perdus — annulable ensuite avec Ctrl+Z.',
            confirm: 'Supprimer',
            onConfirm: function () {
              sendEdit({ op: 'slice_remove', id: s.id });
              if (S.sel.slice === s.id) { S.sel.slice = null; }
              renderMain();
            }
          });
        }
      })
    ];
  }

  RENDERERS.mapping = function () {
    var root = el('section', { class: 'tab-panel', id: 'mapping' });
    var outs = outputs();
    if (S.sel.output === null || !outs.some(function (o) { return o.id === S.sel.output; })) {
      S.sel.output = outs.length ? outs[0].id : null;
    }
    var out = currentOutput();

    var canvas = el('canvas', { id: 'map-canvas', 'data-tip': 'Glisser un coin pour le déplacer. Clic dans un slice pour le sélectionner. Flèches = nudge (Maj ×10, Alt ×0,1).' });
    var ratio = out && out.height ? out.height / out.width : 9 / 16;
    canvas.width = 960;
    canvas.height = Math.round(960 * ratio);
    installMapMouse(canvas);
    root.appendChild(el('div', { id: 'map-canvas-wrap' }, canvas));

    var side = el('div', { id: 'map-side' });

    var outSel = el('select', { 'data-tip': 'Sortie affichée dans l’éditeur' });
    outs.forEach(function (o) {
      var opt = el('option', { value: o.id }, o.name + ' (' + o.width + '×' + o.height + ')');
      if (o.id === S.sel.output) { opt.selected = true; }
      outSel.appendChild(opt);
    });
    outSel.addEventListener('change', function () {
      S.sel.output = parseInt(outSel.value, 10);
      S.sel.slice = null; S.sel.corner = null;
      renderMain();
    });

    side.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Sortie'),
      outSel));

    var selSlice = currentSlice();
    var slicePanel = el('div', { class: 'panel' }, el('h2', null, 'Slices'));
    slicePanel.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Ajoute un slice plein cadre sur cette sortie',
        onclick: function () {
          if (S.sel.output === null) { return; }
          var id = slices().reduce(function (m, s) { return Math.max(m, s.id); }, 0) + 1;
          sendEdit({
            op: 'slice_add', slice: {
              id: id, name: trf('Slice {0}', id), output: S.sel.output,
              corners: [[0, 0], [1, 0], [1, 1], [0, 1]],
              src: { x: 0, y: 0, w: 1, h: 1 }, z: 0, enabled: true
            }
          });
          S.sel.slice = id;
        }
      }, '+ Slice'),
      el('button', {
        class: 'danger', disabled: !selSlice, 'data-tip': 'Supprime le slice sélectionné (confirmation demandée)',
        onclick: function () {
          if (!selSlice) { return; }
          confirmDialog({
            title: trf('Supprimer le slice « {0} » ?', selSlice.name || trf('slice {0}', selSlice.id)),
            message: 'Son calage et ses contenus assignés dans les cues seront perdus — annulable ensuite avec Ctrl+Z.',
            confirm: 'Supprimer',
            onConfirm: function () {
              sendEdit({ op: 'slice_remove', id: selSlice.id });
              S.sel.slice = null;
              renderMain();
            }
          });
        }
      }, '− Slice')));

    slicesOfOutput().forEach(function (s) {
      var item = el('div', {
        class: 'slice-item' + (s.id === S.sel.slice ? ' selected' : ''),
        role: 'button', tabindex: '0',
        'data-tip': 'Sélectionne ce slice dans l’éditeur (Entrée au clavier)',
        onclick: function () { S.sel.slice = s.id; S.sel.corner = null; renderMain(); }
      }, trf('{0} (z {1}{2})', s.name, s.z, s.enabled ? '' : tr(', désactivé')));
      item.addEventListener('keydown', function (e) {
        if (e.key === 'Enter') { e.preventDefault(); S.sel.slice = s.id; S.sel.corner = null; renderMain(); }
      });
      onCtx(item, function () { return sliceCtxItems(s); });
      slicePanel.appendChild(item);
    });
    if (!slicesOfOutput().length) {
      slicePanel.appendChild(emptyState('screen', 'Aucun slice sur cette sortie — « + Slice » pour caler une zone.'));
    }
    side.appendChild(slicePanel);

    /* mires — sélecteur enrichi (grilles 4/16, barres SMPTE…) */
    var mireSel = sel(
      PATTERNS.map(function (p) { return p[0]; }),
      PATTERNS.map(function (p) { return p[1]; }),
      MIRE.kind,
      function (v) { MIRE.kind = v; });
    mireSel.setAttribute('data-tip', 'Mire à poser : identification (nom + résolution de la sortie), grilles de convergence 4/8/16, damier, barres.');
    side.appendChild(el('div', { class: 'panel edit-only' },
      el('h2', null, 'Mires'),
      el('div', { class: 'toolbar' },
        mireSel,
        el('button', {
          disabled: !selSlice, 'data-tip': 'Pose la mire choisie sur le slice sélectionné (dans la cue standby/active)',
          onclick: function () { if (selSlice) { assignContent(selSlice.id, { pattern: MIRE.kind }); } }
        }, 'Mire slice'),
        el('button', {
          'data-tip': 'Pose la mire choisie sur tous les slices de la sortie',
          onclick: function () {
            var ok = 0;
            slicesOfOutput().forEach(function (s) { if (assignContent(s.id, { pattern: MIRE.kind }, null, true)) { ok++; } });
            if (ok) { toast(trf('Mire « {0} » posée sur {1} slice(s) (cue {2})', tr(patternLabel(MIRE.kind)), ok, cnStr(targetCueNumber())), 'ok'); }
          }
        }, 'Mire globale'),
        el('button', {
          'data-tip': 'Retire les mires : contenu « aucun » sur tous les slices de la sortie',
          onclick: function () {
            var ok = 0;
            slicesOfOutput().forEach(function (s) { if (assignContent(s.id, 'none', null, true)) { ok++; } });
            if (ok) { toast(trf('Contenu retiré de {0} slice(s) (cue {1})', ok, cnStr(targetCueNumber())), 'ok'); }
          }
        }, 'Éteindre'))));

    /* paramètres du slice */
    if (selSlice) {
      var p = el('div', { class: 'panel' }, el('h2', null, trf('{0} — paramètres', selSlice.name)));
      p.appendChild(paramSlider('slice/' + selSlice.id + '/opacity', 'Opacité', 0, 1, 1));
      p.appendChild(paramSlider('slice/' + selSlice.id + '/gain/r', 'Gain R', 0, 2, 1));
      p.appendChild(paramSlider('slice/' + selSlice.id + '/gain/g', 'Gain V', 0, 2, 1));
      p.appendChild(paramSlider('slice/' + selSlice.id + '/gain/b', 'Gain B', 0, 2, 1));
      p.appendChild(paramSlider('slice/' + selSlice.id + '/gamma', 'Gamma', 0.2, 3, 1));
      side.appendChild(p);
    }

    root.appendChild(side);
    setTimeout(drawMapping, 0);
    return root;
  };

  function paramSlider(addr, label, min, max, def) {
    var input = el('input', {
      type: 'range', min: min, max: max, step: 0.001, value: def,
      'data-tip': trf('{0} — adresse : {1}', tr(label), addr)
    });
    var val = el('span', { class: 'val' }, fmtF(def));
    input.addEventListener('input', function () {
      var v = parseFloat(input.value);
      val.textContent = fmtF(v);
      sendParam(addr, { f: v });
    });
    enhanceSlider(input, val, def);
    var row = el('div', { class: 'param-row' }, el('span', null, label), input, val, animButton(addr));
    attachParamCtx(row, addr, def, input);
    return row;
  }

  /* Menu contextuel d'un curseur de paramètre : réinitialiser, saisir une
     valeur exacte, ouvrir le popover d'animation. Réinitialiser/Saisir sont
     des param_set runtime (autorisés même en mode Show) ; Animer édite une
     route (grisé en Show). */
  function attachParamCtx(row, addr, def, input) {
    onCtx(row, function () {
      return [
        { kind: 'head', label: addr },
        {
          label: trf('Réinitialiser ({0})', fmtF(def)),
          tip: 'Repose la valeur par défaut (équivaut au double-clic sur le curseur)',
          action: function () { sliderSet(input, def); }
        },
        {
          label: 'Saisir une valeur…',
          tip: 'Valeur exacte au clavier (équivaut au clic sur la valeur affichée)',
          action: function () {
            var v = window.prompt(trf('Valeur pour {0} ({1} à {2}) :', addr, input.min, input.max), input.value);
            if (v === null) { return; }
            var f = parseFloat(String(v).replace(',', '.'));
            if (!isFinite(f)) { uiWarn(trf('Valeur invalide : {0}', v)); return; }
            sliderSet(input, f);
          }
        },
        ctxEdit({
          label: 'Animer…',
          tip: 'Brancher un LFO ou une bande FFT sur ce paramètre (icône ⟳)',
          action: function () {
            var anchor = row.querySelector('.anim-btn') || row;
            openAnimPopover(addr, anchor);
          }
        })
      ];
    });
  }

  function currentOutput() {
    return outputs().find(function (o) { return o.id === S.sel.output; }) || null;
  }
  function slicesOfOutput() {
    return slices().filter(function (s) { return s.output === S.sel.output; });
  }
  function currentSlice() {
    return slices().find(function (s) { return s.id === S.sel.slice; }) || null;
  }

  function drawMapping() {
    var cv = byId('map-canvas');
    if (!cv) { return; }
    var ctx = cv.getContext('2d');
    var W = cv.width, H = cv.height;
    ctx.clearRect(0, 0, W, H);
    /* tiers */
    ctx.strokeStyle = '#1c212b';
    ctx.lineWidth = 1;
    for (var i = 1; i < 3; i++) {
      ctx.beginPath(); ctx.moveTo(W * i / 3, 0); ctx.lineTo(W * i / 3, H); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(0, H * i / 3); ctx.lineTo(W, H * i / 3); ctx.stroke();
    }
    var list = slicesOfOutput().slice().sort(function (a, b) { return a.z - b.z; });
    list.forEach(function (s) {
      var seld = s.id === S.sel.slice;
      var pts = (s.corners || []).map(function (c) { return [c[0] * W, c[1] * H]; });
      if (pts.length !== 4) { return; }
      ctx.beginPath();
      ctx.moveTo(pts[0][0], pts[0][1]);
      for (var i = 1; i < 4; i++) { ctx.lineTo(pts[i][0], pts[i][1]); }
      ctx.closePath();
      ctx.fillStyle = seld ? 'rgba(79,140,255,0.18)' : 'rgba(154,160,166,0.10)';
      ctx.fill();
      ctx.strokeStyle = seld ? '#4f8cff' : '#5a6270';
      ctx.lineWidth = seld ? 2 : 1;
      ctx.stroke();
      /* poignées */
      pts.forEach(function (p, i) {
        ctx.fillStyle = (seld && S.sel.corner === i) ? '#f0a020' : (seld ? '#4f8cff' : '#5a6270');
        ctx.fillRect(p[0] - 5, p[1] - 5, 10, 10);
      });
      /* label au centroïde */
      var cx = (pts[0][0] + pts[1][0] + pts[2][0] + pts[3][0]) / 4;
      var cy = (pts[0][1] + pts[1][1] + pts[2][1] + pts[3][1]) / 4;
      ctx.fillStyle = seld ? '#e8eaed' : '#9aa0a6';
      ctx.font = '12.5px system-ui';
      ctx.textAlign = 'center';
      ctx.fillText(s.name || trf('slice {0}', s.id), cx, cy);
    });
  }

  function installMapMouse(cv) {
    var drag = null; // {sliceId, corner}
    function pos(e) {
      var r = cv.getBoundingClientRect();
      return {
        x: (e.clientX - r.left) * cv.width / r.width,
        y: (e.clientY - r.top) * cv.height / r.height
      };
    }
    /* Pointer Events + setPointerCapture : le drag survit à la sortie du
       canvas (cas fréquent : coin calé au bord, x=0 ou 1) — les move/up
       continuent d'arriver même hors canvas, le coin ne « lâche » plus. */
    cv.addEventListener('pointerdown', function (e) {
      if (e.button !== 0) { return; }
      var p = pos(e);
      /* poignée d'abord (slice sélectionné prioritaire) */
      var hit = hitCorner(p, currentSlice()) || hitCornerAny(p);
      if (hit) {
        S.sel.slice = hit.slice.id;
        S.sel.corner = hit.corner;
        if (!isShowMode()) {
          drag = { slice: hit.slice, corner: hit.corner };
          S.dragging = true;
          try { cv.setPointerCapture(e.pointerId); } catch (err) { /* capture indisponible */ }
        }
        drawMapping();
        return;
      }
      var inside = slicesOfOutput().slice().sort(function (a, b) { return b.z - a.z; })
        .find(function (s) { return pointInQuad(p, s, cv); });
      S.sel.slice = inside ? inside.id : null;
      S.sel.corner = null;
      renderMain();
    });
    cv.addEventListener('pointermove', function (e) {
      if (!drag) { return; }
      var p = pos(e);
      var nx = clamp(p.x / cv.width, 0, 1), ny = clamp(p.y / cv.height, 0, 1);
      drag.slice.corners[drag.corner] = [nx, ny];
      drawMapping();
      var now = Date.now();
      if (now - MAP.throttleTs > 80) {
        MAP.throttleTs = now;
        sendCornerSet(drag.slice.id, drag.corner, nx, ny);
      }
    });
    function endDrag() {
      if (drag) {
        var c = drag.slice.corners[drag.corner];
        sendCornerSet(drag.slice.id, drag.corner, c[0], c[1]);
      }
      drag = null;
      S.dragging = false;
    }
    cv.addEventListener('pointerup', endDrag);
    cv.addEventListener('pointercancel', endDrag);

    /* clic droit : menu contextuel du slice sous le curseur */
    onCtx(cv, function (e) {
      var p = pos(e);
      var s = slicesOfOutput().slice().sort(function (a, b) { return b.z - a.z; })
        .find(function (x) { return pointInQuad(p, x, cv); });
      if (!s) { return null; }
      if (S.sel.slice !== s.id) {
        S.sel.slice = s.id;
        S.sel.corner = null;
        drawMapping();
      }
      return sliceCtxItems(s);
    });

    function hitCorner(p, s) {
      if (!s) { return null; }
      for (var i = 0; i < 4; i++) {
        var c = s.corners[i];
        if (Math.abs(c[0] * cv.width - p.x) < 12 && Math.abs(c[1] * cv.height - p.y) < 12) {
          return { slice: s, corner: i };
        }
      }
      return null;
    }
    function hitCornerAny(p) {
      var list = slicesOfOutput();
      for (var j = 0; j < list.length; j++) {
        var h = hitCorner(p, list[j]);
        if (h) { return h; }
      }
      return null;
    }
  }

  function pointInQuad(p, s, cv) {
    var pts = (s.corners || []).map(function (c) { return [c[0] * cv.width, c[1] * cv.height]; });
    if (pts.length !== 4) { return false; }
    var inside = false;
    for (var i = 0, j = 3; i < 4; j = i++) {
      var xi = pts[i][0], yi = pts[i][1], xj = pts[j][0], yj = pts[j][1];
      if ((yi > p.y) !== (yj > p.y) && p.x < (xj - xi) * (p.y - yi) / (yj - yi) + xi) {
        inside = !inside;
      }
    }
    return inside;
  }

  function sendCornerSet(sliceId, index, x, y) {
    sendEdit({ op: 'corner_set', slice: sliceId, index: index, x: x, y: y });
  }

  function mappingKey(e) {
    var s = currentSlice();
    var out = currentOutput();
    if (!s || !out) { return; }
    var dir = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!dir) { return; }
    e.preventDefault();
    var mult = e.shiftKey ? 10 : (e.altKey ? 0.1 : 1);
    var dx = dir[0] * mult / Math.max(1, out.width || 1920);
    var dy = dir[1] * mult / Math.max(1, out.height || 1080);
    var idxs = (S.sel.corner === null || S.sel.corner === undefined) ? [0, 1, 2, 3] : [S.sel.corner];
    idxs.forEach(function (i) {
      var c = s.corners[i];
      c[0] = clamp(c[0] + dx, 0, 1);
      c[1] = clamp(c[1] + dy, 0, 1);
      sendCornerSet(s.id, i, c[0], c[1]);
    });
    drawMapping();
  }

  /* =============================================================== MÉDIAS */

  /* Menu contextuel d'une carte média. */
  function mediaCtxItems(m) {
    var sliceOk = S.sel.slice !== null;
    var sliceName = sliceOk ? ((currentSlice() || {}).name || trf('slice {0}', S.sel.slice)) : '';
    return [
      { kind: 'head', label: m.name || m.path },
      ctxEdit({
        label: sliceOk ? trf('Assigner au slice « {0} »', sliceName) : tr('Assigner au slice'),
        disabled: !sliceOk,
        reason: 'Sélectionnez d’abord un slice (onglet Mapping)',
        tip: 'Pose ce média sur le slice sélectionné, dans la cue standby/active',
        action: function () {
          S.sel.media = m.id;
          assignContent(S.sel.slice, { media: m.id }, defaultPlayback());
        }
      }),
      { kind: 'sep' },
      ctxEdit({
        label: 'Relocaliser…',
        tip: m.missing
          ? 'Le fichier est introuvable — indiquer son nouveau chemin (relatif à media/)'
          : 'Modifier le chemin du fichier (relatif à media/)',
        action: function () {
          var v = window.prompt(tr('Chemin du fichier, relatif au dossier media/ :'), m.path || '');
          if (v === null) { return; }
          v = v.trim();
          if (!v || v === m.path) { return; }
          var copy = JSON.parse(JSON.stringify(m));
          copy.path = v;
          if (sendEdit({ op: 'media_update', media: copy })) {
            uiInfo('Chemin mis à jour — « Re-scanner » vérifie le fichier et régénère la vignette.');
          }
        }
      }),
      ctxEdit({
        label: 'Régénérer la vignette',
        tip: 'Relance le scan de media/ : vignettes et états manquants sont rafraîchis',
        action: function () {
          if (sendCmd({ cmd: 'media_rescan' })) {
            uiInfo('Re-scan lancé — vignettes en cours de régénération.');
          }
        }
      }),
      { kind: 'sep' },
      ctxEdit({
        label: 'Retirer du pool', danger: true,
        tip: 'Retire le média du show (le fichier reste sur disque)',
        action: function () {
          confirmDialog({
            title: trf('Retirer « {0} » du pool ?', m.name || m.path),
            message: 'Le fichier reste dans media/ ; les cues qui l’utilisent afficheront un contenu manquant. Annulable avec Ctrl+Z.',
            confirm: 'Retirer',
            onConfirm: function () {
              sendEdit({ op: 'media_remove', id: m.id });
              if (S.sel.media === m.id) { S.sel.media = null; }
            }
          });
        }
      })
    ];
  }

  RENDERERS.medias = function () {
    var root = el('section', { class: 'tab-panel' });
    var panel = el('div', { class: 'panel' }, el('h2', null, 'Pool de médias'));

    panel.appendChild(el('div', { class: 'toolbar' },
      el('button', {
        class: 'edit-only', 'data-tip': 'Re-scanne le dossier media/ : nouveaux fichiers, vignettes, état manquant',
        onclick: function () {
          if (sendCmd({ cmd: 'media_rescan' })) {
            uiInfo('Re-scan des dossiers media/ et shaders/ lancé.');
          }
        }
      }, 'Re-scanner'),
      el('button', {
        class: 'primary edit-only',
        disabled: S.sel.media === null || S.sel.slice === null,
        'data-tip': S.sel.slice === null
          ? 'Sélectionnez d’abord un slice (onglet Mapping)'
          : 'Assigne le média sélectionné au slice sélectionné, dans la cue standby/active',
        onclick: function () {
          if (S.sel.media !== null && S.sel.slice !== null) {
            assignContent(S.sel.slice, { media: S.sel.media }, defaultPlayback());
          }
        }
      }, S.sel.slice !== null ? trf('Assigner au slice « {0} »', (currentSlice() || {}).name) : tr('Assigner au slice')),
      el('span', { class: 'muted' },
        S.sel.media !== null ? trf('Sélection : {0}', contentLabel({ media: S.sel.media })) : 'Aucun média sélectionné')));

    var grid = el('div', { id: 'media-grid' });
    medias().forEach(function (m) {
      var img = el('img', { class: 'thumb', src: '/thumb/' + m.id + '.jpg', alt: m.name });
      img.addEventListener('error', function () {
        var ph = el('div', { class: 'thumb-placeholder' }, m.missing ? 'MANQUANT' : 'pas de vignette');
        if (img.parentNode) { img.parentNode.replaceChild(ph, img); }
      });
      var card = el('div', {
        class: 'media-card' + (m.id === S.sel.media ? ' selected' : '') + (m.missing ? ' missing' : ''),
        role: 'button', tabindex: '0', 'aria-label': m.name || m.path,
        'data-tip': m.path + (m.missing ? tr(' — FICHIER MANQUANT') : '') + tr(' — clic : sélectionner, double-clic : assigner au slice')
      },
        img,
        m.missing ? el('span', { class: 'badge-missing' }, 'MANQUANT') : null,
        el('div', { class: 'media-name' }, m.name),
        el('div', { class: 'media-meta' },
          m.missing ? tr('fichier introuvable') :
            (m.width + '×' + m.height + (m.duration_s ? ' • ' + fmtF(m.duration_s, 1) + ' s' : ''))));
      card.addEventListener('click', function () { S.sel.media = m.id; renderMain(); });
      card.addEventListener('dblclick', function () {
        S.sel.media = m.id;
        if (S.sel.slice !== null) { assignContent(S.sel.slice, { media: m.id }, defaultPlayback()); }
      });
      card.addEventListener('keydown', function (e) {
        if (e.key === 'Enter') { e.preventDefault(); S.sel.media = m.id; renderMain(); }
      });
      onCtx(card, function () { return mediaCtxItems(m); });
      grid.appendChild(card);
    });
    if (!medias().length) {
      grid.appendChild(emptyState('film', 'Aucun média — déposez des fichiers dans media/ puis re-scannez.',
        { label: 'Re-scanner media/', onclick: function () { sendCmd({ cmd: 'media_rescan' }); } }));
    }
    panel.appendChild(grid);
    root.appendChild(panel);
    return root;
  };

  /* ============================================================ MATÉRIAUX */

  RENDERERS.materiaux = function () {
    var root = el('section', { class: 'tab-panel two-col' });

    var left = el('div', { class: 'panel' }, el('h2', null, 'Matériaux (ISF / GLSL)'));
    left.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Re-scanne shaders/ (et media/) : nouveaux .fs, matériaux disparus, vignettes',
        onclick: function () {
          if (sendCmd({ cmd: 'media_rescan' })) {
            uiInfo('Re-scan des dossiers media/ et shaders/ lancé.');
          }
        }
      }, 'Re-scanner'),
      el('button', {
        class: 'primary', disabled: S.sel.material === null || S.sel.slice === null,
        'data-tip': S.sel.slice === null
          ? 'Sélectionnez d’abord un slice (onglet Mapping)'
          : 'Assigne le matériau sélectionné au slice sélectionné, dans la cue standby/active',
        onclick: function () {
          if (S.sel.material !== null && S.sel.slice !== null) {
            assignContent(S.sel.slice, { material: S.sel.material });
          }
        }
      }, S.sel.slice !== null ? trf('Assigner au slice « {0} »', (currentSlice() || {}).name) : tr('Assigner au slice'))));

    var table = el('table', { class: 'grid' },
      el('tr', null, el('th', null, 'Nom'), el('th', null, 'Fichier')));
    materials().forEach(function (m) {
      var row = el('tr', {
        class: m.id === S.sel.material ? 'selected' : '',
        'data-tip': 'Clic : sélectionner ce matériau'
      },
        el('td', null, m.name), el('td', { class: 'muted' }, m.path));
      row.addEventListener('click', function () { S.sel.material = m.id; renderMain(); });
      table.appendChild(row);
    });
    left.appendChild(table);
    if (!materials().length) {
      left.appendChild(el('div', { style: 'padding:10px 0 0' },
        emptyState('sparkle', 'Aucun matériau — déposez des .fs (ISF) dans shaders/ puis re-scannez.')));
    }
    root.appendChild(left);

    /* paramètres ISF du slice sélectionné */
    var right = el('div', { class: 'panel' }, el('h2', null, 'Paramètres du matériau'));
    var slice = currentSlice();
    var matId = null;
    if (slice) {
      var target = cues().find(function (c) { return c.number === targetCueNumber(); });
      var st = target && (target.states || []).find(function (x) { return x.slice === slice.id; });
      if (st && st.content && typeof st.content === 'object' && 'material' in st.content) {
        matId = st.content.material;
      }
    }
    if (matId === null && S.sel.material !== null) { matId = S.sel.material; }
    if (matId === null) {
      right.appendChild(el('div', { class: 'muted' },
        'Sélectionnez un matériau (ou un slice qui en porte un) pour éditer ses paramètres.'));
    } else {
      var prefix = 'material/' + matId + '/';
      var found = specs().filter(function (sp) { return sp && sp.addr && sp.addr.indexOf(prefix) === 0; });
      if (!found.length) {
        /* message orienté utilisateur ; le détail technique (fichier,
           adresses, dernières erreurs ISF du journal) reste repliable */
        var mat = materials().find(function (x) { return x.id === matId; }) || null;
        var isfLogs = S.logs.filter(function (l) {
          var lvl = (l.level || '').toLowerCase();
          if (lvl !== 'error' && lvl !== 'warn') { return false; }
          var hay = (l.target || '') + ' ' + (l.message || '');
          return /isf|shader/i.test(hay) || (mat && mat.path && hay.indexOf(mat.path) >= 0);
        }).slice(-5);
        right.appendChild(el('div', { class: 'isf-banner' },
          el('div', { class: 'isf-title' }, 'Réglages du shader indisponibles'),
          el('div', null,
            isfLogs.length
              ? 'Ce shader semble poser problème au chargement : il peut s’afficher avec ses valeurs par défaut, ou pas du tout. Corrigez le fichier puis « Re-scanner ».'
              : 'Le moteur n’a pas (encore) publié les réglages de ce matériau. Il s’affiche avec ses valeurs par défaut ; si le fichier vient d’être ajouté, « Re-scanner ».')));
        right.appendChild(el('details', { class: 'tech' },
          el('summary', null, 'Détail technique'),
          el('pre', null,
            trf('Fichier : shaders/{0}', mat ? mat.path : '?') + '\n' +
            trf('Adresses de paramètres attendues : {0}<input>', prefix) + '\n' +
            (isfLogs.length
              ? (tr('Journal (dernières lignes ISF/shader) :') + '\n' + isfLogs.map(function (l) {
                  return '[' + l.level + '] ' + (l.target || '') + ' — ' + (l.message || '');
                }).join('\n'))
              : 'Aucune erreur ISF/shader dans le journal récent.'))));
      } else {
        found.forEach(function (sp) { right.appendChild(specControl(sp)); });
      }
    }
    root.appendChild(right);
    return root;
  };

  /* Contrôle générique depuis un ParamSpec {addr,label,kind,default}. */
  function specControl(sp) {
    var kind = sp.kind;
    var kname = typeof kind === 'string' ? kind.toLowerCase() : Object.keys(kind || {})[0];
    kname = (kname || '').toLowerCase();
    var label = sp.label || sp.addr;
    var body = kind && typeof kind === 'object' ? kind[Object.keys(kind)[0]] : {};

    if (kname === 'float') {
      var min = body && typeof body.min === 'number' ? body.min : 0;
      var max = body && typeof body.max === 'number' ? body.max : 1;
      var def = sp.default && typeof sp.default === 'object' && 'f' in sp.default ? sp.default.f : min;
      return paramSliderRange(sp.addr, label, min, max, def);
    }
    if (kname === 'bool') {
      var cb = el('input', { type: 'checkbox', 'data-tip': sp.addr });
      if (sp.default && sp.default.b) { cb.checked = true; }
      cb.addEventListener('change', function () { sendParam(sp.addr, { b: cb.checked }); });
      return el('div', { class: 'param-row' }, el('span', null, label), cb, el('span'));
    }
    if (kname === 'color') {
      var color = el('input', { type: 'color', value: '#ffffff', 'data-tip': sp.addr });
      color.addEventListener('change', function () {
        var h = color.value;
        var r = parseInt(h.slice(1, 3), 16) / 255;
        var g = parseInt(h.slice(3, 5), 16) / 255;
        var b = parseInt(h.slice(5, 7), 16) / 255;
        sendParam(sp.addr, { color: [r, g, b, 1] });
      });
      return el('div', { class: 'param-row' }, el('span', null, label), color, el('span'));
    }
    if (kname === 'int') {
      var num = el('input', { type: 'number', step: 1, 'data-tip': sp.addr });
      if (sp.default && typeof sp.default === 'object' && 'i' in sp.default) { num.value = sp.default.i; }
      num.addEventListener('change', function () { sendParam(sp.addr, { i: parseInt(num.value, 10) || 0 }); });
      return el('div', { class: 'param-row' }, el('span', null, label), num, el('span'));
    }
    if (kname === 'enum') {
      var opts = Array.isArray(body) ? body : [];
      var s = sel(opts.map(function (_, i) { return String(i); }), opts, '0', function (v) {
        sendParam(sp.addr, { i: parseInt(v, 10) || 0 });
      });
      s.setAttribute('data-tip', sp.addr);
      return el('div', { class: 'param-row' }, el('span', null, label), s, el('span'));
    }
    if (kname === 'point2') {
      var x = el('input', { type: 'number', step: 0.01, value: 0.5, style: 'width:64px' });
      var y = el('input', { type: 'number', step: 0.01, value: 0.5, style: 'width:64px' });
      function sendP2() { sendParam(sp.addr, { p2: [parseFloat(x.value) || 0, parseFloat(y.value) || 0] }); }
      x.addEventListener('change', sendP2);
      y.addEventListener('change', sendP2);
      return el('div', { class: 'param-row', 'data-tip': sp.addr }, el('span', null, label), el('span', null, x, y), el('span'));
    }
    /* inconnu : champ libre */
    var raw = el('input', { type: 'text', placeholder: 'valeur', 'data-tip': sp.addr });
    raw.addEventListener('change', function () { sendParam(sp.addr, { s: raw.value }); });
    return el('div', { class: 'param-row' }, el('span', null, label), raw, el('span'));
  }

  function paramSliderRange(addr, label, min, max, def) {
    var input = el('input', { type: 'range', min: min, max: max, step: (max - min) / 1000, value: def, 'data-tip': addr });
    var val = el('span', { class: 'val' }, fmtF(def));
    input.addEventListener('input', function () {
      var v = parseFloat(input.value);
      val.textContent = fmtF(v);
      sendParam(addr, { f: v });
    });
    enhanceSlider(input, val, def);
    var row = el('div', { class: 'param-row' }, el('span', null, label), input, val, animButton(addr));
    attachParamCtx(row, addr, def, input);
    return row;
  }

  /* =========================================================== MODULATION */

  /* Couleur stable par modulateur (surimpression spectre, icônes ⟳, badges). */
  var MOD_COLORS = ['#4da3ff', '#f0b232', '#43c47f', '#c678dd', '#ff8fab', '#4fd8d0', '#ff9f4d', '#a3e635'];

  function modColor(id) {
    var n = parseInt(id, 10);
    return MOD_COLORS[Math.abs(isFinite(n) ? n : 0) % MOD_COLORS.length];
  }

  function hexToRgba(hex, a) {
    var r = parseInt(hex.slice(1, 3), 16), g = parseInt(hex.slice(3, 5), 16), b = parseInt(hex.slice(5, 7), 16);
    return 'rgba(' + r + ',' + g + ',' + b + ',' + a + ')';
  }

  function isLfoMod(m) { return !!(m && m.kind && typeof m.kind === 'object' && 'lfo' in m.kind); }
  function isBandMod(m) { return !!(m && m.kind && typeof m.kind === 'object' && 'audio_band' in m.kind); }
  function modById(id) { return modulators().find(function (m) { return m.id === id; }) || null; }
  function nextModId() { return modulators().reduce(function (mx, x) { return Math.max(mx, x.id); }, 0) + 1; }
  function nextRouteId() { return routes().reduce(function (mx, r) { return Math.max(mx, r.id); }, 0) + 1; }
  function routesFor(addr) { return routes().filter(function (r) { return r.target_addr === addr; }); }

  /* Périphériques d'entrée audio publiés par le moteur :
     runtime.audio_devices = { available: [noms…], active: nom|null }
     (tolère aussi une liste plate, par prudence). */
  function audioDevices() {
    var d = rt().audio_devices;
    var list = Array.isArray(d) ? d : ((d && Array.isArray(d.available)) ? d.available : []);
    return list.map(function (x) { return typeof x === 'string' ? x : ((x && x.name) || ''); })
      .filter(function (n) { return !!n; });
  }

  function activeAudioDevice() {
    var d = rt().audio_devices;
    return (d && !Array.isArray(d) && typeof d.active === 'string') ? d.active : null;
  }

  function newLfoCfg(id) {
    return { id: id, name: 'LFO ' + id, kind: { lfo: { wave: 'sine', freq: { hz: 1 }, phase: 0 } } };
  }

  function newBandCfg(id) {
    return {
      id: id, name: 'Bande ' + id,
      kind: { audio_band: { low_hz: 60, high_hz: 120, gain: 1, floor: 0.05, attack_ms: 10, release_ms: 200 } }
    };
  }

  /* ------------------------------------------- analyseur de spectre (canvas)
     64 barres log 20 Hz → 16 kHz (dyn.fft.bins) + bandes AudioBand dessinées
     en surimpression avec poignées draggables (pointer capture). */

  var FFT_FMIN = 20, FFT_FMAX = 16000;
  var SPEC = { disp: null, drag: null, throttleTs: 0, raf: 0, status: null };

  function freqToNorm(f) {
    return clamp(Math.log(Math.max(f, 1) / FFT_FMIN) / Math.log(FFT_FMAX / FFT_FMIN), 0, 1);
  }
  function normToFreq(x) {
    return FFT_FMIN * Math.pow(FFT_FMAX / FFT_FMIN, clamp(x, 0, 1));
  }

  function drawSpectrum(cv) {
    var ctx = cv.getContext('2d');
    if (!ctx) { return; }
    var W = cv.width, H = cv.height, AX = 20;   /* AX : bande d'axe en bas */
    var PH = H - AX;
    ctx.clearRect(0, 0, W, H);
    ctx.fillStyle = '#05070a';
    ctx.fillRect(0, 0, W, H);

    /* grille horizontale discrète */
    ctx.strokeStyle = '#141a23';
    ctx.lineWidth = 1;
    [0.25, 0.5, 0.75].forEach(function (fr) {
      ctx.beginPath(); ctx.moveTo(0, PH * fr); ctx.lineTo(W, PH * fr); ctx.stroke();
    });

    /* axe log annoté */
    ctx.font = '11px system-ui, sans-serif';
    ctx.textAlign = 'center';
    [[50, '50 Hz'], [200, '200 Hz'], [1000, '1 kHz'], [5000, '5 kHz'], [15000, '15 kHz']]
      .forEach(function (pair) {
        var x = freqToNorm(pair[0]) * W;
        ctx.strokeStyle = '#1a2130';
        ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, PH); ctx.stroke();
        ctx.fillStyle = '#747e8e';
        ctx.fillText(pair[1], x, H - 6);
      });

    /* barres — lissage attaque rapide / retombée douce (lecture agréable) */
    var bins = (S.fft && Array.isArray(S.fft.bins)) ? S.fft.bins : null;
    var n = bins ? bins.length : 64;
    if (!SPEC.disp || SPEC.disp.length !== n) {
      SPEC.disp = [];
      for (var k = 0; k < n; k++) { SPEC.disp.push(0); }
    }
    var bw = W / n;
    var grad = ctx.createLinearGradient(0, PH, 0, 0);
    grad.addColorStop(0, 'rgba(77, 163, 255, 0.30)');
    grad.addColorStop(0.65, '#4da3ff');
    grad.addColorStop(1, '#85c2ff');
    ctx.fillStyle = grad;
    for (var i = 0; i < n; i++) {
      var t = bins ? clamp(+bins[i] || 0, 0, 1) : 0;
      var d = SPEC.disp[i];
      d += (t - d) * (t > d ? 0.55 : 0.16);
      SPEC.disp[i] = d;
      var h = d * (PH - 2);
      if (h > 0.5) { ctx.fillRect(i * bw + 1, PH - h, Math.max(1, bw - 2), h); }
    }

    /* bandes AudioBand en surimpression, avec poignées */
    modulators().filter(isBandMod).forEach(function (m) {
      var ab = m.kind.audio_band || {};
      var x1 = freqToNorm(ab.low_hz || FFT_FMIN) * W;
      var x2 = freqToNorm(ab.high_hz || FFT_FMAX) * W;
      var col = modColor(m.id);
      ctx.fillStyle = hexToRgba(col, 0.14);
      ctx.fillRect(x1, 0, Math.max(1, x2 - x1), PH);
      ctx.fillStyle = hexToRgba(col, 0.85);
      ctx.fillRect(x1 - 1, 0, 2, PH);
      ctx.fillRect(x2 - 1, 0, 2, PH);
      drawBandHandle(ctx, x1, PH, col, SPEC.drag && SPEC.drag.id === m.id && SPEC.drag.edge === 'lo');
      drawBandHandle(ctx, x2, PH, col, SPEC.drag && SPEC.drag.id === m.id && SPEC.drag.edge === 'hi');
      ctx.fillStyle = col;
      ctx.textAlign = 'left';
      ctx.font = '11px system-ui, sans-serif';
      ctx.fillText(
        (m.name || ('Bande ' + m.id)) + '  ' + Math.round(ab.low_hz || 0) + '–' + Math.round(ab.high_hz || 0) + ' Hz',
        Math.min(x1 + 6, W - 130), 14);
    });
  }

  function drawBandHandle(ctx, x, ph, col, active) {
    var w = active ? 12 : 9, h = 30, y = ph / 2 - h / 2;
    ctx.fillStyle = active ? col : hexToRgba(col, 0.9);
    ctx.beginPath();
    if (ctx.roundRect) { ctx.roundRect(x - w / 2, y, w, h, 3); ctx.fill(); }
    else { ctx.fillRect(x - w / 2, y, w, h); }
    ctx.strokeStyle = 'rgba(0,0,0,0.55)';
    ctx.lineWidth = 1;
    for (var i = -1; i <= 1; i++) {
      ctx.beginPath();
      ctx.moveTo(x + i * 2.5, y + 9);
      ctx.lineTo(x + i * 2.5, y + h - 9);
      ctx.stroke();
    }
  }

  /* Drag des poignées de bande : pointer capture, écart mini 10 Hz,
     EditOp modulator_update throttlé pendant le geste + commit final. */
  function installSpectrumPointer(cv) {
    function pos(e) {
      var r = cv.getBoundingClientRect();
      return { x: (e.clientX - r.left) * cv.width / r.width, y: (e.clientY - r.top) * cv.height / r.height };
    }
    function hitHandle(p) {
      var best = null, bestDist = 10;   /* tolérance en px canvas */
      modulators().filter(isBandMod).forEach(function (m) {
        var ab = m.kind.audio_band || {};
        var candidates = [
          { edge: 'lo', x: freqToNorm(ab.low_hz || FFT_FMIN) * cv.width },
          { edge: 'hi', x: freqToNorm(ab.high_hz || FFT_FMAX) * cv.width }
        ];
        candidates.forEach(function (c) {
          var d = Math.abs(p.x - c.x);
          if (d <= bestDist) { bestDist = d; best = { id: m.id, edge: c.edge }; }
        });
      });
      return best;
    }
    function sendBand(m) {
      sendEdit({ op: 'modulator_update', modulator: JSON.parse(JSON.stringify(m)) });
    }
    cv.addEventListener('pointerdown', function (e) {
      if (e.button !== 0 || isShowMode()) { return; }
      var h = hitHandle(pos(e));
      if (!h) { return; }
      SPEC.drag = h;
      S.dragging = true;
      try { cv.setPointerCapture(e.pointerId); } catch (err) { /* capture indisponible */ }
      e.preventDefault();
    });
    cv.addEventListener('pointermove', function (e) {
      var p = pos(e);
      if (!SPEC.drag) {
        cv.style.cursor = hitHandle(p) ? 'ew-resize' : 'default';
        return;
      }
      var m = modById(SPEC.drag.id);
      if (!m || !isBandMod(m)) { return; }
      var ab = m.kind.audio_band;
      var f = Math.round(normToFreq(p.x / cv.width));
      if (SPEC.drag.edge === 'lo') {
        ab.low_hz = clamp(f, FFT_FMIN, (ab.high_hz || FFT_FMAX) - 10);
      } else {
        ab.high_hz = clamp(f, (ab.low_hz || FFT_FMIN) + 10, FFT_FMAX);
      }
      syncBandInputs(m);
      var now = Date.now();
      if (now - SPEC.throttleTs > 80) {
        SPEC.throttleTs = now;
        sendBand(m);
      }
    });
    function endDrag() {
      if (SPEC.drag) {
        var m = modById(SPEC.drag.id);
        if (m && isBandMod(m)) { sendBand(m); }
      }
      SPEC.drag = null;
      S.dragging = false;
    }
    cv.addEventListener('pointerup', endDrag);
    cv.addEventListener('pointercancel', endDrag);
  }

  function syncBandInputs(m) {
    var ab = (m.kind && m.kind.audio_band) || {};
    var lo = byId('band-lo-' + m.id), hi = byId('band-hi-' + m.id);
    if (lo && document.activeElement !== lo) { lo.value = Math.round(ab.low_hz || 0); }
    if (hi && document.activeElement !== hi) { hi.value = Math.round(ab.high_hz || 0); }
  }

  /* Statut d'entrée audio (chip + voile sur le spectre) — mis à jour à chaque
     frame mais ne touche le DOM que sur changement. */
  function updateSpectrumStatus() {
    var key = S.fft ? ('ok:' + (S.fft.device || '')) : 'none';
    if (key === SPEC.status) { return; }
    SPEC.status = key;
    var ov = byId('spectrum-overlay');
    if (ov) { ov.classList.toggle('hidden', !!S.fft); }
    var chipEl = byId('spectrum-status');
    if (chipEl) {
      chipEl.className = 'chip ' + (S.fft ? 'ok' : 'warn');
      chipEl.textContent = S.fft
        ? trf('Entrée : {0}', S.fft.device || tr('active'))
        : tr('Aucune entrée audio');
    }
  }

  /* -------------------------------------------------- aperçu LFO (canvas) */

  function lfoDisplayValue(lfo, u, seed) {
    var w = lfo.wave;
    var name = typeof w === 'string' ? w : (Object.keys(w || {})[0] || 'sine');
    var pw = (w && w.square && typeof w.square.pw === 'number') ? w.square.pw : 0.5;
    var p = u - Math.floor(u);
    function rnd(k) {
      var x = Math.sin((k + seed * 17.23) * 12.9898) * 43758.5453;
      return x - Math.floor(x);
    }
    switch (name) {
      case 'tri': return p < 0.5 ? p * 2 : 2 - p * 2;
      case 'square': return p < pw ? 1 : 0;
      case 'saw': return p;
      case 'random_sh': return rnd(Math.floor(u));
      case 'drift': return clamp(0.5 + 0.34 * Math.sin(u * 2 * Math.PI * 0.83 + seed)
        + 0.16 * Math.sin(u * 2 * Math.PI * 2.31 + seed * 1.7), 0, 1);
      default: return 0.5 + 0.5 * Math.sin(p * 2 * Math.PI);
    }
  }

  function drawLfoPreviewCanvas(cv) {
    var id = parseInt(cv.getAttribute('data-mod'), 10);
    var m = modById(id);
    if (!m || !isLfoMod(m)) { return; }
    var lfo = m.kind.lfo || {};
    var freq = lfo.freq || { hz: 1 };
    var hz = (freq && typeof freq === 'object' && 'bpm_sync' in freq)
      ? ((freq.bpm_sync && freq.bpm_sync.mult) || 0) * (rt().bpm || 120) / 60
      : ((freq && typeof freq.hz === 'number') ? freq.hz : 1);
    var ctx = cv.getContext('2d');
    if (!ctx) { return; }
    var W = cv.width, H = cv.height;
    ctx.clearRect(0, 0, W, H);
    ctx.fillStyle = '#05070a';
    ctx.fillRect(0, 0, W, H);
    var col = modColor(id);
    var span = 2;   /* cycles visibles (oscilloscope défilant) */
    var u0 = (performance.now() / 1000) * hz + (lfo.phase || 0);
    ctx.strokeStyle = col;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    var N = 84;
    for (var i = 0; i <= N; i++) {
      var u = hz > 0 ? (u0 - span + span * i / N) : (span * i / N);
      var v = lfoDisplayValue(lfo, u, id);
      var x = i / N * W, y = H - 3 - v * (H - 6);
      if (i === 0) { ctx.moveTo(x, y); } else { ctx.lineTo(x, y); }
    }
    ctx.stroke();
    var vNow = lfoDisplayValue(lfo, hz > 0 ? u0 : 0, id);
    ctx.fillStyle = col;
    ctx.beginPath();
    ctx.arc(W - 3, H - 3 - vNow * (H - 6), 2.5, 0, Math.PI * 2);
    ctx.fill();
  }

  /* Boucle d'animation : tourne tant que l'onglet Modulation affiche un
     canvas (spectre ou aperçu LFO), s'arrête toute seule sinon. */
  function ensureModAnim() {
    if (SPEC.raf) { return; }
    SPEC.raf = requestAnimationFrame(modAnimTick);
  }

  function modAnimTick() {
    SPEC.raf = 0;
    var cv = byId('spectrum-canvas');
    var previews = document.querySelectorAll('canvas.lfo-preview');
    if (!cv && !previews.length) { return; }
    try {
      if (cv) {
        drawSpectrum(cv);
        updateSpectrumStatus();
      }
      previews.forEach(function (p) { drawLfoPreviewCanvas(p); });
    } catch (e) {
      console.error('modAnim', e);
      return;   /* pas de boucle d'erreurs en continu */
    }
    SPEC.raf = requestAnimationFrame(modAnimTick);
  }

  /* ------------------------------------------------------- onglet Modulation */

  RENDERERS.modulation = function () {
    var root = el('section', { class: 'tab-panel', id: 'modulation' });

    /* BPM / tap */
    var bpmIn = el('input', { type: 'number', min: 20, max: 300, step: 0.1, style: 'width:80px', 'data-tip': 'BPM maître — Entrée pour appliquer' });
    bpmIn.value = rt().bpm;
    bpmIn.addEventListener('change', function () {
      var v = parseFloat(bpmIn.value);
      if (isFinite(v) && v > 0) { sendCmd({ cmd: 'bpm_set', bpm: v }); }
    });
    root.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Tempo'),
      el('div', { class: 'toolbar' },
        el('button', {
          class: 'primary', 'data-tip': 'Tap tempo : taper en rythme pour poser le BPM. Raccourci : touche T',
          onclick: function () { sendCmd({ cmd: 'tap_tempo' }); }
        }, 'TAP ', el('kbd', null, 'T')),
        el('span', { id: 'bpm-val' }, fmtF(rt().bpm, 1)),
        el('span', { class: 'muted' }, 'BPM'),
        bpmIn,
        el('span', { class: 'muted', 'data-tip': 'Le tap tempo répond aussi à la touche T, depuis n’importe quel onglet.' },
          el('kbd', null, 'T'), ' = tap tempo'))));

    /* analyseur de spectre */
    var specPanel = el('div', { class: 'panel' }, el('h2', null, 'Analyseur de spectre — entrée audio'));
    var devSel = el('select', {
      id: 'audio-dev-sel',
      'data-tip': 'Périphérique d’entrée audio analysé (FFT). Enregistré dans les réglages du show.'
    });
    fillDeviceSel(devSel);
    devSel.addEventListener('change', function () {
      var copy = JSON.parse(JSON.stringify(settings()));
      copy.audio_input = devSel.value === '' ? null : devSel.value;
      sendEdit({ op: 'settings_update', settings: copy });
    });
    specPanel.appendChild(el('div', { class: 'toolbar' },
      el('span', { class: 'muted' }, 'Entrée audio'),
      devSel,
      el('span', { class: 'chip', id: 'spectrum-status' }, '…'),
      el('span', { class: 'spacer' }),
      el('span', { class: 'muted', 'data-tip': 'Chaque bande FFT apparaît en couleur sur le spectre : glissez ses poignées pour régler ses fréquences (10 Hz d’écart minimum).' },
        'Glissez les poignées pour régler les bandes')));

    var cv = el('canvas', { id: 'spectrum-canvas', width: 1024, height: 264 });
    installSpectrumPointer(cv);
    specPanel.appendChild(el('div', { id: 'spectrum-wrap' },
      cv,
      el('div', { id: 'spectrum-overlay', class: 'spectrum-overlay' },
        el('span', { class: 'spectrum-overlay-title' }, 'Aucune entrée audio'),
        el('span', null, 'Choisir un périphérique dans le sélecteur ci-dessus.'))));

    var bands = modulators().filter(isBandMod);
    if (bands.length) {
      var legend = el('div', { class: 'spec-legend' });
      bands.forEach(function (m) {
        var ab = m.kind.audio_band || {};
        legend.appendChild(el('span', {
          class: 'chip spec-chip', style: 'color:' + modColor(m.id),
          'data-tip': 'Bande « ' + (m.name || m.id) + ' » : ' + Math.round(ab.low_hz || 0) + ' → ' + Math.round(ab.high_hz || 0) + ' Hz'
        }, (m.name || ('Bande ' + m.id))));
      });
      specPanel.appendChild(legend);
    }
    root.appendChild(specPanel);
    SPEC.status = null;   /* force la mise à jour chip/voile au prochain frame */

    /* modulateurs */
    var modPanel = el('div', { class: 'panel' }, el('h2', null, 'Modulateurs'));
    modPanel.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Ajoute un LFO (sinus, 1 Hz) — forme, vitesse et synchro BPM réglables ensuite',
        onclick: function () { sendEdit({ op: 'modulator_add', modulator: newLfoCfg(nextModId()) }); }
      }, '+ LFO'),
      el('button', {
        'data-tip': 'Ajoute une bande d’analyse audio (60–120 Hz) — réglable à la souris sur le spectre',
        onclick: function () { sendEdit({ op: 'modulator_add', modulator: newBandCfg(nextModId()) }); }
      }, '+ Bande audio')));
    modulators().forEach(function (m) { modPanel.appendChild(modulatorCard(m)); });
    if (!modulators().length) {
      modPanel.appendChild(el('div', { style: 'padding:10px 0 0' },
        emptyState('wave', 'Aucun modulateur — « + LFO » ou « + Bande audio » pour animer un paramètre.')));
    }
    root.appendChild(modPanel);

    /* routes */
    var routePanel = el('div', { class: 'panel' }, el('h2', null, 'Routes modulateur → paramètre'));
    routePanel.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        disabled: !modulators().length,
        'data-tip': 'Branche le premier modulateur sur le master (à modifier ensuite). Astuce : l’icône ⟳ à côté de chaque curseur fait la même chose, en mieux.',
        onclick: function () {
          sendEdit({
            op: 'route_add',
            route: { id: nextRouteId(), source: modulators()[0].id, target_addr: 'master/intensity', depth: 0.5, mode: 'add' }
          });
        }
      }, '+ Route')));
    var rtable = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Source'), el('th', null, 'Cible'),
        el('th', { 'data-tip': 'Profondeur par défaut, -1..1 (négatif = inversé ; les cues peuvent la surcharger)' }, 'Profondeur'),
        el('th', { 'data-tip': 'Ajouter : base + signal. Multiplier : atténuation. Remplacer : remplace la base.' }, 'Mode'),
        el('th', { class: 'edit-only' }, '')));
    routes().forEach(function (r) { rtable.appendChild(routeRow(r)); });
    routePanel.appendChild(el('div', { style: 'overflow-x:auto' }, rtable));
    if (!routes().length) {
      routePanel.appendChild(el('div', { style: 'padding:10px 0 0' },
        emptyState('wave', 'Aucune route — un modulateur n’agit que routé vers un paramètre (icône ⟳ des curseurs).')));
    }
    root.appendChild(routePanel);

    setTimeout(ensureModAnim, 0);
    return root;
  };

  function fillDeviceSel(devSel) {
    var devices = audioDevices();
    var current = settings().audio_input || activeAudioDevice() || (S.fft && S.fft.device) || '';
    devSel.textContent = '';
    devSel.appendChild(el('option', { value: '' }, '— aucune —'));
    if (!devices.length) {
      devSel.appendChild(el('option', { value: '', disabled: 'disabled' }, 'Aucun périphérique détecté'));
    }
    devices.forEach(function (d) {
      var o = el('option', { value: d }, d);
      if (d === current) { o.selected = true; }
      devSel.appendChild(o);
    });
    if (current && devices.indexOf(current) < 0) {
      var ghost = el('option', { value: current }, current + ' (absent)');
      ghost.selected = true;
      devSel.appendChild(ghost);
    }
    devSel.setAttribute('data-count', String(devices.length));
  }

  function modulatorCard(m) {
    var isLfo = isLfoMod(m);
    var col = modColor(m.id);
    var card = el('div', { class: 'mod-card' });

    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(m));
      mut(copy);
      sendEdit({ op: 'modulator_update', modulator: copy });
    }

    var name = el('input', { type: 'text', value: m.name || '', style: 'width:150px', 'data-tip': 'Nom du modulateur' });
    name.addEventListener('change', function () { commit(function (x) { x.name = name.value; }); });

    card.appendChild(el('div', { class: 'mod-head' },
      el('span', { class: 'mod-color-dot', style: 'background:' + col + ';color:' + col }),
      name,
      el('span', { class: 'mod-kind' }, isLfo ? 'LFO' : 'Bande FFT'),
      el('span', { class: 'spacer' }),
      el('div', { class: 'mod-meter', 'data-tip': 'Niveau instantané du modulateur (vumètre temps réel)' },
        el('div', { id: 'mod-meter-' + m.id })),
      el('button', {
        class: 'danger edit-only', 'data-tip': 'Supprime ce modulateur (et laisse ses routes orphelines — pensez à les retirer)',
        onclick: function () { sendEdit({ op: 'modulator_remove', id: m.id }); }
      }, '✕')));

    var body = el('div', { class: 'mod-body' });
    if (isLfo) {
      var lfo = m.kind.lfo || {};
      var waveVal = typeof lfo.wave === 'string' ? lfo.wave : 'square';

      /* sélecteur de forme : segments avec mini-icônes SVG */
      var seg = el('div', { class: 'wave-seg', role: 'group' });
      [['sine', 'Sinus'], ['tri', 'Triangle'], ['square', 'Carré'], ['saw', 'Dent de scie'],
       ['random_sh', 'Random S&H'], ['drift', 'Drift (dérive douce)']].forEach(function (pair) {
        var btn = el('button', {
          class: 'wave-btn' + (pair[0] === waveVal ? ' active' : ''),
          'data-tip': trf('Forme d’onde : {0}', tr(pair[1])),
          onclick: function () {
            commit(function (x) {
              var old = x.kind.lfo.wave;
              var pw = (old && old.square && typeof old.square.pw === 'number') ? old.square.pw : 0.5;
              x.kind.lfo.wave = pair[0] === 'square' ? { square: { pw: pw } } : pair[0];
            });
          }
        });
        btn.innerHTML = WAVE_ICONS[pair[0]] || '';
        seg.appendChild(btn);
      });
      body.appendChild(seg);

      /* fréquence : Hz fixes OU synchro BPM avec multiplicateurs préréglés */
      var isBpm = lfo.freq && typeof lfo.freq === 'object' && 'bpm_sync' in lfo.freq;
      var mult = isBpm ? ((lfo.freq.bpm_sync && lfo.freq.bpm_sync.mult) || 1) : 1;
      var hzVal = !isBpm && lfo.freq && typeof lfo.freq.hz === 'number' ? lfo.freq.hz : 1;

      var modeSeg = el('div', { class: 'wave-seg' },
        el('button', {
          class: 'wave-btn' + (isBpm ? '' : ' active'),
          'data-tip': 'Fréquence libre, en Hertz',
          onclick: function () {
            if (!isBpm) { return; }
            commit(function (x) {
              /* continuité : on convertit le multiplicateur en Hz équivalents */
              var hz = Math.round(mult * (rt().bpm || 120) / 60 * 1000) / 1000;
              x.kind.lfo.freq = { hz: hz > 0 ? hz : 1 };
            });
          }
        }, 'Hz'),
        el('button', {
          class: 'wave-btn' + (isBpm ? ' active' : ''),
          'data-tip': 'Synchro sur le BPM maître (tap tempo : touche T)',
          onclick: function () {
            if (isBpm) { return; }
            commit(function (x) { x.kind.lfo.freq = { bpm_sync: { mult: 1 } } });
          }
        }, 'BPM'));
      body.appendChild(modeSeg);

      if (isBpm) {
        var presets = el('div', { class: 'wave-seg bpm-presets' });
        [[0.125, '1/8'], [0.25, '1/4'], [0.5, '1/2'], [1, '1'], [2, '2'], [4, '4']].forEach(function (p) {
          presets.appendChild(el('button', {
            class: 'wave-btn' + (Math.abs(mult - p[0]) < 1e-6 ? ' active' : ''),
            'data-tip': p[0] < 1
              ? trf('Un cycle sur {0} temps', Math.round(1 / p[0]))
              : (p[0] === 1 ? tr('Un cycle par temps') : trf('{0} cycles par temps', p[0])),
            onclick: function () {
              commit(function (x) { x.kind.lfo.freq = { bpm_sync: { mult: p[0] } }; });
            }
          }, '×' + p[1]));
        });
        body.appendChild(presets);
        if ([0.125, 0.25, 0.5, 1, 2, 4].every(function (v) { return Math.abs(mult - v) > 1e-6; })) {
          body.appendChild(el('span', { class: 'muted', 'data-tip': 'Multiplicateur personnalisé (cycles par temps)' },
            '×' + mult));
        }
      } else {
        var fin = el('input', {
          type: 'number', step: 0.01, min: 0.01, value: hzVal,
          style: 'width:76px', 'data-tip': 'Fréquence du LFO en Hertz (cycles par seconde)'
        });
        fin.addEventListener('change', function () {
          var v = parseFloat(fin.value);
          if (!isFinite(v) || v <= 0) { fin.value = hzVal; return; }
          commit(function (x) { x.kind.lfo.freq = { hz: v }; });
        });
        body.appendChild(el('span', { class: 'toolbar', style: 'margin:0' }, fin, el('span', { class: 'muted' }, 'Hz')));
      }

      body.appendChild(el('canvas', {
        class: 'lfo-preview', width: 150, height: 36, 'data-mod': m.id,
        'data-tip': 'Aperçu du LFO — forme et vitesse réelles (les 2 derniers cycles)'
      }));
    } else {
      var ab = (m.kind && m.kind.audio_band) || {};
      var lo = el('input', {
        type: 'number', id: 'band-lo-' + m.id, value: Math.round(ab.low_hz || 0), min: FFT_FMIN, max: FFT_FMAX,
        style: 'width:80px', 'data-tip': 'Borne basse de la bande (Hz) — aussi réglable en glissant la poignée gauche sur le spectre'
      });
      var hi = el('input', {
        type: 'number', id: 'band-hi-' + m.id, value: Math.round(ab.high_hz || 0), min: FFT_FMIN, max: FFT_FMAX,
        style: 'width:80px', 'data-tip': 'Borne haute de la bande (Hz) — aussi réglable en glissant la poignée droite sur le spectre'
      });
      function applyBand() {
        var l = parseFloat(lo.value), h = parseFloat(hi.value);
        if (!isFinite(l)) { l = ab.low_hz || 60; }
        if (!isFinite(h)) { h = ab.high_hz || 120; }
        l = clamp(l, FFT_FMIN, FFT_FMAX - 10);
        h = clamp(h, l + 10, FFT_FMAX);
        lo.value = Math.round(l); hi.value = Math.round(h);
        commit(function (x) {
          x.kind.audio_band.low_hz = l;
          x.kind.audio_band.high_hz = h;
        });
      }
      lo.addEventListener('change', applyBand);
      hi.addEventListener('change', applyBand);
      body.appendChild(el('span', { class: 'toolbar', style: 'margin:0' },
        lo, el('span', { class: 'muted' }, '→'), hi, el('span', { class: 'muted' }, 'Hz')));
      body.appendChild(el('span', { class: 'muted' },
        'Glissez la bande colorée sur le spectre ci-dessus pour la régler à l’oreille.'));
    }
    card.appendChild(body);
    return card;
  }

  function routeRow(r) {
    var row = el('tr');
    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(r));
      mut(copy);
      sendEdit({ op: 'route_update', route: copy });
    }
    var src = sel(
      modulators().map(function (m) { return String(m.id); }),
      modulators().map(function (m) { return m.name; }),
      String(r.source),
      function (v) { commit(function (x) { x.source = parseInt(v, 10); }); });
    src.setAttribute('data-tip', 'Modulateur source');
    row.appendChild(el('td', null, el('span', { class: 'toolbar', style: 'margin:0;flex-wrap:nowrap' },
      el('span', { class: 'mod-color-dot', style: 'background:' + modColor(r.source) + ';color:' + modColor(r.source) }),
      src)));

    var addrs = paramAddrs();
    if (addrs.indexOf(r.target_addr) < 0) { addrs.unshift(r.target_addr); }
    var tgt = sel(addrs, addrs, r.target_addr, function (v) { commit(function (x) { x.target_addr = v; }); });
    tgt.setAttribute('data-tip', 'Paramètre cible (adresse stable)');
    row.appendChild(el('td', null, tgt));

    var depth = el('input', {
      type: 'range', min: -1, max: 1, step: 0.01, value: r.depth,
      'data-tip': 'Profondeur -1..1 — négatif = signal inversé'
    });
    var dval = el('span', { class: 'val' }, fmtF(r.depth));
    depth.addEventListener('input', function () { dval.textContent = fmtF(parseFloat(depth.value)); });
    depth.addEventListener('change', function () { commit(function (x) { x.depth = parseFloat(depth.value); }); });
    enhanceSlider(depth, dval, 0.5);
    row.appendChild(el('td', null, el('span', { class: 'toolbar' }, depth, dval)));

    var mode = sel(['add', 'mul', 'replace'], ['Ajouter', 'Multiplier', 'Remplacer'], r.mode,
      function (v) { commit(function (x) { x.mode = v; }); });
    row.appendChild(el('td', null, mode));

    row.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime cette route',
        onclick: function () { sendEdit({ op: 'route_remove', id: r.id }); }
      }, '✕')));
    return row;
  }

  /* ============================================= animation par paramètre
     Modèle Resolume : chaque curseur porte une icône ⟳ qui ouvre un popover
     « Animation » — brancher un LFO ou une bande FFT (ModRoute) sur l'adresse
     du paramètre, régler profondeur/mode, lister/supprimer les routes. */

  var POP = { addr: null, el: null, x: 0, y: 0 };

  function animButton(addr) {
    var rs = routesFor(addr);
    var names = rs.map(function (r) {
      var m = modById(r.source);
      return m ? m.name : trf('mod {0}', r.source);
    });
    var btn = el('button', {
      class: 'anim-btn edit-only' + (rs.length ? ' active' : ''),
      'data-tip': rs.length
        ? trf('Paramètre animé par : {0} — clic pour éditer l’animation', names.join(', '))
        : 'Animer ce paramètre (LFO, bande FFT…) — clic pour choisir une source',
      onclick: function (e) {
        e.stopPropagation();
        openAnimPopover(addr, btn);
      }
    });
    btn.innerHTML = ICONS.anim;
    if (rs.length) {
      btn.style.color = modColor(rs[0].source);
      if (rs.length > 1) { btn.appendChild(el('span', { class: 'anim-count' }, String(rs.length))); }
    }
    return btn;
  }

  function closeAnimPopover() {
    if (POP.el && POP.el.parentNode) { POP.el.parentNode.removeChild(POP.el); }
    POP.addr = null;
    POP.el = null;
  }

  function openAnimPopover(addr, anchor) {
    if (POP.addr === addr) { closeAnimPopover(); return; }
    closeAnimPopover();
    POP.addr = addr;
    POP.el = el('div', { id: 'anim-popover' });
    document.body.appendChild(POP.el);
    var r = anchor.getBoundingClientRect();
    POP.x = r.left;
    POP.y = r.bottom + 6;
    renderAnimPopover();
  }

  /* (Re)construit le contenu du popover depuis S.show — appelé à l'ouverture
     et à chaque edit_applied de route ou de modulateur tant qu'il est ouvert. */
  function renderAnimPopover() {
    var pop = POP.el;
    if (!pop) { return; }
    var addr = POP.addr;
    pop.textContent = '';

    pop.appendChild(el('div', { class: 'pop-head' },
      el('span', { class: 'pop-title' }, 'Animation'),
      el('code', { class: 'pop-addr', 'data-tip': 'Adresse stable du paramètre (aussi pilotable en OSC/MIDI/DMX)' }, addr),
      el('span', { class: 'spacer' }),
      el('button', { class: 'ghost', 'data-tip': 'Fermer (Échap)', onclick: closeAnimPopover }, '✕')));

    /* routes actives : badges éditables */
    var rs = routesFor(addr);
    if (!rs.length) {
      pop.appendChild(el('div', { class: 'muted', style: 'margin:2px 0 10px' },
        'Aucune animation sur ce paramètre.'));
    }
    rs.forEach(function (r) {
      var m = modById(r.source);
      var col = modColor(r.source);
      function commitR(mut) {
        var copy = JSON.parse(JSON.stringify(r));
        mut(copy);
        sendEdit({ op: 'route_update', route: copy });
      }
      var depth = el('input', {
        type: 'range', min: -1, max: 1, step: 0.01, value: r.depth,
        'data-tip': 'Profondeur de modulation -1..1 (négatif = inversé)'
      });
      var dval = el('span', { class: 'val' }, fmtF(r.depth));
      depth.addEventListener('input', function () { dval.textContent = fmtF(parseFloat(depth.value)); });
      depth.addEventListener('change', function () {
        commitR(function (x) { x.depth = parseFloat(depth.value) || 0; });
      });
      enhanceSlider(depth, dval, 0.5);
      var mode = sel(['add', 'mul', 'replace'], ['Ajouter', 'Multiplier', 'Remplacer'], r.mode,
        function (v) { commitR(function (x) { x.mode = v; }); });
      mode.setAttribute('data-tip', 'Ajouter : base + signal. Multiplier : atténuation. Remplacer : remplace la base.');
      pop.appendChild(el('div', { class: 'pop-route' },
        el('span', { class: 'mod-color-dot', style: 'background:' + col + ';color:' + col }),
        el('span', { class: 'pop-route-name' }, m ? m.name : ('mod ' + r.source)),
        depth, dval, mode,
        el('button', {
          class: 'danger', 'data-tip': 'Débranche cette animation (supprime la route)',
          onclick: function () { sendEdit({ op: 'route_remove', id: r.id }); }
        }, '✕')));
    });

    if (isShowMode()) {
      pop.appendChild(el('div', { class: 'muted' }, 'Mode Show : édition verrouillée.'));
    } else {
      /* branchement d'une nouvelle source */
      var depthN = el('input', {
        type: 'range', min: -1, max: 1, step: 0.01, value: 0.5,
        'data-tip': 'Profondeur de la nouvelle animation (-1..1)'
      });
      var dvalN = el('span', { class: 'val' }, '0.50');
      depthN.addEventListener('input', function () { dvalN.textContent = fmtF(parseFloat(depthN.value)); });
      enhanceSlider(depthN, dvalN, 0.5);
      var modeN = sel(['add', 'mul', 'replace'], ['Ajouter', 'Multiplier', 'Remplacer'], 'add', null);
      modeN.setAttribute('data-tip', 'Mode d’application de la nouvelle animation');

      var src = el('select', { 'data-tip': 'Source à brancher : modulateur existant, ou création directe' });
      src.appendChild(el('option', { value: '' }, '— Ajouter une source —'));
      if (rs.length) { src.appendChild(el('option', { value: 'none' }, 'Aucune (tout débrancher)')); }
      var lfos = modulators().filter(isLfoMod);
      var bandsM = modulators().filter(isBandMod);
      if (lfos.length) {
        var og1 = el('optgroup', { label: 'LFO existants' });
        lfos.forEach(function (m) { og1.appendChild(el('option', { value: 'mod:' + m.id }, m.name || ('LFO ' + m.id))); });
        src.appendChild(og1);
      }
      if (bandsM.length) {
        var og2 = el('optgroup', { label: 'Bandes FFT existantes' });
        bandsM.forEach(function (m) { og2.appendChild(el('option', { value: 'mod:' + m.id }, m.name || ('Bande ' + m.id))); });
        src.appendChild(og2);
      }
      src.appendChild(el('option', { value: 'new_lfo' }, '+ Créer un LFO'));
      src.appendChild(el('option', { value: 'new_band' }, '+ Créer une bande FFT'));
      src.appendChild(el('option', {
        value: 'timecode', disabled: 'disabled',
        title: 'Le chase timecode pilote les CUES (Réglages → Chase timecode) ; l’animation de paramètres au timecode viendra plus tard'
      }, 'Timecode (réservé)'));

      src.addEventListener('change', function () {
        var v = src.value;
        src.value = '';
        if (!v) { return; }
        if (v === 'none') {
          routesFor(addr).forEach(function (r) { sendEdit({ op: 'route_remove', id: r.id }); });
          return;
        }
        var d = parseFloat(depthN.value);
        if (!isFinite(d)) { d = 0.5; }
        var srcId = null;
        if (v === 'new_lfo') {
          srcId = nextModId();
          sendEdit({ op: 'modulator_add', modulator: newLfoCfg(srcId) });
        } else if (v === 'new_band') {
          srcId = nextModId();
          sendEdit({ op: 'modulator_add', modulator: newBandCfg(srcId) });
        } else if (v.indexOf('mod:') === 0) {
          srcId = parseInt(v.slice(4), 10);
        }
        if (srcId === null || !isFinite(srcId)) { return; }
        sendEdit({
          op: 'route_add',
          route: { id: nextRouteId(), source: srcId, target_addr: addr, depth: d, mode: modeN.value }
        });
      });

      pop.appendChild(el('div', { class: 'pop-add' },
        el('div', { class: 'pop-add-row' }, el('span', { class: 'pop-label' }, 'Source'), src),
        el('div', { class: 'pop-add-row' }, el('span', { class: 'pop-label' }, 'Profondeur'), depthN, dvalN),
        el('div', { class: 'pop-add-row' }, el('span', { class: 'pop-label' }, 'Mode'), modeN)));
      pop.appendChild(el('div', { class: 'muted pop-hint' },
        'La source choisie est branchée immédiatement. Réglez ensuite le LFO ou la bande dans l’onglet Modulation.'));
    }

    /* positionnement : sous l'ancre, borné à la fenêtre */
    var pw = pop.offsetWidth, ph = pop.offsetHeight;
    var x = clamp(POP.x, 8, Math.max(8, window.innerWidth - pw - 8));
    var y = POP.y;
    if (y + ph > window.innerHeight - 8) { y = Math.max(8, POP.y - ph - 40); }
    pop.style.left = x + 'px';
    pop.style.top = y + 'px';
  }

  function installAnimPopoverClose() {
    document.addEventListener('pointerdown', function (e) {
      if (!POP.el) { return; }
      var t = e.target;
      if (POP.el.contains(t)) { return; }
      if (t && t.closest && t.closest('.anim-btn')) { return; }
      closeAnimPopover();
    });
  }

  /* ================================================================ PATCH */

  RENDERERS.patch = function () {
    var root = el('section', { class: 'tab-panel' });

    /* --- état réel des protocoles (runtime.protocols) --- */
    root.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'État des protocoles'),
      el('div', {
        id: 'proto-line', class: 'proto-line',
        'data-tip': 'État réel au démarrage des services : vert = actif, gris = non configuré, rouge = erreur (port pris, périphérique absent…)'
      })));

    /* --- OSC --- */
    var osc = el('div', { class: 'panel' }, el('h2', null, 'OSC'));
    var st = settings();
    osc.appendChild(el('div', null,
      'Entrée : port ', el('b', null, st.osc_in_port !== undefined ? st.osc_in_port : 9000),
      '  —  Sortie (feedback) : ',
      el('b', null, patch().osc_out ? (patch().osc_out.host + ':' + patch().osc_out.port) : 'non configurée')));
    osc.appendChild(el('div', { class: 'muted', style: 'margin:6px 0' },
      'Adresses (clic = copier) : /conduite/cue/go, /conduite/cue/back, /conduite/cue/goto, /conduite/master, /conduite/dbo, /conduite/bpm, /conduite/bpm/tap, et /conduite/param/<adresse> :'));
    var chips = el('div');
    ['cue/go', 'cue/back', 'cue/goto', 'master', 'dbo', 'bpm', 'bpm/tap'].forEach(function (a) {
      chips.appendChild(addrChip('/conduite/' + a));
    });
    paramAddrs().forEach(function (a) { chips.appendChild(addrChip('/conduite/param/' + a)); });
    osc.appendChild(chips);
    root.appendChild(osc);

    /* --- MIDI --- */
    var midi = el('div', { class: 'panel' }, el('h2', null, 'MIDI'));
    midi.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        disabled: true,
        'data-tip': 'MIDI Learn arrive dans une prochaine version — utiliser l’ajout manuel ci-dessous'
      }, 'Learn'),
      el('button', {
        'data-tip': 'Ajoute un binding CC → master (canal 1, CC 7, pickup) à modifier ensuite',
        onclick: function () {
          sendEdit({
            op: 'patch_midi_add',
            binding: { cc: { channel: 0, cc: 7, fourteen_bits: false, addr: 'master/intensity', min: 0, max: 1, pickup: true } }
          });
        }
      }, '+ CC'),
      el('button', {
        'data-tip': 'Ajoute un binding Note → GO (canal 1, note 60) à modifier ensuite',
        onclick: function () {
          sendEdit({
            op: 'patch_midi_add',
            binding: { note: { channel: 0, note: 60, command: { cmd: 'go' } } }
          });
        }
      }, '+ Note')));
    var mtable = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Type'), el('th', null, 'Canal'), el('th', null, 'N°'),
        el('th', null, 'Cible'),
        el('th', { 'data-tip': 'Soft-takeover : le fader physique doit rejoindre la valeur avant de la piloter (pas de saut)' }, 'Pickup'),
        el('th', { class: 'edit-only' }, '')));
    (patch().midi || []).forEach(function (b, i) { mtable.appendChild(midiRow(b, i)); });
    midi.appendChild(el('div', { style: 'overflow-x:auto' }, mtable));
    if (!(patch().midi || []).length) {
      midi.appendChild(el('div', { style: 'padding:8px 0 0' },
        emptyState('plug', 'Aucun binding MIDI — « + CC » ou « + Note » pour piloter la conduite.')));
    }
    root.appendChild(midi);

    /* --- Art-Net --- */
    var art = el('div', { class: 'panel' }, el('h2', null, 'Art-Net (DMX)'));
    art.appendChild(el('div', { class: 'muted' },
      trf('Nœud {0} — univers écoutés : {1}',
        tr(st.artnet_enabled ? 'actif' : 'inactif'),
        (st.artnet_universes || []).join(', ') || '—')));
    art.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Ajoute un patch DMX (univers 0, canal 1 → master) à modifier ensuite',
        onclick: function () {
          sendEdit({
            op: 'patch_artnet_add',
            entry: { universe: 0, channel: 1, bits: 'eight', addr: 'master/intensity', min: 0, max: 1, smoothing_ms: 80 }
          });
        }
      }, '+ Canal')));
    var atable = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Univers'), el('th', { 'data-tip': 'Canal DMX 1–512 (16 bits : LSB sur canal+1)' }, 'Canal'),
        el('th', null, 'Bits'), el('th', null, 'Cible'),
        el('th', null, 'Min'), el('th', null, 'Max'),
        el('th', { 'data-tip': 'Lissage à la réception (ms) — le DMX arrive à ~44 Hz' }, 'Lissage'),
        el('th', { class: 'edit-only' }, '')));
    (patch().artnet || []).forEach(function (e, i) { atable.appendChild(artnetRow(e, i)); });
    art.appendChild(el('div', { style: 'overflow-x:auto' }, atable));
    if (!(patch().artnet || []).length) {
      art.appendChild(el('div', { style: 'padding:8px 0 0' },
        emptyState('plug', 'Aucun patch Art-Net — « + Canal » pour piloter un paramètre en DMX.')));
    }
    root.appendChild(art);

    /* --- Clavier remappable --- */
    root.appendChild(renderKeyPanel());

    return root;
  };

  function addrChip(addr) {
    var chip = el('span', { class: 'addr-chip', 'data-tip': 'Copier ' + addr }, addr);
    chip.addEventListener('click', function () { copyText(addr); });
    return chip;
  }

  function midiRow(b, index) {
    var row = el('tr');
    var isNote = b && typeof b === 'object' && 'note' in b;
    var body = isNote ? b.note : (b && b.cc) || {};

    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(b));
      mut(isNote ? copy.note : copy.cc);
      sendEdit({ op: 'patch_midi_update', index: index, binding: copy });
    }

    row.appendChild(el('td', null, isNote ? 'Note' : ('CC' + (body.fourteen_bits ? ' 14 bits' : ''))));

    var ch = el('input', { type: 'number', min: 1, max: 16, value: (body.channel || 0) + 1, 'data-tip': 'Canal MIDI 1–16' });
    ch.addEventListener('change', function () {
      commit(function (x) { x.channel = clamp((parseInt(ch.value, 10) || 1) - 1, 0, 15); });
    });
    row.appendChild(el('td', null, ch));

    var num = el('input', { type: 'number', min: 0, max: 127, value: isNote ? body.note : body.cc });
    num.addEventListener('change', function () {
      commit(function (x) {
        var v = clamp(parseInt(num.value, 10) || 0, 0, 127);
        if (isNote) { x.note = v; } else { x.cc = v; }
      });
    });
    row.appendChild(el('td', null, num));

    if (isNote) {
      var cmds = ['go', 'back', 'dbo', 'dbo_release', 'tap_tempo', 'panic'];
      var cur = body.command && body.command.cmd ? body.command.cmd : 'go';
      var csel = sel(cmds, ['GO', 'Back', 'DBO', 'DBO release', 'Tap tempo', 'Panic'], cur, function (v) {
        commit(function (x) {
          x.command = (v === 'dbo' || v === 'panic') ? { cmd: v, fade_s: v === 'dbo' ? 0 : 2 } : { cmd: v };
        });
      });
      csel.setAttribute('data-tip', 'Commande déclenchée par la note');
      row.appendChild(el('td', null, csel));
      row.appendChild(el('td', { class: 'muted' }, '—'));
    } else {
      var addrs = paramAddrs();
      if (addrs.indexOf(body.addr) < 0) { addrs.unshift(body.addr || ''); }
      var asel = sel(addrs, addrs, body.addr, function (v) { commit(function (x) { x.addr = v; }); });
      row.appendChild(el('td', null, asel));
      var pk = el('input', { type: 'checkbox', checked: !!body.pickup });
      pk.addEventListener('change', function () { commit(function (x) { x.pickup = pk.checked; }); });
      row.appendChild(el('td', null, pk));
    }

    row.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime ce binding',
        onclick: function () { sendEdit({ op: 'patch_midi_remove', index: index }); }
      }, '✕')));
    return row;
  }

  function artnetRow(e, index) {
    var row = el('tr');
    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(e));
      mut(copy);
      sendEdit({ op: 'patch_artnet_update', index: index, entry: copy });
    }
    function numCell(key, min, max, step) {
      var i = el('input', { type: 'number', min: min, max: max, step: step || 1, value: e[key] });
      i.addEventListener('change', function () {
        commit(function (x) { x[key] = clamp(parseFloat(i.value) || 0, min, max); });
      });
      return el('td', null, i);
    }
    row.appendChild(numCell('universe', 0, 32767));
    row.appendChild(numCell('channel', 1, 512));
    var bits = sel(['eight', 'sixteen'], ['8 bits', '16 bits'], e.bits, function (v) {
      commit(function (x) { x.bits = v; });
    });
    row.appendChild(el('td', null, bits));
    var addrs = paramAddrs();
    if (addrs.indexOf(e.addr) < 0) { addrs.unshift(e.addr || ''); }
    var asel = sel(addrs, addrs, e.addr, function (v) { commit(function (x) { x.addr = v; }); });
    row.appendChild(el('td', null, asel));
    row.appendChild(numCell('min', -1e9, 1e9, 0.01));
    row.appendChild(numCell('max', -1e9, 1e9, 0.01));
    row.appendChild(numCell('smoothing_ms', 0, 10000));
    row.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime cette entrée de patch',
        onclick: function () { sendEdit({ op: 'patch_artnet_remove', index: index }); }
      }, '✕')));
    return row;
  }

  /* ============================================================== SORTIES */

  RENDERERS.sorties = function () {
    var root = el('section', { class: 'tab-panel' });
    var panel = el('div', { class: 'panel' }, el('h2', null, 'Sorties'));

    panel.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Ajoute une sortie 1920×1080',
        onclick: function () {
          var id = outputs().reduce(function (m, o) { return Math.max(m, o.id); }, 0) + 1;
          sendEdit({
            op: 'output_add',
            output: { id: id, name: trf('Sortie {0}', id), monitor_index: null, width: 1920, height: 1080, fullscreen: true, enabled: false }
          });
        }
      }, '+ Sortie')));

    var table = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Nom'),
        el('th', { 'data-tip': 'Index du moniteur physique (vide = fenêtré)' }, 'Moniteur'),
        el('th', null, 'Largeur'), el('th', null, 'Hauteur'),
        el('th', { 'data-tip': 'Plein écran sans bordure sur le moniteur choisi' }, 'Plein écran'),
        el('th', { 'data-tip': 'Sortie active (rendue)' }, 'Active'),
        el('th', null, ''), el('th', { class: 'edit-only' }, '')));

    outputs().forEach(function (o) {
      var row = el('tr');
      function commit(mut) {
        var copy = JSON.parse(JSON.stringify(o));
        mut(copy);
        sendEdit({ op: 'output_update', output: copy });
      }
      var name = el('input', { type: 'text', value: o.name || '' });
      name.addEventListener('change', function () { commit(function (x) { x.name = name.value; }); });
      row.appendChild(el('td', null, name));

      var mon = el('input', { type: 'number', min: 0, value: o.monitor_index === null || o.monitor_index === undefined ? '' : o.monitor_index, placeholder: '—' });
      mon.addEventListener('change', function () {
        commit(function (x) { x.monitor_index = mon.value === '' ? null : Math.max(0, parseInt(mon.value, 10) || 0); });
      });
      row.appendChild(el('td', null, mon));

      var w = el('input', { type: 'number', min: 1, value: o.width });
      w.addEventListener('change', function () { commit(function (x) { x.width = Math.max(1, parseInt(w.value, 10) || 1920); }); });
      row.appendChild(el('td', null, w));
      var h = el('input', { type: 'number', min: 1, value: o.height });
      h.addEventListener('change', function () { commit(function (x) { x.height = Math.max(1, parseInt(h.value, 10) || 1080); }); });
      row.appendChild(el('td', null, h));

      var fs = el('input', { type: 'checkbox', checked: !!o.fullscreen });
      fs.addEventListener('change', function () { commit(function (x) { x.fullscreen = fs.checked; }); });
      row.appendChild(el('td', null, fs));

      var en = el('input', { type: 'checkbox', checked: !!o.enabled });
      en.addEventListener('change', function () { commit(function (x) { x.enabled = en.checked; }); });
      row.appendChild(el('td', null, en));

      /* mires : identification en un clic + sélecteur enrichi (grilles
         4/16, damier, barres SMPTE, extinction) */
      function applyOutputPattern(kind) {
        var ok = 0;
        slices().filter(function (s) { return s.output === o.id; })
          .forEach(function (s) {
            var content = kind === 'none' ? 'none' : { pattern: kind };
            if (assignContent(s.id, content, null, true)) { ok++; }
          });
        if (!ok) { uiWarn('Aucun slice sur cette sortie — rien à afficher.'); return; }
        toast(kind === 'none'
          ? trf('Contenu retiré de « {0} » ({1} slice(s))', o.name || o.id, ok)
          : trf('Mire « {0} » posée sur « {1} » ({2} slice(s))', tr(patternLabel(kind)), o.name || o.id, ok), 'ok');
      }
      var mireOutSel = sel(
        ['', 'ident', 'grid', 'grid4', 'grid16', 'checker', 'bars', 'color_bars', 'none'],
        ['Mire…', 'Identification', 'Grille', 'Grille 4', 'Grille 16', 'Damier', 'Barres', 'Barres SMPTE', 'Éteindre'],
        '',
        function (v) {
          mireOutSel.value = '';
          if (v) { applyOutputPattern(v); }
        });
      mireOutSel.className = 'edit-only';
      mireOutSel.setAttribute('data-tip', 'Pose une mire sur tous les slices de cette sortie (cue standby/active) — « Éteindre » retire le contenu');
      row.appendChild(el('td', null,
        el('span', { class: 'toolbar', style: 'margin:0;flex-wrap:nowrap' },
          el('button', {
            class: 'edit-only',
            'data-tip': 'Affiche la mire d’identification (nom + résolution de la sortie) sur tous ses slices',
            onclick: function () { applyOutputPattern('ident'); }
          }, 'Identifier'),
          mireOutSel)));

      row.appendChild(el('td', { class: 'edit-only' },
        el('button', {
          class: 'danger', 'data-tip': 'Supprime cette sortie (confirmation demandée)',
          onclick: function () {
            var nbSlices = slices().filter(function (s) { return s.output === o.id; }).length;
            confirmDialog({
              title: trf('Supprimer la sortie « {0} » ?', o.name || trf('sortie {0}', o.id)),
              message: (nbSlices
                ? trf('{0} slice(s) calé(s) sur cette sortie deviendront orphelins. ', nbSlices)
                : '') + tr('Annulable ensuite avec Ctrl+Z.'),
              confirm: 'Supprimer',
              onConfirm: function () { sendEdit({ op: 'output_remove', id: o.id }); }
            });
          }
        }, '✕')));
      table.appendChild(row);
    });

    panel.appendChild(el('div', { style: 'overflow-x:auto' }, table));
    if (!outputs().length) {
      panel.appendChild(el('div', { style: 'padding:8px 0 0' },
        emptyState('screen', 'Aucune sortie — ajoutez-en une puis activez-la pour projeter.', {
          label: '+ Sortie',
          onclick: function () {
            var id = outputs().reduce(function (m, o) { return Math.max(m, o.id); }, 0) + 1;
            sendEdit({
              op: 'output_add',
              output: { id: id, name: trf('Sortie {0}', id), monitor_index: null, width: 1920, height: 1080, fullscreen: true, enabled: false }
            });
          }
        })));
    }
    root.appendChild(panel);
    return root;
  };

  /* ============================================================== JOURNAL */

  RENDERERS.journal = function () {
    var root = el('section', { class: 'tab-panel' });
    /* panel-fill : le journal occupe la hauteur restante en flex
       (plus de calc(100vh - N px) fragile) */
    var panel = el('div', { class: 'panel panel-fill' }, el('h2', null, 'Journal'));

    var filter = sel(['all', 'error', 'warn', 'info', 'debug', 'trace'],
      ['Tous', 'Erreurs', 'Avertissements', 'Info', 'Debug', 'Trace'],
      S.logFilter,
      function (v) { S.logFilter = v; renderJournalList(); });
    filter.setAttribute('data-tip', 'Filtre par niveau de log');

    panel.appendChild(el('div', { class: 'toolbar' },
      filter,
      el('button', {
        'data-tip': 'Copie le journal filtré dans le presse-papiers',
        onclick: function () {
          var txt = filteredLogs().map(function (l) {
            return '[' + l.level + '] ' + l.target + ' — ' + l.message;
          }).join('\n');
          copyText(txt);
        }
      }, 'Copier'),
      el('button', {
        'data-tip': 'Vide l’affichage local (le fichier de log du moteur est conservé)',
        onclick: function () { S.logs = []; renderJournalList(); }
      }, 'Effacer'),
      el('span', { class: 'muted', id: 'journal-count' }, '')));

    panel.appendChild(el('div', { id: 'journal-list' }));
    root.appendChild(panel);
    setTimeout(renderJournalList, 0);
    return root;
  };

  function filteredLogs() {
    if (S.logFilter === 'all') { return S.logs; }
    return S.logs.filter(function (l) { return (l.level || '').toLowerCase() === S.logFilter; });
  }

  function renderJournalList() {
    var list = byId('journal-list');
    if (!list) { return; }
    list.textContent = '';
    filteredLogs().forEach(function (l) { list.appendChild(logLineDom(l)); });
    if (!filteredLogs().length) {
      list.appendChild(emptyState('list', S.logFilter === 'all'
        ? 'Journal vide — les événements du moteur s’afficheront ici.'
        : 'Aucune ligne à ce niveau de filtre.'));
    }
    list.scrollTop = list.scrollHeight;
    var count = byId('journal-count');
    if (count) { count.textContent = trf('{0} ligne(s)', filteredLogs().length); }
  }

  function logLineDom(l) {
    var lvl = (l.level || 'info').toLowerCase();
    return el('div', { class: 'log-line ' + lvl },
      el('span', { class: 'log-level' }, lvl.toUpperCase()),
      el('span', { class: 'log-target' }, l.target || ''),
      el('span', null, l.message || ''));
  }

  function pushLog(level, target, message) {
    S.logs.push({ level: level, target: target, message: message, ts: Date.now() });
    if (S.logs.length > 500) { S.logs.splice(0, S.logs.length - 500); }
    var list = byId('journal-list');
    if (list && (S.logFilter === 'all' || S.logFilter === level)) {
      var empty = list.querySelector('.empty-state');
      if (empty) { list.removeChild(empty); }
      list.appendChild(logLineDom({ level: level, target: target, message: message }));
      list.scrollTop = list.scrollHeight;
    }
  }

  /* ============================================================= RÉGLAGES */

  RENDERERS.reglages = function () {
    var root = el('section', { class: 'tab-panel' });
    var st = settings();

    /* show */
    var showPanel = el('div', { class: 'panel' }, el('h2', null, 'Show'));
    var name = el('input', { type: 'text', value: show().name || '', 'data-tip': 'Nom du show (fichier shows/<nom>)' });
    name.addEventListener('change', function () { sendEdit({ op: 'show_rename', name: name.value }); });
    /* « Charger » : liste réelle des shows du dossier shows/ (runtime.shows)
       — repli sur un champ texte si le moteur ne la publie pas.
       Tolère des entrées String ou { name, modified } (date affichée si
       le moteur la publie un jour). */
    var showsList = Array.isArray(rt().shows) ? rt().shows.slice() : null;
    var loadName;
    if (showsList && showsList.length) {
      loadName = el('select', { 'data-tip': 'Shows présents dans le dossier shows/ (liste publiée par le moteur)' });
      loadName.appendChild(el('option', { value: '' }, '— choisir un show —'));
      showsList
        .map(function (s) {
          return (s && typeof s === 'object')
            ? { name: String(s.name || ''), date: s.modified || s.date || null }
            : { name: String(s), date: null };
        })
        .filter(function (s) { return s.name; })
        .sort(function (a, b) { return a.name.localeCompare(b.name); })
        .forEach(function (s) {
          loadName.appendChild(el('option', { value: s.name },
            s.name +
            (s.date ? ' — ' + s.date : '') +
            (s.name === show().name ? ' (ouvert)' : '')));
        });
    } else {
      loadName = el('input', { type: 'text', placeholder: 'nom du show à charger' });
    }
    var saveAsName = el('input', { type: 'text', placeholder: 'enregistrer sous…' });
    showPanel.appendChild(el('div', { class: 'settings-grid' },
      el('span', null, 'Nom du show'), name));
    showPanel.appendChild(el('div', { class: 'toolbar' },
      el('button', {
        class: 'primary', 'data-tip': 'Enregistre le show (écriture atomique + backup rotatif)',
        onclick: function () { sendCmd({ cmd: 'show_save' }); }
      }, 'Enregistrer'),
      el('span', { class: 'edit-only' },
        saveAsName,
        el('button', {
          'data-tip': 'Enregistre une copie sous un autre nom',
          onclick: function () { if (saveAsName.value.trim()) { sendCmd({ cmd: 'show_save_as', name: saveAsName.value.trim() }); } }
        }, 'Enregistrer sous')),
      el('span', { class: 'edit-only' },
        loadName,
        el('button', {
          'data-tip': 'Charge un show du dossier shows/ (le show courant est sauvegardé automatiquement avant)',
          onclick: function () {
            var n = (loadName.value || '').trim();
            if (!n) { uiWarn('Choisissez un show à charger.'); return; }
            if (n === show().name) { uiInfo(trf('Le show « {0} » est déjà ouvert.', n)); return; }
            confirmDialog({
              title: trf('Charger « {0} » ?', n),
              message: 'Le show courant est sauvegardé automatiquement avant le chargement.',
              confirm: 'Charger', danger: false,
              onConfirm: function () { sendCmd({ cmd: 'show_load', name: n }); }
            });
          }
        }, 'Charger')),
      el('button', {
        class: 'edit-only', 'data-tip': 'Nouveau show vide (confirmation demandée)',
        onclick: function () {
          confirmDialog({
            title: 'Nouveau show ?',
            message: tr('Remplace la conduite courante par un show vide. Le fichier du show actuel reste sur disque (shows/), mais les modifications non enregistrées seront perdues.'),
            confirm: 'Nouveau show',
            onConfirm: function () { sendCmd({ cmd: 'show_new' }); }
          });
        }
      }, 'Nouveau'),
      el('button', {
        class: 'edit-only',
        'data-tip': 'Collecter le show : copie médias et shaders dans un dossier autonome shows/<nom>-collecte (clé USB, autre machine)',
        onclick: function () {
          if (sendCmd({ cmd: 'show_collect' })) {
            toast(el('span', null, 'Collecte en cours vers ',
              el('code', null, 'shows/' + (show().name || 'show') + '-collecte'),
              ' — un toast confirmera la fin.'));
          }
        }
      }, 'Collecter le show')));
    root.appendChild(showPanel);

    /* mode */
    var modePanel = el('div', { class: 'panel' }, el('h2', null, 'Mode'));
    modePanel.appendChild(el('div', { class: 'toolbar' },
      el('span', null, 'Mode courant : ', el('b', null, isShowMode() ? 'Show (verrouillé)' : 'Édition')),
      el('button', {
        class: isShowMode() ? 'primary' : 'danger',
        'data-tip': isShowMode()
          ? 'Repasse en mode Édition (déverrouille l’édition)'
          : 'Passe en mode Show : édition verrouillée, seul l’onglet Live reste visible',
        onclick: function () { sendCmd({ cmd: 'mode_set', mode: isShowMode() ? 'edit' : 'show' }); }
      }, isShowMode() ? 'Repasser en Édition' : 'Passer en mode Show')));
    root.appendChild(modePanel);

    /* réglages persistés */
    var cfgPanel = el('div', { class: 'panel edit-only' }, el('h2', null, 'Réglages du show'));
    function commitSettings(mut) {
      var copy = JSON.parse(JSON.stringify(st));
      mut(copy);
      sendEdit({ op: 'settings_update', settings: copy });
    }
    function portInput(key, tip) {
      var i = el('input', { type: 'number', min: 1, max: 65535, value: st[key], 'data-tip': tip });
      i.addEventListener('change', function () {
        commitSettings(function (x) { x[key] = clamp(parseInt(i.value, 10) || x[key], 1, 65535); });
      });
      return i;
    }
    var lang = sel(['fr', 'en'], ['Français', 'English'], st.language || 'fr', function (v) {
      commitSettings(function (x) { x.language = v; });
    });
    lang.setAttribute('data-tip', 'Langue de l’interface — appliquée immédiatement et enregistrée dans le show. Les messages émis par le moteur (journal) restent en français.');
    var fps = el('input', { type: 'number', min: 1, max: 30, value: st.mjpeg_fps || 8, 'data-tip': 'Cadence des préviews MJPEG (img/s)' });
    fps.addEventListener('change', function () {
      commitSettings(function (x) { x.mjpeg_fps = clamp(parseInt(fps.value, 10) || 8, 1, 30); });
    });
    /* garde-fous de conduite : anti double-GO et fondu du panic (Échap) */
    var goMs = el('input', {
      type: 'number', min: 0, max: 5000, step: 50,
      value: (typeof st.min_go_interval_ms === 'number' ? st.min_go_interval_ms : 300),
      'data-tip': 'Délai minimal entre deux GO (ms) — un GO pendant le délai est refusé, toutes sources (UI, OSC, MIDI, MSC). 0 = désactivé.'
    });
    goMs.addEventListener('change', function () {
      commitSettings(function (x) {
        x.min_go_interval_ms = clamp(parseInt(goMs.value, 10) || 0, 0, 5000);
      });
    });
    var panicFade = el('input', {
      type: 'number', min: 0, max: 30, step: 0.1,
      value: (typeof st.panic_fade_s === 'number' ? st.panic_fade_s : 2),
      'data-tip': 'Durée du fondu au noir du panic (touche Échap). Double Échap = toujours arrêt sec (0 s).'
    });
    panicFade.addEventListener('change', function () {
      commitSettings(function (x) {
        x.panic_fade_s = clamp(parseFloat(panicFade.value) || 0, 0, 30);
      });
    });
    /* chase timecode : les cues à déclencheur TC suivent un MTC entrant */
    var tcChk = el('input', {
      type: 'checkbox', checked: !!st.timecode_chase,
      'data-tip': 'Suit un timecode MTC entrant (ports MIDI). À l’avancée normale : chaque cue dont le déclencheur passe est jouée (GO, transition respectée). Sur un saut avant/arrière : calage direct (GOTO) sur la dernière cue dont le déclencheur est ≤ au timecode. Perte de signal : 2 s de roue libre puis pause du chase — les cues actives continuent, rien n’est coupé ; au retour du signal, re-calage comme après un saut. Les cues sans déclencheur restent manuelles.'
    });
    tcChk.addEventListener('change', function () {
      commitSettings(function (x) { x.timecode_chase = tcChk.checked; });
    });
    /* mise à jour : opt-in, désactivée par défaut, libellé honnête */
    var updChk = el('input', {
      type: 'checkbox', checked: !!st.update_check,
      'data-tip': 'Une seule requête vers latest.json au démarrage, en mode Édition uniquement (timeout 3 s). Ne télécharge jamais rien : un badge discret signale la version, c’est tout. Décochée par défaut.'
    });
    updChk.addEventListener('change', function () {
      commitSettings(function (x) { x.update_check = updChk.checked; });
    });
    cfgPanel.appendChild(el('div', { class: 'settings-grid' },
      el('span', null, 'Port OSC entrant'), portInput('osc_in_port', 'Port UDP d’écoute OSC (défaut 9000) — redémarrage du service OSC'),
      el('span', null, 'Port OSC sortant'), portInput('osc_out_port', 'Port de feedback OSC par défaut'),
      el('span', null, 'Langue'), lang,
      el('span', null, 'Préview (img/s)'), fps,
      el('span', null, 'Anti double-GO (ms)'), goMs,
      el('span', null, 'Fondu du panic (s)'), panicFade,
      el('span', null, 'Chase timecode'),
      el('span', { class: 'toolbar', style: 'margin:0' }, tcChk,
        el('span', { class: 'muted' }, 'déclenche les cues à déclencheur TC sur un timecode MTC entrant')),
      el('span', null, 'Vérifier les mises à jour'),
      el('span', { class: 'toolbar', style: 'margin:0' }, updChk,
        el('span', { class: 'muted' }, 'vérifie une fois au démarrage, ne télécharge rien'))));
    root.appendChild(cfgPanel);

    /* maintenance : rapport de diagnostic (zip anonymisé pour le support) */
    root.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Maintenance'),
      el('div', { class: 'toolbar' },
        el('button', {
          'data-tip': 'Génère logs/diagnostic-<date>.zip : derniers logs, config, show, versions (app + ffmpeg), santé — chemins personnels expurgés (C:\\Users\\<vous> → ~). À joindre à une demande de support.',
          onclick: function () {
            if (sendCmd({ cmd: 'diagnostic_report' })) {
              uiInfo('Génération du rapport de diagnostic…');
            }
          }
        }, 'Rapport de diagnostic'),
        el('span', { class: 'muted' },
          'Zip anonymisé (logs, config, show, versions) — un toast donnera son chemin.'))));

    /* quitter proprement (sauvegarde si modifié, flush des logs, sorties) */
    root.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Application'),
      el('div', { class: 'toolbar' },
        el('button', {
          class: 'danger',
          'data-tip': 'Arrêt propre du moteur : sauvegarde du show s’il a été modifié, fermeture des sorties et des ports, purge des logs',
          onclick: function () {
            confirmDialog({
              title: 'Quitter Conduite ?',
              message: 'Arrêt propre : le show est sauvegardé s’il a été modifié, puis le moteur et les sorties s’éteignent.',
              confirm: 'Quitter',
              onConfirm: function () {
                sendCmd({ cmd: 'quit' });
                uiInfo('Arrêt demandé — le moteur se ferme.');
              }
            });
          }
        }, 'Quitter Conduite'),
        el('span', { class: 'muted' }, 'Ferme le moteur et toutes les sorties (la page web devient inactive).'))));

    /* À propos : version, licence, crédits (GET /about, best-effort) */
    var about = S.about || {};
    var ap = el('div', { class: 'panel' }, el('h2', null, 'À propos'));
    ap.appendChild(el('div', { class: 'about-head' },
      el('span', { class: 'about-name' }, about.name || 'Conduite'),
      el('span', { class: 'about-version' },
        about.version
          ? ('v' + about.version + (about.git ? ' (' + about.git + ')' : ''))
          : 'version non publiée par le moteur'),
      el('span', { class: 'spacer' }),
      el('span', { class: 'muted' }, tr('Licence ') + (about.license || 'MIT'))));
    ap.appendChild(el('div', { class: 'about-desc' },
      tr(about.description || 'Régie vidéo de spectacle — cues, mapping, ISF, MIDI/OSC/Art-Net') +
      (about.copyright ? ' — ' + about.copyright : '')));
    (Array.isArray(about.credits) ? about.credits : []).forEach(function (c) {
      if (!c || typeof c !== 'object') { return; }
      ap.appendChild(el('div', { class: 'about-credit' },
        el('span', { class: 'ac-name' }, c.name || ''),
        el('span', { class: 'ac-role' }, c.role || ''),
        el('span', { class: 'ac-lic' },
          (c.license || '') + (c.notice ? ' — ' + c.notice : ''))));
    });
    ap.appendChild(el('div', { class: 'about-foot' },
      tr('Licences tierces complètes : dossier licenses/ du portable (THIRD-PARTY-NOTICES.html, FFMPEG.txt) et shaders/CREDITS.txt. Réutilisation ciblée de Lanterne (MIT, même auteur).') +
      (about.website ? ' — ' + about.website : '')));
    root.appendChild(ap);

    return root;
  };

  /* ============================================== clavier remappable (Patch)
     Contrat KeyBinding : { key: "F5" | "Ctrl+3" | "Shift+G", command:
     CommandTemplate }. Le moteur ne fait que persister (EditOp
     key_binding_add/remove) — l'exécution est ICI : keydown global hors
     champs de saisie => touche → CommandTemplate → commande runtime.
     Les raccourcis SYSTÈME (Espace GO, Échap panic, B DBO, T tap, chiffres
     onglets, flèches nudge) restent prioritaires et non remappables. */

  var LEARN = { btn: null, done: null, prev: '' };
  var KEYS = { cmdKind: 'go', cueStr: '' };   /* état du formulaire (survit aux re-renders) */

  /* Chaîne humaine d'un keydown : "F5", "Ctrl+3", "Shift+G"… (null si
     modificateur seul). e.key est dépendant de la disposition clavier —
     c'est voulu : « la touche que vous tapez », AZERTY compris. */
  function keyToString(e) {
    var k = e.key;
    if (k === 'Control' || k === 'Shift' || k === 'Alt' || k === 'Meta' || k === 'AltGraph' || k === 'Dead') { return null; }
    var mods = '';
    if (e.ctrlKey) { mods += 'Ctrl+'; }
    if (e.altKey) { mods += 'Alt+'; }
    if (e.metaKey) { mods += 'Meta+'; }
    if (e.shiftKey) { mods += 'Shift+'; }
    if (k === ' ' || k === 'Spacebar') { k = 'Space'; }
    else if (k.length === 1) { k = k.toUpperCase(); }
    return mods + k;
  }

  /* Touche refusée en learn (réservée au système) => raison, sinon null. */
  var RESERVED_BARE = {
    Space: 'Espace = GO', Escape: 'Échap = Panic', B: 'B = DBO',
    T: 'T = Tap tempo', O: 'O = Notes de la cue en standby',
    Enter: 'Entrée est réservée (validation)', Tab: 'Tab est réservée (navigation)'
  };

  function reservedReason(ks) {
    if (ks.indexOf('+') >= 0) { return null; }   /* avec modificateur : libre */
    if (RESERVED_BARE[ks]) {
      return trf('Touche réservée : {0} (raccourci système, non remappable).', tr(RESERVED_BARE[ks]));
    }
    if (/^[0-9]$/.test(ks)) {
      return trf('Touche réservée : les chiffres changent d’onglet. Ajoutez un modificateur (ex. Ctrl+{0}).', ks);
    }
    if (ks.indexOf('Arrow') === 0) {
      return 'Touche réservée : les flèches font le nudge des coins (Mapping).';
    }
    return null;
  }

  /* Libellé humain d'un CommandTemplate. */
  function cmdTemplateLabel(t) {
    if (!t || !t.cmd) { return '?'; }
    switch (t.cmd) {
      case 'go': return 'GO';
      case 'back': return 'Back';
      case 'goto': return 'GOTO ' + cnStr(t.cue);
      case 'standby': return 'Standby ' + cnStr(t.cue);
      case 'dbo': return 'DBO' + (t.fade_s ? ' (fondu ' + fmtF(t.fade_s, 1) + ' s)' : '');
      case 'dbo_release': return 'DBO release';
      case 'tap_tempo': return 'Tap tempo';
      case 'panic': return 'Panic (fondu ' + fmtF(t.fade_s || 0, 1) + ' s)';
      case 'bpm_set': return 'BPM ' + t.bpm;
      case 'param_set': return 'Param ' + t.addr;
      case 'mode_set': return 'Mode ' + t.mode;
      default: return t.cmd;
    }
  }

  /* CommandTemplate → commande runtime (miroir de to_command côté Rust). */
  function runTemplate(t) {
    if (!t || !t.cmd) { return; }
    switch (t.cmd) {
      case 'go': go(); return;                                   /* garde anti double-GO */
      case 'back': back(); return;
      case 'goto': sendCmd({ cmd: 'cue_goto', cue: t.cue }); return;
      case 'standby': sendCmd({ cmd: 'cue_standby', cue: t.cue }); return;
      case 'panic': sendCmd({ cmd: 'cue_panic', fade_s: t.fade_s || 0 }); return;
      case 'dbo': sendCmd({ cmd: 'dbo', fade_s: t.fade_s || 0 }); return;
      case 'dbo_release': sendCmd({ cmd: 'dbo_release' }); return;
      case 'tap_tempo': sendCmd({ cmd: 'tap_tempo' }); return;
      case 'bpm_set': sendCmd({ cmd: 'bpm_set', bpm: t.bpm }); return;
      case 'mode_set': sendCmd({ cmd: 'mode_set', mode: t.mode }); return;
      case 'param_set': sendCmd({ cmd: 'param_set', addr: t.addr, value: t.value, source: 'ui' }); return;
      default: return;
    }
  }

  /* --- mode learn : capture de la prochaine touche --- */

  function cancelLearn() {
    if (!LEARN.btn) { return false; }
    LEARN.btn.classList.remove('key-learning');
    LEARN.btn.textContent = LEARN.prev;
    LEARN.btn = null;
    LEARN.done = null;
    return true;
  }

  function startLearn(btn, done) {
    cancelLearn();
    LEARN.btn = btn;
    LEARN.done = done;
    LEARN.prev = btn.textContent;
    btn.classList.add('key-learning');
    btn.textContent = tr('Appuyez sur une touche… (Échap : annuler)');
  }

  /* Écouteur en phase de CAPTURE : pendant le learn, la frappe est absorbée
     avant les raccourcis globaux (pas de GO/panic en plein apprentissage). */
  function installKeyLearn() {
    document.addEventListener('keydown', function (e) {
      if (!LEARN.btn) { return; }
      e.preventDefault();
      e.stopPropagation();
      if (e.key === 'Escape') { cancelLearn(); return; }
      var ks = keyToString(e);
      if (!ks) { return; }   /* modificateur seul : on attend la suite */
      var reason = reservedReason(ks);
      if (reason) { toast(reason, 'warn'); return; }   /* on reste en learn */
      var done = LEARN.done;
      cancelLearn();
      if (done) { done(ks); }
    }, true);
    document.addEventListener('pointerdown', function (e) {
      if (LEARN.btn && e.target !== LEARN.btn) { cancelLearn(); }
    }, true);
  }

  /* Ajout avec détection de conflit : touche déjà prise => remplacer ? */
  function addKeyBinding(ks, command) {
    var keys = patch().keys || [];
    var idx = -1;
    for (var i = 0; i < keys.length; i++) {
      if (keys[i].key === ks) { idx = i; break; }
    }
    function doAdd() {
      if (sendEdit({ op: 'key_binding_add', binding: { key: ks, command: command } })) {
        toast(el('span', null, el('kbd', null, ks), ' → ' + cmdTemplateLabel(command)), 'ok');
      }
    }
    if (idx >= 0) {
      confirmDialog({
        title: 'Touche déjà affectée',
        message: trf('« {0} » déclenche déjà : {1}.\nRemplacer par : {2} ?',
          ks, cmdTemplateLabel(keys[idx].command), cmdTemplateLabel(command)),
        confirm: 'Remplacer', danger: false,
        onConfirm: function () {
          sendEdit({ op: 'key_binding_remove', index: idx });
          doAdd();
        }
      });
      return;
    }
    doAdd();
  }

  /* Re-capture de la touche d'un binding existant (bouton Learn de la ligne). */
  function replaceKeyBinding(index, ks) {
    var keys = patch().keys || [];
    var b = keys[index];
    if (!b) { return; }
    if (b.key === ks) { return; }
    var other = -1;
    for (var i = 0; i < keys.length; i++) {
      if (i !== index && keys[i].key === ks) { other = i; break; }
    }
    function apply(removeOther) {
      var command = b.command;
      if (removeOther) {
        var hi = Math.max(index, other), lo = Math.min(index, other);
        sendEdit({ op: 'key_binding_remove', index: hi });
        sendEdit({ op: 'key_binding_remove', index: lo });
      } else {
        sendEdit({ op: 'key_binding_remove', index: index });
      }
      if (sendEdit({ op: 'key_binding_add', binding: { key: ks, command: command } })) {
        toast(el('span', null, el('kbd', null, ks), ' → ' + cmdTemplateLabel(command)), 'ok');
      }
    }
    if (other >= 0) {
      confirmDialog({
        title: 'Touche déjà affectée',
        message: trf('« {0} » déclenche déjà : {1}.\nRemplacer par : {2} ?',
          ks, cmdTemplateLabel(keys[other].command), cmdTemplateLabel(b.command)),
        confirm: 'Remplacer', danger: false,
        onConfirm: function () { apply(true); }
      });
      return;
    }
    apply(false);
  }

  /* CommandTemplate depuis le formulaire « nouveau raccourci ». */
  function buildKeyTemplate(kind, cueStr) {
    if (kind === 'goto' || kind === 'standby') {
      var n = cnParse(cueStr);
      if (n === null) { return null; }
      return { cmd: kind, cue: n };
    }
    if (kind === 'dbo') { return { cmd: 'dbo', fade_s: 0 }; }
    if (kind === 'panic') { return { cmd: 'panic', fade_s: 2 }; }
    return { cmd: kind };
  }

  /* Panneau Patch > Clavier. */
  function renderKeyPanel() {
    var panel = el('div', { class: 'panel' }, el('h2', null, 'Clavier'));

    /* raccourcis système, lecture seule */
    panel.appendChild(el('div', { class: 'muted', style: 'margin-bottom:6px' },
      'Raccourcis système — prioritaires, non remappables :'));
    var sys = el('div', { class: 'key-sys-list', style: 'margin-bottom:12px' });
    [['Espace', 'GO'], ['Échap', 'Panic (double appui : arrêt sec)'],
     ['B', 'DBO (maintien / double frappe)'], ['T', 'Tap tempo'],
     ['O', 'Notes de la cue en standby'], ['1–9, 0', 'Onglets'],
     ['Flèches', 'Nudge des coins (Mapping)'], ['Ctrl+Z', 'Annuler / rétablir']]
      .forEach(function (p) {
        sys.appendChild(el('span', null, el('kbd', null, tr(p[0])), ' ' + tr(p[1])));
      });
    panel.appendChild(sys);

    /* bindings personnalisés */
    var keys = patch().keys || [];
    var table = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Touche'),
        el('th', null, 'Commande'),
        el('th', { class: 'edit-only' }, ''),
        el('th', { class: 'edit-only' }, '')));
    keys.forEach(function (b, i) {
      var row = el('tr');
      row.appendChild(el('td', null, el('kbd', null, b.key || '?')));
      row.appendChild(el('td', null, cmdTemplateLabel(b.command)));
      var learnBtn = el('button', {
        class: 'edit-only',
        'data-tip': trf('Capture la prochaine touche pour remplacer « {0} » (Échap : annuler)', b.key)
      }, 'Learn');
      learnBtn.addEventListener('click', function () {
        startLearn(learnBtn, function (ks) { replaceKeyBinding(i, ks); });
      });
      row.appendChild(el('td', { class: 'edit-only' }, learnBtn));
      row.appendChild(el('td', { class: 'edit-only' },
        el('button', {
          class: 'danger', 'data-tip': 'Supprime ce raccourci',
          onclick: function () { sendEdit({ op: 'key_binding_remove', index: i }); }
        }, '✕')));
      table.appendChild(row);
    });
    panel.appendChild(el('div', { style: 'overflow-x:auto' }, table));
    if (!keys.length) {
      panel.appendChild(el('div', { style: 'padding:8px 0 0' },
        emptyState('plug', 'Aucun raccourci personnalisé — choisissez une commande puis « Learn » pour capturer une touche.')));
    }

    /* nouveau binding : commande + (cue) + Learn */
    var kinds = ['go', 'back', 'goto', 'standby', 'dbo', 'dbo_release', 'tap_tempo', 'panic'];
    var klabels = ['GO', 'Back', 'GOTO cue…', 'Standby cue…', 'DBO (sec)', 'DBO release', 'Tap tempo', 'Panic (fondu 2 s)'];
    var cueIn = el('input', {
      type: 'text', placeholder: 'n° de cue', value: KEYS.cueStr,
      style: 'width:90px;' + ((KEYS.cmdKind === 'goto' || KEYS.cmdKind === 'standby') ? '' : 'display:none'),
      'data-tip': 'Numéro de la cue visée (ex. 12.5)'
    });
    cueIn.addEventListener('input', function () { KEYS.cueStr = cueIn.value; });
    var csel = sel(kinds, klabels, KEYS.cmdKind, function (v) {
      KEYS.cmdKind = v;
      cueIn.style.display = (v === 'goto' || v === 'standby') ? '' : 'none';
    });
    csel.setAttribute('data-tip', 'Commande à déclencher par la touche');
    var learnNew = el('button', {
      class: 'primary',
      'data-tip': 'Capture la prochaine touche ou combinaison (ex. F5, Ctrl+3) — Échap pour annuler'
    }, 'Learn — capturer la touche');
    learnNew.addEventListener('click', function () {
      var tpl = buildKeyTemplate(KEYS.cmdKind, cueIn.value);
      if (!tpl) { uiWarn('Numéro de cue invalide (ex. 12.5).'); return; }
      startLearn(learnNew, function (ks) { addKeyBinding(ks, tpl); });
    });
    panel.appendChild(el('div', { class: 'toolbar edit-only', style: 'margin:10px 0 0' },
      el('span', { class: 'muted' }, 'Nouveau raccourci :'),
      csel, cueIn, learnNew));

    return panel;
  }

  /* ============================================================== clavier */

  function installKeyboard() {
    document.addEventListener('keydown', function (e) {
      /* --- mode learn actif : la capture (phase capture) a déjà tout
         absorbé — ceinture au cas où. --- */
      if (LEARN.btn) { return; }
      /* --- dialogue de confirmation ouvert : modal, il capte tout ---
         (Entrée = confirmer, Échap = annuler ; surtout PAS de GO sur
         Espace pendant qu'un dialogue est affiché). */
      if (confirmOpen()) {
        if (e.key === 'Escape') { e.preventDefault(); closeConfirm(false); }
        else if (e.key === 'Enter') { e.preventDefault(); closeConfirm(true); }
        return;
      }
      var t = e.target;
      var editing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
      /* --- Échap = panic universel, JAMAIS désactivé (même mode Show).
         Dans un champ : le 1er Échap sort du champ, le 2e déclenche.
         Exception : un menu contextuel ouvert consomme l'Échap (on ferme
         le menu, pas le plateau). --- */
      if (e.key === 'Escape') {
        if (closeCtxMenu()) { return; }
        if (editing) { t.blur(); return; }
        closeAnimPopover();   /* les popovers se ferment au passage */
        closeStatusPanel();
        closeUpdatePop();
        if (!e.repeat) { escPanic(); }
        return;
      }
      if (editing) { return; }
      /* --- Ctrl+Z / Ctrl+Maj+Z : annuler / rétablir (mode édition) --- */
      if ((e.ctrlKey || e.metaKey) && (e.key === 'z' || e.key === 'Z')) {
        e.preventDefault();
        if (!e.repeat) { if (e.shiftKey) { uiRedo(); } else { uiUndo(); } }
        return;
      }
      /* e.repeat : l'auto-repeat clavier ne doit JAMAIS déclencher une
         action de conduite (rafale de GO, strobe DBO, tap tempo faussé). */
      if (e.code === 'Space') { e.preventDefault(); if (!e.repeat) { go(); } return; }
      if (e.key === 'b' || e.key === 'B') { if (!e.repeat) { dboKeyDown(); } return; }
      if (e.key === 't' || e.key === 'T') { if (!e.repeat) { sendCmd({ cmd: 'tap_tempo' }); } return; }
      if (e.key === 'o' || e.key === 'O') { if (!e.repeat) { editStandbyNotes(); } return; }
      /* --- raccourcis remappables (patch.keys) — APRÈS les raccourcis
         système, AVANT les onglets (le learn refuse de toute façon les
         touches réservées nues). Actifs aussi en mode Show : c'est fait
         pour conduire. --- */
      var ks = keyToString(e);
      if (ks) {
        var kb = (patch().keys || []).find(function (b) { return b && b.key === ks; });
        if (kb) {
          e.preventDefault();
          if (!e.repeat) { runTemplate(kb.command); }
          return;
        }
      }
      if (/^[1-9]$/.test(e.key) && !e.ctrlKey && !e.altKey && !e.metaKey) {
        var tab = visibleTabs()[parseInt(e.key, 10) - 1];
        if (tab) { setTab(tab.id); }
        return;
      }
      if (e.key === '0' && !e.ctrlKey && !e.altKey && !e.metaKey) {
        var last = visibleTabs()[9];
        if (last) { setTab(last.id); }
        return;
      }
      if (S.tab === 'mapping') { mappingKey(e); }
    });
    document.addEventListener('keyup', function (e) {
      if (e.key === 'b' || e.key === 'B') { dboKeyUp(); }
    });
  }

  /* ======================================================== presse-papiers */

  function copyText(txt) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(txt).then(
        function () { uiInfo('Copié dans le presse-papiers.'); },
        function () { copyFallback(txt); });
    } else {
      copyFallback(txt);
    }
  }
  function copyFallback(txt) {
    var ta = el('textarea', { style: 'position:fixed;left:-9999px' });
    ta.value = txt;
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); uiInfo('Copié dans le presse-papiers.'); }
    catch (e) { uiWarn('Copie impossible.'); }
    document.body.removeChild(ta);
  }

  /* ===================================================== événements moteur */

  function onEvent(ev) {
    if (!ev || typeof ev !== 'object') { return; }
    var r = rt();
    switch (ev.type) {
      case 'cue_changed': r.active = ev.active; updateDyn(); break;
      case 'standby_changed': r.standby = ev.standby; updateDyn(); break;
      case 'transition_progress': r.progress = ev.progress; r.transition_active = ev.progress < 1; updateDyn(); break;
      case 'bpm_changed': r.bpm = ev.bpm; updateDyn(); break;
      case 'master_changed': r.master = ev.value; updateDyn(); break;
      case 'dbo_changed': r.dbo = ev.active; updateDyn(); break;
      case 'mode_changed':
        r.mode = ev.mode;
        renderAll();
        break;
      case 'health_tick': S.health = ev.snapshot; updateHealth(); break;
      case 'timecode_locked':
        toast(trf('Timecode verrouillé ({0} i/s).', tcRateLabel(ev.rate)), 'ok');
        break;
      case 'timecode_unlocked':
        toast('Signal timecode perdu — les cues actives continuent.', 'warn');
        break;
      case 'log_line':
        pushLog(ev.level, ev.target, ev.message);
        reactToLog(ev);
        break;
      case 'diagnostic_ready':
        /* zip prêt (chemins expurgés) — chemin cliquable dans le toast */
        toast(el('span', null, 'Rapport de diagnostic prêt : ',
          el('code', null, String(ev.path || ''))), 'ok');
        pushLog('info', 'ui', trf('Rapport de diagnostic : {0}', ev.path || ''));
        break;
      case 'warning':
        /* avertissement de conduite non bloquant (GO refusé par l'anti
           double-GO, commande impossible…) — throttlé côté moteur */
        uiWarn(tr(String(ev.message || 'Avertissement du moteur')));
        break;
      case 'show_loaded':
        /* re-synchronisation complète via un nouveau hello */
        Conduite.ws.reconnect();
        break;
      case 'recovery_available': {
        /* un fichier de récupération plus récent que le show a été trouvé
           au démarrage (arrêt sale probable) : proposer la restauration */
        RECOVERY.prompted = ev.path || '';
        var when = '';
        if (typeof ev.timestamp === 'number' && isFinite(ev.timestamp)) {
          var ms = ev.timestamp < 1e12 ? ev.timestamp * 1000 : ev.timestamp;
          try { when = new Date(ms).toLocaleString(I18N && I18N.lang() === 'en' ? 'en-GB' : 'fr-FR'); } catch (e1) { when = String(ev.timestamp); }
        } else if (ev.timestamp) {
          when = String(ev.timestamp);
        }
        confirmDialog({
          title: 'Récupération après arrêt inattendu',
          message: tr('Un état de récupération plus récent que le show enregistré a été trouvé')
            + (when ? ' (' + when + ')' : '') + ' :\n'
            + (ev.path || '') + '\n\n' + tr('Restaurer cet état ? « Ignorer » conserve le show tel qu’enregistré.'),
          confirm: 'Restaurer', cancel: 'Ignorer', danger: false,
          onConfirm: function () { sendCmd({ cmd: 'recovery_load', path: ev.path }); },
          onCancel: function () { sendCmd({ cmd: 'recovery_dismiss' }); }
        });
        break;
      }
      case 'edit_applied': {
        applyOp(ev.op || {});
        var opn = (ev.op && ev.op.op) || '';
        if (opn === 'corner_set') {
          /* pas de re-render pendant un drag : juste le canvas */
          if (!S.dragging) { drawMapping(); }
        } else if (opn === 'modulator_update' && S.dragging) {
          /* drag d'une bande sur le spectre : le canvas se redessine via la
             boucle d'animation, un re-render casserait le geste */
        } else if (opn === 'settings_update' && syncLang()) {
          /* changement de langue : le chrome (onglets, pied de page, badge de
             mode) est hors du panneau courant — il faut TOUT re-rendre. */
          renderAll();
        } else {
          requestRenderMain();
        }
        if (POP.el && (opn.indexOf('route_') === 0 || opn.indexOf('modulator_') === 0)) {
          renderAnimPopover();
        }
        break;
      }
      default:
        break;
    }
  }

  /* Réactions UI à certaines lignes de journal du moteur : la collecte de
     show et le diagnostic tournent en tâche de fond et ne publient (pour
     l'instant) que des logs — on en fait des toasts de fin d'opération. */
  function reactToLog(ev) {
    var msg = String(ev.message || '');
    if (String(ev.target || '').indexOf('app::session') < 0) { return; }
    if (msg.indexOf('show collecté') >= 0) {
      toast(el('span', null, 'Collecte terminée — dossier ',
        el('code', null, 'shows/' + (show().name || 'show') + '-collecte')), 'ok');
    } else if (msg.indexOf('collecte impossible') >= 0) {
      toast('Collecte du show impossible — détail dans le Journal.', 'err');
    } else if (msg.indexOf('rapport de diagnostic : pas encore disponible') >= 0) {
      toast('Rapport de diagnostic indisponible dans cette version du moteur.', 'warn');
    }
  }

  /* ================================================================ boot */

  /* Récupération post-crash proposée UNE fois par chemin — l'événement
     recovery_available au démarrage, ou runtime.recovery pour un client
     connecté après coup (le moteur garde l'info tant que rien n'est tranché). */
  var RECOVERY = { prompted: null };

  function maybePromptRecovery() {
    var rec = rt().recovery;
    if (!rec || typeof rec !== 'object' || !rec.path) { return; }
    if (RECOVERY.prompted === rec.path || confirmOpen()) { return; }
    onEvent({ type: 'recovery_available', path: rec.path, timestamp: rec.timestamp });
  }

  function onMessage(m) {
    switch (m.type) {
      case 'hello':
        S.raw = m.state || {};
        S.show = S.raw.show || null;
        S.runtime = S.raw.runtime || Object.assign({}, RT0);
        renderAll();
        maybePromptRecovery();
        break;
      case 'dyn':
        /* fft absent = pas d'entrée audio active (contrat WS) */
        S.fft = (m.fft && typeof m.fft === 'object' && Array.isArray(m.fft.bins)) ? m.fft : null;
        if (m.runtime && typeof m.runtime === 'object') {
          S.runtime = m.runtime;
          var wasShow = document.body.classList.contains('mode-show');
          if (wasShow !== isShowMode()) { renderAll(); }
          else { updateDyn(); }
          maybePromptRecovery();
        }
        break;
      case 'event':
        onEvent(m.event);
        break;
      case 'pong':
        break;
      default:
        break;
    }
  }

  function startClock() {
    setInterval(function () {
      var c = byId('clock');
      /* Horloge 24 h dans les deux langues : en régie on lit des heures de
         conduite, jamais un « 9:05:03 PM ». */
      if (c) { c.textContent = new Date().toLocaleTimeString('fr-FR', { hour12: false }); }
    }, 1000);
  }

  document.addEventListener('DOMContentLoaded', function () {
    installTooltips();
    installKeyLearn();     /* AVANT installKeyboard : capture prioritaire */
    installKeyboard();
    installDeferredRender();
    installAnimPopoverClose();
    installCtxMenuClose();
    startClock();
    var badge = byId('mode-badge');
    if (badge) {
      badge.addEventListener('dblclick', function () {
        sendCmd({ cmd: 'mode_set', mode: isShowMode() ? 'edit' : 'show' });
      });
      badge.addEventListener('keydown', function (e) {
        if (e.key === 'Enter') {
          e.preventDefault();
          sendCmd({ cmd: 'mode_set', mode: isShowMode() ? 'edit' : 'show' });
        }
      });
    }
    var warnChip = byId('warn-chip');
    if (warnChip) { warnChip.addEventListener('click', toggleStatusPanel); }
    var updBadge = byId('update-badge');
    if (updBadge) { updBadge.addEventListener('click', toggleUpdatePop); }
    loadAbout();
    Conduite.ws.on('open', function () { S.connected = true; refreshPreviews(); updateHealth(); loadAbout(); });
    Conduite.ws.on('close', function () { S.connected = false; updateHealth(); });
    Conduite.ws.on('message', onMessage);
    Conduite.ws.connect();
    renderAll();
  });

  /* Hooks de dev/test — aucun usage en fonctionnement normal :
     - debugInject : injecte un message comme s'il arrivait du WS (ex. une
       trame dyn avec fft factice pour vérifier l'analyseur sans micro) ;
     - debugRenderModulation : force un dessin synchrone du spectre et des
       aperçus LFO (la boucle rAF est en pause quand l'onglet est masqué). */
  Conduite.debugInject = onMessage;
  Conduite.debugRenderModulation = function () {
    var cv = byId('spectrum-canvas');
    if (cv) { drawSpectrum(cv); updateSpectrumStatus(); }
    document.querySelectorAll('canvas.lfo-preview').forEach(function (p) {
      drawLfoPreviewCanvas(p);
    });
  };

})();
