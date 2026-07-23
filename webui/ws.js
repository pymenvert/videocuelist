/* Conduite — couche WebSocket : reconnexion automatique avec backoff,
   bandeau "reconnexion…", ping périodique. Aucune dépendance. */
'use strict';

window.Conduite = window.Conduite || {};

Conduite.ws = (function () {
  var sock = null;
  var backoff = 500;          // ms, doublé à chaque échec, plafonné
  var BACKOFF_MAX = 8000;
  var closedByUs = false;
  var pingTimer = null;
  var listeners = { open: [], message: [], close: [] };

  function banner(show) {
    var b = document.getElementById('banner');
    if (b) { b.classList.toggle('hidden', !show); }
  }

  function emit(kind, arg) {
    var fns = listeners[kind] || [];
    for (var i = 0; i < fns.length; i++) {
      try { fns[i](arg); } catch (e) { console.error('Conduite.ws listener', e); }
    }
  }

  function connect() {
    var proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
    try {
      sock = new WebSocket(proto + location.host + '/ws');
    } catch (e) {
      scheduleReconnect();
      return;
    }
    sock.onopen = function () {
      backoff = 500;
      banner(false);
      if (pingTimer) { clearInterval(pingTimer); }
      pingTimer = setInterval(function () { send({ type: 'ping' }); }, 10000);
      emit('open');
    };
    sock.onmessage = function (e) {
      var msg = null;
      try { msg = JSON.parse(e.data); } catch (err) { return; }
      if (msg && typeof msg === 'object') { emit('message', msg); }
    };
    sock.onclose = function () {
      if (pingTimer) { clearInterval(pingTimer); pingTimer = null; }
      emit('close');
      if (!closedByUs) { scheduleReconnect(); }
      closedByUs = false;
    };
    sock.onerror = function () {
      try { sock.close(); } catch (e) { /* déjà fermé */ }
    };
  }

  function scheduleReconnect() {
    banner(true);
    setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, BACKOFF_MAX);
  }

  /** Envoie un objet JSON. Retourne false si la socket n'est pas prête. */
  function send(obj) {
    if (sock && sock.readyState === WebSocket.OPEN) {
      try { sock.send(JSON.stringify(obj)); return true; } catch (e) { return false; }
    }
    return false;
  }

  /** Force une reconnexion (ex. après chargement d'un nouveau show). */
  function reconnect() {
    if (sock && sock.readyState === WebSocket.OPEN) {
      closedByUs = false; // la fermeture déclenchera la reconnexion auto
      try { sock.close(); } catch (e) { scheduleReconnect(); }
    } else {
      connect();
    }
  }

  function on(kind, fn) {
    if (listeners[kind]) { listeners[kind].push(fn); }
  }

  return { connect: connect, send: send, on: on, reconnect: reconnect };
})();
