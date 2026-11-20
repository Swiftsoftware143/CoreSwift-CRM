/*!
 * CoreSwift booking — embeds one calendar on any website.
 *
 * Two shapes, one file:
 *
 *   INLINE   the calendar sits in the page, and resizes itself as the customer filters or picks.
 *   POPUP    a button floats in the corner of the site; pressing it opens the calendar in a modal
 *            over the page, so a visitor never leaves what they were reading.
 *
 * Usage (paste on the customer's site):
 *
 *   <div id="my-bookings"></div>
 *   <script src="https://booking.coreswiftcrm.com/embed.js"
 *           data-calendar="consultations"
 *           data-mode="inline"
 *           data-target="my-bookings"></script>
 *
 *   <script src="https://booking.coreswiftcrm.com/embed.js"
 *           data-calendar="consultations"
 *           data-mode="popup"
 *           data-label="Book a call"></script>
 *
 *   INSIDE YOUR OWN MODAL / POP-UP / PANEL — `data-fill="container"` makes the calendar fill the box
 *   you put it in instead of reporting its own content height. Whatever sizes the container (your
 *   modal, a CSS grid cell, a fixed-height section) decides the size, and the calendar scrolls inside
 *   it. Nothing to measure, no gaps, no cut-off:
 *
 *   <div id="booking-modal-body" style="height:min(88vh,900px)">   <!-- your popup's body -->
 *     <div id="my-bookings" style="height:100%"></div>
 *   </div>
 *   <script src="https://booking.coreswiftcrm.com/embed.js"
 *           data-calendar="consultations"
 *           data-mode="inline"
 *           data-fill="container"
 *           data-min-height="420"
 *           data-target="my-bookings"></script>
 *
 * WHY AN IFRAME AND NOT AN INJECTED FORM: the booking page keeps its own origin, so the customer's
 * site never touches the customer's card details or their booking data — and a change here can never
 * break their page.
 */
