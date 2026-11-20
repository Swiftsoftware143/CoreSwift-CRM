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
    var css = document.createElement('style');
    css.textContent =
      '.csbk-fab{position:fixed;right:20px;bottom:20px;z-index:2147483000;border:0;border-radius:999px;' +
      'padding:14px 20px;font:600 15px/1 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;' +
      'color:#fff;cursor:pointer;box-shadow:0 10px 28px rgba(15,23,42,.28);background:' + ACCENT + ';' +
      'transition:transform .14s ease,box-shadow .18s ease}' +
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
      '@media (max-width:600px){.csbk-box{height:94vh;border-radius:14px}.csbk-fab{right:14px;bottom:14px}}';
    document.head.appendChild(css);

    var fab = document.createElement('button');
    fab.className = 'csbk-fab';
    fab.type = 'button';
    fab.textContent = LABEL;

    var ov = document.createElement('div');
    ov.className = 'csbk-ov';
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

    function open() { ov.classList.add('open'); document.documentElement.style.overflow = 'hidden'; }
    function close() { ov.classList.remove('open'); document.documentElement.style.overflow = ''; }
    fab.addEventListener('click', open);
    x.addEventListener('click', close);
    ov.addEventListener('click', function (e) { if (e.target === ov) close(); });
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') close(); });

    // the booking page asks to be closed once the customer finishes
    window.addEventListener('message', function (e) {
      if (String(e.data).indexOf('coreswift-booking-close') !== -1) close();
    });
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
