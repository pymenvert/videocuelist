/* Conduite — web UI v1 (vanilla, aucune dépendance).
   Store d'état minimal : `hello` pose S.show/S.runtime, les events et les
   trames "dyn" font des mises à jour ciblées. Tout est défensif : état
   absent => placeholders, jamais d'exception bloquante. */
'use strict';

(function () {

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
        else if (k === 'checked') { n.checked = !!v; }
        else { n.setAttribute(k, v); }
      });
    }
    for (var i = 2; i < arguments.length; i++) {
      appendChild(n, arguments[i]);
    }
    return n;
  }

  function appendChild(n, c) {
    if (c === null || c === undefined || c === false) { return; }
    if (Array.isArray(c)) { c.forEach(function (x) { appendChild(n, x); }); return; }
    n.appendChild(typeof c === 'object' ? c : document.createTextNode(String(c)));
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
    screen: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="4" width="19" height="13" rx="2"/><path d="M8 20.5h8M12 17v3.5"/></svg>'
  };

  /* État vide sympathique : icône + message. */
  function emptyState(icon, text) {
    var ic = el('span', { class: 'empty-icon', 'aria-hidden': 'true' });
    ic.innerHTML = ICONS[icon] || '';
    return el('div', { class: 'empty-state' }, ic, el('span', null, text));
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
        return 'Média : ' + (m ? m.name : ('#' + c.media));
      }
      if ('material' in c) {
        var mt = materials().find(function (x) { return x.id === c.material; });
        return 'Matériau : ' + (mt ? mt.name : ('#' + c.material));
      }
      if ('pattern' in c) { return 'Mire : ' + c.pattern; }
      if ('color' in c) { return 'Couleur'; }
    }
    return '?';
  }

  function defaultPlayback() { return { in_s: 0, out_s: null, speed: 1, end: 'loop' }; }

  function newCue(number) {
    return {
      number: number, name: 'Cue ' + cnStr(number), color: null, notes: '',
      transition: { kind: 'crossfade', dur_s: 1.0, curve: 'linear' },
      follow: 'manual', goto_after: null, states: [], mod_routes: [],
      triggers: { midi_note: null, osc: null }
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
      pushLog('warn', 'ui', 'Hors ligne — commande perdue : ' + (cmd && cmd.cmd));
      return false;
    }
    return true;
  }

  function sendEdit(op) {
    if (isShowMode()) {
      pushLog('warn', 'ui', 'Mode Show verrouillé — édition refusée (' + (op && op.op) + ')');
      return false;
    }
    var c = { cmd: 'edit' };
    Object.keys(op).forEach(function (k) { c[k] = op[k]; });
    return sendCmd(c);
  }

  function sendParam(addr, value, live) {
    return sendCmd({ cmd: 'param_set', addr: addr, value: value, source: 'ui' });
  }

  /* GO : anti-rafale — un GO au plus toutes les 250 ms, quel que soit le
     chemin (Espace, bouton, double événement). Une touche qui accroche ne
     fait pas défiler la conduite. */
  var lastGoTs = 0;

  function go() {
    var now = Date.now();
    if (now - lastGoTs < 250) { return; }
    lastGoTs = now;
    sendCmd({ cmd: 'cue_go' });
  }
  function back() { sendCmd({ cmd: 'cue_back' }); }

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

  /* Cue cible pour les assignations de contenu : standby, sinon active. */
  function targetCueNumber() {
    var r = rt();
    if (r.standby !== null && r.standby !== undefined) { return r.standby; }
    if (r.active !== null && r.active !== undefined) { return r.active; }
    var list = cues();
    return list.length ? list[0].number : null;
  }

  function assignContent(sliceId, content, playback) {
    var n = targetCueNumber();
    if (n === null) {
      pushLog('warn', 'ui', 'Aucune cue cible (standby/active) pour l’assignation.');
      return;
    }
    sendEdit({
      op: 'cue_update_state', number: n,
      state: { slice: sliceId, content: content, playback: playback || null, params: {} }
    });
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
        tip.textContent = target.getAttribute('data-tip') || '';
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

  /* ======================================================== onglets & rendu */

  var TABS = [
    { id: 'live', label: 'Live', tip: 'Conduite en jeu : cuelist, GO, préviews, master, DBO. Raccourci : 1' },
    { id: 'cues', label: 'Cues', tip: 'Édition de la cuelist : numéros, transitions, follow, notes. Raccourci : 2' },
    { id: 'mapping', label: 'Mapping', tip: 'Calage des slices : coins, nudge clavier, mires. Raccourci : 3' },
    { id: 'medias', label: 'Médias', tip: 'Pool de médias : vignettes, assignation, re-scan. Raccourci : 4' },
    { id: 'materiaux', label: 'Matériaux', tip: 'Shaders ISF/GLSL : assignation et paramètres. Raccourci : 5' },
    { id: 'modulation', label: 'Modulation', tip: 'LFO, bandes audio, routes, tap tempo. Raccourci : 6' },
    { id: 'patch', label: 'Patch', tip: 'OSC, MIDI, Art-Net : bindings et adresses. Raccourci : 7' },
    { id: 'sorties', label: 'Sorties', tip: 'Écrans / projecteurs : résolution, plein écran, identification. Raccourci : 8' },
    { id: 'journal', label: 'Journal', tip: 'Logs du moteur en direct. Raccourci : 9' },
    { id: 'reglages', label: 'Réglages', tip: 'Show, ports, langue, mode Édition/Show. Raccourci : 0' }
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
      }, el('span', { class: 'tab-key' }, key), t.label));
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
      main.appendChild(el('div', { class: 'panel danger-text' }, 'Erreur de rendu : ' + e.message));
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

  function renderAll() {
    var name = byId('show-name');
    if (name) { name.textContent = show().name || ''; }
    document.body.classList.toggle('mode-show', isShowMode());
    var badge = byId('mode-badge');
    if (badge) { badge.textContent = isShowMode() ? 'SHOW' : 'ÉDITION'; }
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

    right.appendChild(el('div', { class: 'preview-wrap' },
      el('span', { class: 'preview-label' }, 'PROGRAM'),
      previewImg('/preview.mjpeg', 'Préview program : ce qui sort réellement (~8 img/s)')));
    right.appendChild(el('div', { class: 'preview-wrap' },
      el('span', { class: 'preview-label' }, 'PRÉVIEW (STANDBY)'),
      previewImg('/preview-b.mjpeg', 'Préview de la cue en standby, rendue à blanc')));

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
          el('button', { 'data-tip': 'GOTO : saute directement à ce numéro de cue', onclick: function () { doGoto(gotoInput); } }, 'GOTO')))));

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

    right.appendChild(el('div', { class: 'panel' },
      el('h2', null, 'Master'),
      el('div', { id: 'master-row' },
        master,
        el('span', { id: 'master-val' }, Math.round(rt().master * 100) + ' %')),
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
      pushLog('warn', 'ui', 'Numéro de cue invalide : ' + input.value);
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

  function cuelistDom() {
    var wrap = el('div', { id: 'cuelist', 'data-tip': 'Clic : met la cue en standby. Double-clic : GOTO immédiat.' });
    var list = cues();
    if (!list.length) {
      wrap.appendChild(el('div', { style: 'padding:12px' },
        emptyState('list', 'Aucune cue — créez la conduite dans l’onglet Cues.')));
      return wrap;
    }
    list.forEach(function (c) {
      var row = el('div', { class: 'cue-row', 'data-cn': c.number },
        el('span', { class: 'cue-num' },
          c.color ? el('span', { class: 'cue-color-dot', style: 'background:' + c.color }) : null,
          cnStr(c.number)),
        el('span', { class: 'cue-name' }, c.name || ''),
        el('span', { class: 'cue-trans' }, transitionLabel(c.transition), followBadge(c)),
        c.notes ? el('span', { class: 'cue-notes' }, c.notes) : null,
        el('div', { class: 'cue-progress' }, el('div')));
      row.addEventListener('click', function () { sendCmd({ cmd: 'cue_standby', cue: c.number }); });
      row.addEventListener('dblclick', function () { sendCmd({ cmd: 'cue_goto', cue: c.number }); });
      wrap.appendChild(row);
    });
    return wrap;
  }

  function transitionLabel(t) {
    if (!t) { return ''; }
    var k = { cut: 'Cut', crossfade: 'Fondu', through_black: 'Par le noir' }[t.kind] || t.kind;
    return t.kind === 'cut' ? k : k + ' ' + fmtF(t.dur_s, 1) + ' s';
  }

  function followBadge(c) {
    var k = followKind(c.follow);
    if (k === 'after_media') { return ' • suit le média'; }
    if (k === 'wait') { return ' • attente ' + fmtF(followWait(c.follow), 1) + ' s'; }
    if (c.goto_after !== null && c.goto_after !== undefined) { return ' • boucle vers ' + cnStr(c.goto_after); }
    return '';
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
      if (r.remaining_s > 0) { line.appendChild(rtItem('RESTE', fmtF(r.remaining_s, 1) + ' s', '')); }
      if (r.transition_active) { line.appendChild(rtItem('TRANSITION', 'en cours…', '')); }
    }
    var master = byId('master-range');
    if (master && !MASTER.held && Date.now() >= MASTER.until) {
      master.value = r.master;
      var lbl = byId('master-val');
      if (lbl) { lbl.textContent = Math.round(r.master * 100) + ' %'; }
    }
    var dbo = byId('dbo-btn');
    if (dbo) {
      dbo.classList.toggle('engaged', !!r.dbo);
      var dlbl = dbo.querySelector('.dbo-label');
      if (dlbl) { dlbl.textContent = r.dbo ? 'DBO ACTIF — relâcher' : 'DBO'; }
    }
    var bpm = byId('bpm-val');
    if (bpm) { bpm.textContent = fmtF(r.bpm, 1); }
    (r.mod_levels || []).forEach(function (pair) {
      var m = byId('mod-meter-' + pair[0]);
      if (m) { m.style.width = Math.round(clamp(pair[1], 0, 1) * 100) + '%'; }
    });
  }

  function updateHealth() {
    var line = byId('health-line');
    if (!line) { return; }
    var h = S.health;
    line.textContent = '';
    if (!h) {
      line.appendChild(chip(S.connected ? '' : 'bad',
        S.connected ? 'En attente de données…' : 'Hors ligne'));
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
      line.appendChild(chip(cls, 'S' + p[0] + ' ' + fmtF(v, 0) + ' fps', 'Cadence de rendu de la sortie ' + p[0]));
    });
    var drops = 0;
    (h.drops || []).forEach(function (p) { drops += p[1]; });
    line.appendChild(chip(grade(drops, 1, 100), 'drops ' + drops, 'Frames perdues (cumul)'));
    line.appendChild(chip(grade(h.cpu_pct, 70, 90), 'CPU ' + fmtF(h.cpu_pct, 0) + ' %', 'Charge processeur du moteur'));
    line.appendChild(chip('', fmtF(h.mem_mb, 0) + ' Mo', 'Mémoire utilisée par le moteur'));
    if (h.temp_c !== null && h.temp_c !== undefined) {
      line.appendChild(chip(grade(h.temp_c, 70, 80), fmtF(h.temp_c, 0) + ' °C', 'Température (Raspberry Pi)'));
    }
    line.appendChild(chip(S.connected ? 'ok' : 'bad', 'WS ' + (S.connected ? 'OK' : 'coupé'),
      'Liaison WebSocket avec le moteur'));
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
        onclick: function () {
          if (!selCue) { return; }
          var copy = JSON.parse(JSON.stringify(selCue));
          var next = nextCueAfter(selCue.number);
          copy.number = next === null ? selCue.number + 1000
            : Math.floor((selCue.number + next) / 2);
          if (copy.number === selCue.number) {
            pushLog('warn', 'ui', 'Pas de place entre ' + cnStr(selCue.number) + ' et la suivante.');
            return;
          }
          copy.name += ' (copie)';
          sendEdit({ op: 'cue_add', cue: copy });
        }
      }, 'Dupliquer'),
      el('button', {
        class: 'danger', disabled: !selCue, 'data-tip': 'Supprime la cue sélectionnée',
        onclick: function () {
          if (selCue) { sendEdit({ op: 'cue_remove', number: selCue.number }); S.sel.cue = null; }
        }
      }, 'Supprimer'),
      el('span', { class: 'spacer' }),
      el('button', {
        class: 'primary', disabled: !selCue,
        'data-tip': 'Recopie l’état des slices de la cue active dans la cue sélectionnée (snapshot)',
        onclick: function () {
          if (!selCue) { return; }
          var active = cues().find(function (c) { return c.number === rt().active; });
          if (!active || !active.states || !active.states.length) {
            pushLog('warn', 'ui', 'Aucun état courant à enregistrer (pas de cue active).');
            return;
          }
          active.states.forEach(function (st) {
            sendEdit({ op: 'cue_update_state', number: selCue.number, state: JSON.parse(JSON.stringify(st)) });
          });
          pushLog('info', 'ui', 'État courant enregistré dans la cue ' + cnStr(selCue.number));
        }
      }, 'Enregistrer l’état courant dans la cue')));

    var table = el('table', { class: 'grid' },
      el('tr', null,
        el('th', { 'data-tip': 'Numéro décimal (1, 2, 2.5…) — insertion sans renumérotation' }, 'N°'),
        el('th', null, 'Nom'),
        el('th', { 'data-tip': 'Type de transition d’entrée' }, 'Transition'),
        el('th', { 'data-tip': 'Durée de la transition (secondes)' }, 'Durée'),
        el('th', { 'data-tip': 'Courbe d’interpolation' }, 'Courbe'),
        el('th', { 'data-tip': 'Enchaînement : GO manuel, fin de média, ou attente chronométrée' }, 'Follow'),
        el('th', { 'data-tip': 'Notes de régie (visibles en Live)' }, 'Notes'),
        el('th', { 'data-tip': 'Contenus posés par cette cue' }, 'Contenus')));

    cues().forEach(function (c) {
      table.appendChild(cueRow(c));
    });
    panel.appendChild(el('div', { style: 'overflow-x:auto' }, table));
    if (!cues().length) {
      panel.appendChild(el('div', { style: 'padding:10px 0 0' },
        emptyState('list', 'Aucune cue — bouton « Ajouter » pour commencer la conduite.')));
    }
    root.appendChild(panel);
    return root;
  };

  function cueRow(c) {
    var tr = el('tr', { class: c.number === S.sel.cue ? 'selected' : '' });
    tr.addEventListener('click', function (e) {
      if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT' || e.target.tagName === 'BUTTON') { return; }
      S.sel.cue = c.number;
      renderMain();
    });

    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(c));
      mut(copy);
      sendEdit({ op: 'cue_update', cue: copy });
    }

    var num = el('input', { type: 'text', value: cnStr(c.number), style: 'width:60px', 'data-tip': 'Renuméroter la cue (déplace dans la liste)' });
    num.addEventListener('change', function () {
      var n = cnParse(num.value);
      if (n === null || n === c.number) { num.value = cnStr(c.number); return; }
      if (cues().some(function (x) { return x.number === n; })) {
        pushLog('warn', 'ui', 'Le numéro ' + cnStr(n) + ' existe déjà.');
        num.value = cnStr(c.number);
        return;
      }
      var copy = JSON.parse(JSON.stringify(c));
      copy.number = n;
      sendEdit({ op: 'cue_remove', number: c.number });
      sendEdit({ op: 'cue_add', cue: copy });
      S.sel.cue = n;
    });
    tr.appendChild(el('td', null, num));

    var name = el('input', { type: 'text', value: c.name || '', 'data-tip': 'Nom de la cue' });
    name.addEventListener('change', function () { commit(function (x) { x.name = name.value; }); });
    tr.appendChild(el('td', null, name));

    var kind = sel(['cut', 'crossfade', 'through_black'], ['Cut', 'Fondu', 'Par le noir'],
      (c.transition || {}).kind || 'cut',
      function (v) { commit(function (x) { x.transition = x.transition || {}; x.transition.kind = v; }); });
    kind.setAttribute('data-tip', 'Cut : bascule sèche. Fondu : crossfade A/B. Par le noir : descente puis montée.');
    tr.appendChild(el('td', null, kind));

    var dur = el('input', { type: 'number', min: 0, step: 0.1, value: (c.transition || {}).dur_s || 0 });
    dur.addEventListener('change', function () {
      commit(function (x) { x.transition = x.transition || {}; x.transition.dur_s = parseFloat(dur.value) || 0; });
    });
    tr.appendChild(el('td', null, dur));

    var curve = sel(['linear', 'ease_in', 'ease_out', 'ease_in_out', 's_curve'],
      ['Linéaire', 'Ease in', 'Ease out', 'Ease in-out', 'Courbe S'],
      (c.transition || {}).curve || 'linear',
      function (v) { commit(function (x) { x.transition = x.transition || {}; x.transition.curve = v; }); });
    tr.appendChild(el('td', null, curve));

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
    tr.appendChild(el('td', null, fsel, fwait));

    var notes = el('input', { type: 'text', value: c.notes || '', 'data-tip': 'Notes de régie' });
    notes.addEventListener('change', function () { commit(function (x) { x.notes = notes.value; }); });
    tr.appendChild(el('td', null, notes));

    var contents = (c.states || []).map(function (st) {
      var slice = slices().find(function (s) { return s.id === st.slice; });
      return (slice ? slice.name : ('slice ' + st.slice)) + ' : ' + contentLabel(st.content);
    }).join(' | ') || '—';
    tr.appendChild(el('td', { class: 'muted' }, contents));

    return tr;
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
              id: id, name: 'Slice ' + id, output: S.sel.output,
              corners: [[0, 0], [1, 0], [1, 1], [0, 1]],
              src: { x: 0, y: 0, w: 1, h: 1 }, z: 0, enabled: true
            }
          });
          S.sel.slice = id;
        }
      }, '+ Slice'),
      el('button', {
        class: 'danger', disabled: !selSlice, 'data-tip': 'Supprime le slice sélectionné',
        onclick: function () {
          if (selSlice) { sendEdit({ op: 'slice_remove', id: selSlice.id }); S.sel.slice = null; renderMain(); }
        }
      }, '− Slice')));

    slicesOfOutput().forEach(function (s) {
      slicePanel.appendChild(el('div', {
        class: 'slice-item' + (s.id === S.sel.slice ? ' selected' : ''),
        'data-tip': 'Sélectionne ce slice dans l’éditeur',
        onclick: function () { S.sel.slice = s.id; S.sel.corner = null; renderMain(); }
      }, s.name + ' (z ' + s.z + (s.enabled ? '' : ', désactivé') + ')'));
    });
    if (!slicesOfOutput().length) {
      slicePanel.appendChild(emptyState('screen', 'Aucun slice sur cette sortie — « + Slice » pour caler une zone.'));
    }
    side.appendChild(slicePanel);

    /* mires */
    side.appendChild(el('div', { class: 'panel edit-only' },
      el('h2', null, 'Mires'),
      el('div', { class: 'toolbar' },
        el('button', {
          disabled: !selSlice, 'data-tip': 'Pose la mire d’identification sur le slice sélectionné (dans la cue standby/active)',
          onclick: function () { if (selSlice) { assignContent(selSlice.id, { pattern: 'ident' }); } }
        }, 'Mire slice'),
        el('button', {
          'data-tip': 'Pose la grille de convergence sur tous les slices de la sortie',
          onclick: function () { slicesOfOutput().forEach(function (s) { assignContent(s.id, { pattern: 'grid' }); }); }
        }, 'Mire globale'),
        el('button', {
          'data-tip': 'Retire les mires : contenu « aucun » sur tous les slices de la sortie',
          onclick: function () { slicesOfOutput().forEach(function (s) { assignContent(s.id, 'none'); }); }
        }, 'Éteindre'))));

    /* paramètres du slice */
    if (selSlice) {
      var p = el('div', { class: 'panel' }, el('h2', null, selSlice.name + ' — paramètres'));
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
      'data-tip': label + ' — adresse : ' + addr
    });
    var val = el('span', { class: 'val' }, fmtF(def));
    input.addEventListener('input', function () {
      var v = parseFloat(input.value);
      val.textContent = fmtF(v);
      sendParam(addr, { f: v });
    });
    return el('div', { class: 'param-row' }, el('span', null, label), input, val);
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
      ctx.font = '13px system-ui';
      ctx.textAlign = 'center';
      ctx.fillText(s.name || ('slice ' + s.id), cx, cy);
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

  RENDERERS.medias = function () {
    var root = el('section', { class: 'tab-panel' });
    var panel = el('div', { class: 'panel' }, el('h2', null, 'Pool de médias'));

    panel.appendChild(el('div', { class: 'toolbar' },
      el('button', {
        class: 'edit-only', 'data-tip': 'Re-scanne le dossier media/ : nouveaux fichiers, vignettes, état manquant',
        onclick: function () { sendCmd({ cmd: 'media_rescan' }); }
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
      }, 'Assigner au slice ' + (S.sel.slice !== null ? '« ' + (currentSlice() || {}).name + ' »' : '')),
      el('span', { class: 'muted' },
        S.sel.media !== null ? ('Sélection : ' + contentLabel({ media: S.sel.media })) : 'Aucun média sélectionné')));

    var grid = el('div', { id: 'media-grid' });
    medias().forEach(function (m) {
      var img = el('img', { class: 'thumb', src: '/thumb/' + m.id + '.jpg', alt: m.name });
      img.addEventListener('error', function () {
        var ph = el('div', { class: 'thumb-placeholder' }, m.missing ? 'MANQUANT' : 'pas de vignette');
        if (img.parentNode) { img.parentNode.replaceChild(ph, img); }
      });
      var card = el('div', {
        class: 'media-card' + (m.id === S.sel.media ? ' selected' : '') + (m.missing ? ' missing' : ''),
        'data-tip': m.path + (m.missing ? ' — FICHIER MANQUANT' : '') + ' — clic : sélectionner, double-clic : assigner au slice'
      },
        img,
        m.missing ? el('span', { class: 'badge-missing' }, 'MANQUANT') : null,
        el('div', { class: 'media-name' }, m.name),
        el('div', { class: 'media-meta' },
          m.missing ? 'fichier introuvable' :
            (m.width + '×' + m.height + (m.duration_s ? ' • ' + fmtF(m.duration_s, 1) + ' s' : ''))));
      card.addEventListener('click', function () { S.sel.media = m.id; renderMain(); });
      card.addEventListener('dblclick', function () {
        S.sel.media = m.id;
        if (S.sel.slice !== null) { assignContent(S.sel.slice, { media: m.id }, defaultPlayback()); }
      });
      grid.appendChild(card);
    });
    if (!medias().length) {
      grid.appendChild(emptyState('film', 'Aucun média — déposez des fichiers dans media/ puis « Re-scanner ».'));
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
        class: 'primary', disabled: S.sel.material === null || S.sel.slice === null,
        'data-tip': S.sel.slice === null
          ? 'Sélectionnez d’abord un slice (onglet Mapping)'
          : 'Assigne le matériau sélectionné au slice sélectionné, dans la cue standby/active',
        onclick: function () {
          if (S.sel.material !== null && S.sel.slice !== null) {
            assignContent(S.sel.slice, { material: S.sel.material });
          }
        }
      }, 'Assigner au slice ' + (S.sel.slice !== null ? '« ' + (currentSlice() || {}).name + ' »' : ''))));

    var table = el('table', { class: 'grid' },
      el('tr', null, el('th', null, 'Nom'), el('th', null, 'Fichier')));
    materials().forEach(function (m) {
      var tr = el('tr', {
        class: m.id === S.sel.material ? 'selected' : '',
        'data-tip': 'Clic : sélectionner ce matériau'
      },
        el('td', null, m.name), el('td', { class: 'muted' }, m.path));
      tr.addEventListener('click', function () { S.sel.material = m.id; renderMain(); });
      table.appendChild(tr);
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
        right.appendChild(el('div', { class: 'muted' },
          'Spécifications ISF indisponibles pour ce matériau (le moteur ne les publie pas encore). ' +
          'Adresses : ' + prefix + '<input>.'));
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
    return el('div', { class: 'param-row' }, el('span', null, label), input, val);
  }

  /* =========================================================== MODULATION */

  RENDERERS.modulation = function () {
    var root = el('section', { class: 'tab-panel' });

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
          class: 'primary', 'data-tip': 'Tap tempo (touche T) : taper en rythme pour poser le BPM',
          onclick: function () { sendCmd({ cmd: 'tap_tempo' }); }
        }, 'TAP'),
        el('span', { id: 'bpm-val', style: 'font-size:20px;font-variant-numeric:tabular-nums' }, fmtF(rt().bpm, 1)),
        el('span', { class: 'muted' }, 'BPM'),
        bpmIn)));

    /* modulateurs */
    var modPanel = el('div', { class: 'panel' }, el('h2', null, 'Modulateurs'));
    modPanel.appendChild(el('div', { class: 'toolbar edit-only' },
      el('button', {
        'data-tip': 'Ajoute un LFO sinus 1 Hz',
        onclick: function () {
          var id = modulators().reduce(function (m, x) { return Math.max(m, x.id); }, 0) + 1;
          sendEdit({
            op: 'modulator_add',
            modulator: { id: id, name: 'LFO ' + id, kind: { lfo: { wave: 'sine', freq: { hz: 1 }, phase: 0 } } }
          });
        }
      }, '+ LFO'),
      el('button', {
        'data-tip': 'Ajoute une bande audio (60–120 Hz)',
        onclick: function () {
          var id = modulators().reduce(function (m, x) { return Math.max(m, x.id); }, 0) + 1;
          sendEdit({
            op: 'modulator_add',
            modulator: {
              id: id, name: 'Bande ' + id,
              kind: { audio_band: { low_hz: 60, high_hz: 120, gain: 1, floor: 0.05, attack_ms: 10, release_ms: 200 } }
            }
          });
        }
      }, '+ Bande audio')));

    var table = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Nom'), el('th', null, 'Type'), el('th', null, 'Réglages'),
        el('th', { 'data-tip': 'Niveau instantané du modulateur (vumètre temps réel)' }, 'Niveau'),
        el('th', { class: 'edit-only' }, '')));
    modulators().forEach(function (m) { table.appendChild(modulatorRow(m)); });
    modPanel.appendChild(el('div', { style: 'overflow-x:auto' }, table));
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
        'data-tip': 'Branche le premier modulateur sur le master (à modifier ensuite)',
        onclick: function () {
          var id = routes().reduce(function (m, r) { return Math.max(m, r.id); }, 0) + 1;
          sendEdit({
            op: 'route_add',
            route: { id: id, source: modulators()[0].id, target_addr: 'master/intensity', depth: 0.5, mode: 'add' }
          });
        }
      }, '+ Route')));
    var rtable = el('table', { class: 'grid' },
      el('tr', null,
        el('th', null, 'Source'), el('th', null, 'Cible'),
        el('th', { 'data-tip': 'Profondeur par défaut (les cues peuvent la surcharger)' }, 'Profondeur'),
        el('th', { 'data-tip': 'Add : base + signal. Mul : atténuation. Replace : remplace la base.' }, 'Mode'),
        el('th', { class: 'edit-only' }, '')));
    routes().forEach(function (r) { rtable.appendChild(routeRow(r)); });
    routePanel.appendChild(el('div', { style: 'overflow-x:auto' }, rtable));
    if (!routes().length) {
      routePanel.appendChild(el('div', { style: 'padding:10px 0 0' },
        emptyState('wave', 'Aucune route — un modulateur n’agit que routé vers un paramètre.')));
    }
    root.appendChild(routePanel);

    return root;
  };

  function modulatorRow(m) {
    var tr = el('tr');
    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(m));
      mut(copy);
      sendEdit({ op: 'modulator_update', modulator: copy });
    }
    var name = el('input', { type: 'text', value: m.name || '' });
    name.addEventListener('change', function () { commit(function (x) { x.name = name.value; }); });
    tr.appendChild(el('td', null, name));

    var isLfo = m.kind && typeof m.kind === 'object' && 'lfo' in m.kind;
    tr.appendChild(el('td', { class: 'muted' }, isLfo ? 'LFO' : 'Bande audio'));

    var cfg = el('td');
    if (isLfo) {
      var lfo = m.kind.lfo || {};
      var waveVal = typeof lfo.wave === 'string' ? lfo.wave : 'square';
      var wave = sel(['sine', 'tri', 'square', 'saw', 'random_sh', 'drift'],
        ['Sinus', 'Triangle', 'Carré', 'Dent de scie', 'Random S&H', 'Drift'],
        waveVal,
        function (v) {
          commit(function (x) { x.kind.lfo.wave = v === 'square' ? { square: { pw: 0.5 } } : v; });
        });
      wave.setAttribute('data-tip', 'Forme d’onde du LFO');
      var isBpm = lfo.freq && typeof lfo.freq === 'object' && 'bpm_sync' in lfo.freq;
      var freqVal = isBpm ? lfo.freq.bpm_sync.mult : (lfo.freq && lfo.freq.hz !== undefined ? lfo.freq.hz : 1);
      var mode = sel(['hz', 'bpm'], ['Hz', 'Sync BPM'], isBpm ? 'bpm' : 'hz', applyFreq);
      mode.setAttribute('data-tip', 'Fréquence en Hz fixes, ou multiplicateur du BPM (0,25 = 1 cycle sur 4 temps)');
      var fin = el('input', { type: 'number', step: 0.01, min: 0, value: freqVal, style: 'width:70px' });
      fin.addEventListener('change', applyFreq);
      function applyFreq() {
        var v = parseFloat(fin.value) || 1;
        commit(function (x) {
          x.kind.lfo.freq = mode.value === 'bpm' ? { bpm_sync: { mult: v } } : { hz: v };
        });
      }
      cfg.appendChild(el('span', { class: 'toolbar' }, wave, mode, fin));
    } else {
      var ab = (m.kind && m.kind.audio_band) || {};
      var lo = el('input', { type: 'number', value: ab.low_hz || 0, style: 'width:70px', 'data-tip': 'Borne basse (Hz)' });
      var hi = el('input', { type: 'number', value: ab.high_hz || 0, style: 'width:70px', 'data-tip': 'Borne haute (Hz)' });
      function applyBand() {
        commit(function (x) {
          x.kind.audio_band.low_hz = parseFloat(lo.value) || 0;
          x.kind.audio_band.high_hz = parseFloat(hi.value) || 0;
        });
      }
      lo.addEventListener('change', applyBand);
      hi.addEventListener('change', applyBand);
      cfg.appendChild(el('span', { class: 'toolbar' }, lo, el('span', { class: 'muted' }, '→'), hi, el('span', { class: 'muted' }, 'Hz')));
    }
    tr.appendChild(cfg);

    tr.appendChild(el('td', null,
      el('div', { class: 'mod-meter' }, el('div', { id: 'mod-meter-' + m.id }))));

    tr.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime ce modulateur',
        onclick: function () { sendEdit({ op: 'modulator_remove', id: m.id }); }
      }, 'X')));
    return tr;
  }

  function routeRow(r) {
    var tr = el('tr');
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
    tr.appendChild(el('td', null, src));

    var addrs = paramAddrs();
    if (addrs.indexOf(r.target_addr) < 0) { addrs.unshift(r.target_addr); }
    var tgt = sel(addrs, addrs, r.target_addr, function (v) { commit(function (x) { x.target_addr = v; }); });
    tgt.setAttribute('data-tip', 'Paramètre cible (adresse stable)');
    tr.appendChild(el('td', null, tgt));

    var depth = el('input', { type: 'range', min: 0, max: 1, step: 0.01, value: r.depth });
    var dval = el('span', { class: 'val' }, fmtF(r.depth));
    depth.addEventListener('input', function () { dval.textContent = fmtF(parseFloat(depth.value)); });
    depth.addEventListener('change', function () { commit(function (x) { x.depth = parseFloat(depth.value); }); });
    tr.appendChild(el('td', null, el('span', { class: 'toolbar' }, depth, dval)));

    var mode = sel(['add', 'mul', 'replace'], ['Add', 'Mul', 'Replace'], r.mode,
      function (v) { commit(function (x) { x.mode = v; }); });
    tr.appendChild(el('td', null, mode));

    tr.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime cette route',
        onclick: function () { sendEdit({ op: 'route_remove', id: r.id }); }
      }, 'X')));
    return tr;
  }

  /* ================================================================ PATCH */

  RENDERERS.patch = function () {
    var root = el('section', { class: 'tab-panel' });

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
      'Nœud ' + (st.artnet_enabled ? 'actif' : 'inactif') +
      ' — univers écoutés : ' + ((st.artnet_universes || []).join(', ') || '—')));
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

    return root;
  };

  function addrChip(addr) {
    var chip = el('span', { class: 'addr-chip', 'data-tip': 'Copier ' + addr }, addr);
    chip.addEventListener('click', function () { copyText(addr); });
    return chip;
  }

  function midiRow(b, index) {
    var tr = el('tr');
    var isNote = b && typeof b === 'object' && 'note' in b;
    var body = isNote ? b.note : (b && b.cc) || {};

    function commit(mut) {
      var copy = JSON.parse(JSON.stringify(b));
      mut(isNote ? copy.note : copy.cc);
      sendEdit({ op: 'patch_midi_update', index: index, binding: copy });
    }

    tr.appendChild(el('td', null, isNote ? 'Note' : ('CC' + (body.fourteen_bits ? ' 14 bits' : ''))));

    var ch = el('input', { type: 'number', min: 1, max: 16, value: (body.channel || 0) + 1, 'data-tip': 'Canal MIDI 1–16' });
    ch.addEventListener('change', function () {
      commit(function (x) { x.channel = clamp((parseInt(ch.value, 10) || 1) - 1, 0, 15); });
    });
    tr.appendChild(el('td', null, ch));

    var num = el('input', { type: 'number', min: 0, max: 127, value: isNote ? body.note : body.cc });
    num.addEventListener('change', function () {
      commit(function (x) {
        var v = clamp(parseInt(num.value, 10) || 0, 0, 127);
        if (isNote) { x.note = v; } else { x.cc = v; }
      });
    });
    tr.appendChild(el('td', null, num));

    if (isNote) {
      var cmds = ['go', 'back', 'dbo', 'dbo_release', 'tap_tempo', 'panic'];
      var cur = body.command && body.command.cmd ? body.command.cmd : 'go';
      var csel = sel(cmds, ['GO', 'Back', 'DBO', 'DBO release', 'Tap tempo', 'Panic'], cur, function (v) {
        commit(function (x) {
          x.command = (v === 'dbo' || v === 'panic') ? { cmd: v, fade_s: v === 'dbo' ? 0 : 2 } : { cmd: v };
        });
      });
      csel.setAttribute('data-tip', 'Commande déclenchée par la note');
      tr.appendChild(el('td', null, csel));
      tr.appendChild(el('td', { class: 'muted' }, '—'));
    } else {
      var addrs = paramAddrs();
      if (addrs.indexOf(body.addr) < 0) { addrs.unshift(body.addr || ''); }
      var asel = sel(addrs, addrs, body.addr, function (v) { commit(function (x) { x.addr = v; }); });
      tr.appendChild(el('td', null, asel));
      var pk = el('input', { type: 'checkbox', checked: !!body.pickup });
      pk.addEventListener('change', function () { commit(function (x) { x.pickup = pk.checked; }); });
      tr.appendChild(el('td', null, pk));
    }

    tr.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime ce binding',
        onclick: function () { sendEdit({ op: 'patch_midi_remove', index: index }); }
      }, 'X')));
    return tr;
  }

  function artnetRow(e, index) {
    var tr = el('tr');
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
    tr.appendChild(numCell('universe', 0, 32767));
    tr.appendChild(numCell('channel', 1, 512));
    var bits = sel(['eight', 'sixteen'], ['8 bits', '16 bits'], e.bits, function (v) {
      commit(function (x) { x.bits = v; });
    });
    tr.appendChild(el('td', null, bits));
    var addrs = paramAddrs();
    if (addrs.indexOf(e.addr) < 0) { addrs.unshift(e.addr || ''); }
    var asel = sel(addrs, addrs, e.addr, function (v) { commit(function (x) { x.addr = v; }); });
    tr.appendChild(el('td', null, asel));
    tr.appendChild(numCell('min', -1e9, 1e9, 0.01));
    tr.appendChild(numCell('max', -1e9, 1e9, 0.01));
    tr.appendChild(numCell('smoothing_ms', 0, 10000));
    tr.appendChild(el('td', { class: 'edit-only' },
      el('button', {
        class: 'danger', 'data-tip': 'Supprime cette entrée de patch',
        onclick: function () { sendEdit({ op: 'patch_artnet_remove', index: index }); }
      }, 'X')));
    return tr;
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
            output: { id: id, name: 'Sortie ' + id, monitor_index: null, width: 1920, height: 1080, fullscreen: true, enabled: false }
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
      var tr = el('tr');
      function commit(mut) {
        var copy = JSON.parse(JSON.stringify(o));
        mut(copy);
        sendEdit({ op: 'output_update', output: copy });
      }
      var name = el('input', { type: 'text', value: o.name || '' });
      name.addEventListener('change', function () { commit(function (x) { x.name = name.value; }); });
      tr.appendChild(el('td', null, name));

      var mon = el('input', { type: 'number', min: 0, value: o.monitor_index === null || o.monitor_index === undefined ? '' : o.monitor_index, placeholder: '—' });
      mon.addEventListener('change', function () {
        commit(function (x) { x.monitor_index = mon.value === '' ? null : Math.max(0, parseInt(mon.value, 10) || 0); });
      });
      tr.appendChild(el('td', null, mon));

      var w = el('input', { type: 'number', min: 1, value: o.width });
      w.addEventListener('change', function () { commit(function (x) { x.width = Math.max(1, parseInt(w.value, 10) || 1920); }); });
      tr.appendChild(el('td', null, w));
      var h = el('input', { type: 'number', min: 1, value: o.height });
      h.addEventListener('change', function () { commit(function (x) { x.height = Math.max(1, parseInt(h.value, 10) || 1080); }); });
      tr.appendChild(el('td', null, h));

      var fs = el('input', { type: 'checkbox', checked: !!o.fullscreen });
      fs.addEventListener('change', function () { commit(function (x) { x.fullscreen = fs.checked; }); });
      tr.appendChild(el('td', null, fs));

      var en = el('input', { type: 'checkbox', checked: !!o.enabled });
      en.addEventListener('change', function () { commit(function (x) { x.enabled = en.checked; }); });
      tr.appendChild(el('td', null, en));

      tr.appendChild(el('td', null,
        el('button', {
          class: 'edit-only',
          'data-tip': 'Affiche la mire d’identification (nom + numéro) sur tous les slices de cette sortie',
          onclick: function () {
            slices().filter(function (s) { return s.output === o.id; })
              .forEach(function (s) { assignContent(s.id, { pattern: 'ident' }); });
          }
        }, 'Identifier')));

      tr.appendChild(el('td', { class: 'edit-only' },
        el('button', {
          class: 'danger', 'data-tip': 'Supprime cette sortie',
          onclick: function () { sendEdit({ op: 'output_remove', id: o.id }); }
        }, 'X')));
      table.appendChild(tr);
    });

    panel.appendChild(el('div', { style: 'overflow-x:auto' }, table));
    if (!outputs().length) {
      panel.appendChild(el('div', { style: 'padding:10px 0 0' },
        emptyState('screen', 'Aucune sortie — « + Sortie » puis activez-la pour projeter.')));
    }
    root.appendChild(panel);
    return root;
  };

  /* ============================================================== JOURNAL */

  RENDERERS.journal = function () {
    var root = el('section', { class: 'tab-panel' });
    var panel = el('div', { class: 'panel' }, el('h2', null, 'Journal'));

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
    if (count) { count.textContent = filteredLogs().length + ' ligne(s)'; }
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
    var loadName = el('input', { type: 'text', placeholder: 'nom du show à charger' });
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
          'data-tip': 'Charge un show existant (le show courant doit être enregistré)',
          onclick: function () { if (loadName.value.trim()) { sendCmd({ cmd: 'show_load', name: loadName.value.trim() }); } }
        }, 'Charger')),
      el('button', {
        class: 'edit-only', 'data-tip': 'Nouveau show vide',
        onclick: function () { sendCmd({ cmd: 'show_new' }); }
      }, 'Nouveau'),
      el('button', {
        class: 'edit-only',
        'data-tip': 'Collecter le show : copie tous les médias dans un dossier autonome (clé USB, autre machine)',
        onclick: function () { sendCmd({ cmd: 'show_collect' }); }
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
    var lang = sel(['fr', 'en'], ['Français', 'English (à venir)'], st.language || 'fr', function (v) {
      commitSettings(function (x) { x.language = v; });
      if (v === 'en') { pushLog('info', 'ui', 'Interface anglaise : à venir — le réglage est enregistré.'); }
    });
    lang.setAttribute('data-tip', 'Langue de l’interface (anglais : à venir)');
    var fps = el('input', { type: 'number', min: 1, max: 30, value: st.mjpeg_fps || 8, 'data-tip': 'Cadence des préviews MJPEG (img/s)' });
    fps.addEventListener('change', function () {
      commitSettings(function (x) { x.mjpeg_fps = clamp(parseInt(fps.value, 10) || 8, 1, 30); });
    });
    cfgPanel.appendChild(el('div', { class: 'settings-grid' },
      el('span', null, 'Port OSC entrant'), portInput('osc_in_port', 'Port UDP d’écoute OSC (défaut 9000) — redémarrage du service OSC'),
      el('span', null, 'Port OSC sortant'), portInput('osc_out_port', 'Port de feedback OSC par défaut'),
      el('span', null, 'Langue'), lang,
      el('span', null, 'Préview (img/s)'), fps));
    root.appendChild(cfgPanel);

    return root;
  };

  /* ============================================================== clavier */

  function installKeyboard() {
    document.addEventListener('keydown', function (e) {
      var t = e.target;
      var editing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
      if (editing) {
        if (e.key === 'Escape') { t.blur(); }
        return;
      }
      /* e.repeat : l'auto-repeat clavier ne doit JAMAIS déclencher une
         action de conduite (rafale de GO, strobe DBO, tap tempo faussé). */
      if (e.code === 'Space') { e.preventDefault(); if (!e.repeat) { go(); } return; }
      if (e.key === 'b' || e.key === 'B') { if (!e.repeat) { dboKeyDown(); } return; }
      if (e.key === 't' || e.key === 'T') { if (!e.repeat) { sendCmd({ cmd: 'tap_tempo' }); } return; }
      if (/^[1-9]$/.test(e.key)) {
        var tab = visibleTabs()[parseInt(e.key, 10) - 1];
        if (tab) { setTab(tab.id); }
        return;
      }
      if (e.key === '0') {
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
        function () { pushLog('info', 'ui', 'Copié dans le presse-papiers.'); },
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
    try { document.execCommand('copy'); pushLog('info', 'ui', 'Copié.'); }
    catch (e) { pushLog('warn', 'ui', 'Copie impossible.'); }
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
      case 'log_line': pushLog(ev.level, ev.target, ev.message); break;
      case 'show_loaded':
        /* re-synchronisation complète via un nouveau hello */
        Conduite.ws.reconnect();
        break;
      case 'edit_applied':
        applyOp(ev.op || {});
        if (ev.op && ev.op.op === 'corner_set') {
          /* pas de re-render pendant un drag : juste le canvas */
          if (!S.dragging) { drawMapping(); }
        } else {
          requestRenderMain();
        }
        break;
      default:
        break;
    }
  }

  /* ================================================================ boot */

  function onMessage(m) {
    switch (m.type) {
      case 'hello':
        S.raw = m.state || {};
        S.show = S.raw.show || null;
        S.runtime = S.raw.runtime || Object.assign({}, RT0);
        renderAll();
        break;
      case 'dyn':
        if (m.runtime && typeof m.runtime === 'object') {
          S.runtime = m.runtime;
          var wasShow = document.body.classList.contains('mode-show');
          if (wasShow !== isShowMode()) { renderAll(); }
          else { updateDyn(); }
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
      if (c) { c.textContent = new Date().toLocaleTimeString('fr-FR'); }
    }, 1000);
  }

  document.addEventListener('DOMContentLoaded', function () {
    installTooltips();
    installKeyboard();
    installDeferredRender();
    startClock();
    var badge = byId('mode-badge');
    if (badge) {
      badge.addEventListener('dblclick', function () {
        sendCmd({ cmd: 'mode_set', mode: isShowMode() ? 'edit' : 'show' });
      });
    }
    Conduite.ws.on('open', function () { S.connected = true; refreshPreviews(); updateHealth(); });
    Conduite.ws.on('close', function () { S.connected = false; updateHealth(); });
    Conduite.ws.on('message', onMessage);
    Conduite.ws.connect();
    renderAll();
  });

})();