(function () {
  'use strict';

  var script = document.currentScript ||
    (function () { var all = document.getElementsByTagName('script'); return all[all.length - 1]; })();

  var CAL = (script.getAttribute('data-calendar') || '').trim();
  var MODE = (script.getAttribute('data-mode') || 'inline').trim().toLowerCase();
  var TARGET = (script.getAttribute('data-target') || '').trim();
  var LABEL = (script.getAttribute('data-label') || 'Book now').trim();
  var HOST = (script.getAttribute('data-host') || 'https://booking.coreswiftcrm.com').replace(/\/+$/, '');
  var ACCENT = (script.getAttribute('data-accent') || '#6366f1').trim();
  // 'container' = fill whatever box the host page gives us, instead of reporting our own height.
  // This is what makes the calendar look right inside somebody else's pop-up: their modal owns the
  // size, we simply use all of it, and our content scrolls rather than pushing their layout around.
  var FILL = (script.getAttribute('data-fill') || '').trim().toLowerCase();
  var FILLS = FILL === 'container' || FILL === '1' || FILL === 'true';
  var MIN_H = parseInt(script.getAttribute('data-min-height') || '420', 10);
  if (!isFinite(MIN_H) || MIN_H < 200) MIN_H = 420;

  if (!CAL) {
    console.warn('[coreswift-booking] data-calendar is required');
    return;
  }

  var SRC = HOST + '/book/' + encodeURIComponent(CAL) + '?embed=1';

  function frame(id) {
    var f = document.createElement('iframe');
    f.src = SRC;
    f.id = id;
    f.title = 'Booking';
    f.setAttribute('loading', 'lazy');
    f.setAttribute('allowtransparency', 'true');
    f.style.cssText = 'width:100%;border:0;display:block;background:transparent;' +
      (MODE !== 'inline' ? 'height:100%;border-radius:0;'
        : FILLS ? 'height:100%;min-height:' + MIN_H + 'px;'
        : 'min-height:520px;');
    return f;
  }

  if (MODE === 'popup') {
    // ── HOW AND WHEN THE POP-UP APPEARS ───────────────────────────────────────────────────────────
    // Everything a customer's marketing team would normally ask for, because a booking prompt that
    // appears the wrong way on the wrong site does more harm than no prompt at all.
    //
    //   data-position   bottom-right (default) | bottom-left | top-right | top-left
    //   data-trigger    click (default) | delay | exit | delay-exit
    //   data-delay      seconds before a `delay` trigger fires (default 10)
    //   data-once       session (default) | day | week | ever | off   -- how often AUTO-opening happens
    //   data-hide-mobile "true" to keep the button and the auto-open off small screens
    //
    // Rules that are not optional, because they are what makes this usable rather than annoying:
    //   * a CLICK always works — it is the visitor asking;
    //   * an auto-open (delay / exit) happens at most once per `data-once` window, tracked per calendar;
    //   * someone who has already completed a booking is never auto-prompted again;
    //   * exit-intent is not fired on touch screens, where there is no such gesture;
    //   * pressing the button, Escape, the ×, or clicking the backdrop closes it, and focus returns to
    //     where it was — the pop-up is announced as a dialog, not an anonymous box.
    var POSITION = (script.getAttribute('data-position') || 'bottom-right').trim().toLowerCase();
    var TRIGGER = (script.getAttribute('data-trigger') || 'click').trim().toLowerCase();
    var DELAY = parseInt(script.getAttribute('data-delay') || '10', 10);
    if (!isFinite(DELAY) || DELAY < 1) DELAY = 10;
    if (DELAY > 120) DELAY = 120;
    var ONCE = (script.getAttribute('data-once') || 'session').trim().toLowerCase();
    var HIDE_MOBILE = (script.getAttribute('data-hide-mobile') || '').trim().toLowerCase() === 'true';

    var POS = { 'bottom-right': 'br', 'bottom-left': 'bl', 'top-right': 'tr', 'top-left': 'tl' }[POSITION] || 'br';
    var WANTS_CLICK = TRIGGER === 'click' || TRIGGER === 'delay' || TRIGGER === 'exit' || TRIGGER === 'delay-exit';
    var WANTS_DELAY = TRIGGER === 'delay' || TRIGGER === 'delay-exit';
    var WANTS_EXIT = TRIGGER === 'exit' || TRIGGER === 'delay-exit';
    var IS_SMALL = window.matchMedia && window.matchMedia('(max-width: 600px)').matches;

    // Frequency cap. sessionStorage for a session, localStorage for anything longer, and nothing at
    // all when the customer explicitly wants the prompt on every visit.
    var STORE = (function () {
      try {
        return ONCE === 'session' ? window.sessionStorage : window.localStorage;
      } catch (e) {
        return null; // storage blocked (private mode): fall back to sessionStore-less behaviour below
      }
    })();
    var KEY = 'csbk:' + CAL + ':auto';
    var BOOKED_KEY = 'csbk:' + CAL + ':booked';
    var WINDOW_MS = { session: -1, day: 86400000, week: 604800000, ever: Infinity, off: 0 }[ONCE];
    if (typeof WINDOW_MS === 'undefined') WINDOW_MS = -1;

    function alreadyAutoOpened() {
      if (ONCE === 'off') return false;
      try {
        if (ONCE === 'session') return STORE && STORE.getItem(KEY) === '1';
        var at = STORE && parseInt(STORE.getItem(KEY) || '0', 10);
        if (!at) return false;
        if (WINDOW_MS === Infinity) return true;
        return (Date.now() - at) < WINDOW_MS;
      } catch (e) {
        return false;
      }
    }
    function markAutoOpened() {
      try {
        if (STORE) STORE.setItem(KEY, ONCE === 'session' ? '1' : String(Date.now()));
      } catch (e) {}
    }
    function alreadyBooked() {
      try {
        return !!(window.localStorage && window.localStorage.getItem(BOOKED_KEY) === '1');
      } catch (e) {
        return false;
      }
    }

    if (HIDE_MOBILE && IS_SMALL) return; // nothing rendered at all on a small screen

    var css = document.createElement('style');
    css.textContent =
      '.csbk-fab{position:fixed;z-index:2147483000;border:0;border-radius:999px;' +
      'padding:14px 20px;font:600 15px/1 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;' +
      'color:#fff;cursor:pointer;box-shadow:0 10px 28px rgba(15,23,42,.28);background:' + ACCENT + ';' +
      'transition:transform .14s ease,box-shadow .18s ease}' +
      '.csbk-fab.br{right:20px;bottom:20px}.csbk-fab.bl{left:20px;bottom:20px}' +
      '.csbk-fab.tr{right:20px;top:20px}.csbk-fab.tl{left:20px;top:20px}' +
      '.csbk-fab:hover{transform:translateY(-2px);box-shadow:0 14px 34px rgba(15,23,42,.34)}' +
      '.csbk-fab:active{transform:scale(.97)}' +
      '.csbk-ov{position:fixed;inset:0;z-index:2147483001;background:rgba(15,23,42,.55);display:none;' +
      'align-items:center;justify-content:center;padding:18px;backdrop-filter:blur(2px)}' +
      '.csbk-ov.open{display:flex}' +
      '.csbk-box{position:relative;width:min(1080px,100%);height:min(88vh,900px);background:#fff;' +
      'border-radius:18px;overflow:hidden;box-shadow:0 30px 80px rgba(15,23,42,.4)}' +
      '.csbk-x{position:absolute;top:10px;right:12px;z-index:2;border:0;background:#fff;color:#0f172a;' +
      'width:34px;height:34px;border-radius:50%;font-size:18px;line-height:1;cursor:pointer;' +
      'box-shadow:0 2px 8px rgba(15,23,42,.18)}' +
      '@media (max-width:600px){.csbk-box{height:94vh;border-radius:14px}' +
      '.csbk-fab.br,.csbk-fab.bl{right:14px;left:auto;bottom:14px}.csbk-fab.bl{left:14px;right:auto}}';
    document.head.appendChild(css);

    var fab = document.createElement('button');
    fab.className = 'csbk-fab ' + POS;
    fab.type = 'button';
    fab.textContent = LABEL;
    fab.setAttribute('aria-haspopup', 'dialog');

    var ov = document.createElement('div');
    ov.className = 'csbk-ov';
    ov.setAttribute('role', 'dialog');
    ov.setAttribute('aria-modal', 'true');
    ov.setAttribute('aria-label', LABEL);
    var box = document.createElement('div');
    box.className = 'csbk-box';
    var x = document.createElement('button');
    x.className = 'csbk-x';
    x.type = 'button';
    x.setAttribute('aria-label', 'Close');
    x.innerHTML = '&times;';

    box.appendChild(x);
    box.appendChild(frame('csbk-frame-popup'));
    ov.appendChild(box);
    document.body.appendChild(fab);
    document.body.appendChild(ov);

    var lastFocus = null;
    function open(byUser) {
      lastFocus = document.activeElement;
      ov.classList.add('open');
      document.documentElement.style.overflow = 'hidden';
      try { x.focus(); } catch (e) {}
      // Only an AUTOMATIC open spends the visitor's allowance; a click is them asking.
      if (!byUser) markAutoOpened();
    }
    function close() {
      ov.classList.remove('open');
      document.documentElement.style.overflow = '';
      try { if (lastFocus && lastFocus.focus) lastFocus.focus(); } catch (e) {}
    }
    fab.addEventListener('click', function () { open(true); });
    x.addEventListener('click', close);
    ov.addEventListener('click', function (e) { if (e.target === ov) close(); });
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') close(); });

    // The calendar tells us when a booking completes: remember it, close, and never auto-prompt again.
    window.addEventListener('message', function (e) {
      var d = e.data;
      var s = typeof d === 'string' ? d : '';
      if (s.indexOf('coreswift-booking-close') !== -1) { close(); return; }
      if ((d && d.type === 'coreswift-booking-done') || s.indexOf('coreswift-booking-done') !== -1) {
        try { window.localStorage.setItem(BOOKED_KEY, '1'); } catch (err) {}
        close();
      }
    });

    // The button is always available: it is the visitor's own way in, and hiding it would take away
    // the only affordance an auto-open cannot replace. `data-hide-mobile` is the switch for that, and
    // it removes the whole widget rather than leaving a button beside a pop-up it cannot open.
    var _alwaysClickable = WANTS_CLICK;

    // ── the automatic arms, each capped and each respectful of a visitor who already booked ──────
    var canAutoOpen = !alreadyBooked() && !alreadyAutoOpened();
    if (canAutoOpen && WANTS_DELAY) {
      window.setTimeout(function () {
        if (!ov.classList.contains('open')) open(false);
      }, DELAY * 1000);
    }
    if (canAutoOpen && WANTS_EXIT) {
      // No exit-intent on touch: there is no pointer leaving the window, and a timer pretending to be
      // one is just a pop-up the visitor did not ask for.
      var touch = ('ontouchstart' in window) || (navigator.maxTouchPoints > 0);
      if (!touch) {
        document.addEventListener('mouseout', function (e) {
          if (e.relatedTarget || e.toElement) return;
          if ((e.clientY || 0) > 0) return;
          if (ov.classList.contains('open')) return;
          if (alreadyAutoOpened()) return;
          open(false);
        });
      }
    }
    return;
  }

  // ---- inline ------------------------------------------------------------------------------
  var host = TARGET ? document.getElementById(TARGET) : null;
  if (!host) {
    host = document.createElement('div');
    if (TARGET) {
      console.warn('[coreswift-booking] no element with id "' + TARGET + '", appending to the body');
    }
    script.parentNode.insertBefore(host, script.nextSibling);
  }
  if (FILLS) {
    // The host element must have a height for "100%" to mean anything. If the page already sized it
    // (a pop-up body, a grid cell), leave that alone; otherwise let it fill its own parent.
    host.style.display = host.style.display || 'block';
    if (!host.style.height) host.style.height = '100%';
    if (!host.style.minHeight) host.style.minHeight = MIN_H + 'px';
  }
  var inline = frame('csbk-frame-inline');
  host.appendChild(inline);

  // the page reports its own height so nothing is cut off when the customer filters or picks.
  // In fill mode the CONTAINER owns the height, so a report must not resize the frame — that is the
  // one thing that would make the calendar jump around inside somebody's modal.
  if (FILLS) return;
  window.addEventListener('message', function (e) {
    var data = e.data;
    if (!data || typeof data !== 'object') {
      if (typeof data === 'string' && data.indexOf('coreswift-booking-height:') === 0) {
        inline.style.height = data.split(':')[1] + 'px';
      }
      return;
    }
    if (data.type === 'coreswift-booking-height' && data.height) {
      // A long list of dates would otherwise make the host page enormous. Past 1400px the frame
      // stops growing and the calendar scrolls inside it, which keeps the customer's page readable.
      inline.style.height = Math.max(420, Math.min(1400, data.height)) + 'px';
    }
  });
})();
