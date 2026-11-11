/*
 * CoreSwift — the Inbox tab of the CRM console.
 *
 * WHAT THIS SCREEN IS (kanban B204 / B205 / B211, David 2026-11-11)
 *
 * Two questions, both answered from the account's own records:
 *
 *   1. WHICH INBOXES DO I HAVE, and how do I open one? A list of the account's mailboxes, each
 *      row with a LOGIN button on the right, plus a "Create new inbox" button. LOGIN opens the mail
 *      client on its own host in a new window — the inbox behaves like separate software with its
 *      own front door (B211). A mailbox whose domain is still waiting for its mail records says so
 *      and its LOGIN is refused: never show a working login on an inbox that cannot receive.
 *
 *   2. WHAT MAIL HAS ARRIVED? Every message that was received, tagged with the address it came to
 *      and whether it ALSO became a ticket. This is the fix for the defect that made info@ mail
 *      invisible: the screen used to read TICKETS, so a message that did not create one was stored
 *      nowhere a human could see. It reads /private-email/messages now — the messages themselves.
 *
 * Plan ceilings are answered by the server with a plain sentence (402); this screen shows that
 * sentence as an upgrade notice and never a dead button.
 */
(function () {
  'use strict';

  // The mail client's own front door (B211). One host serves every account: same login, same data,
  // opened in a NEW window so the CRM stays where it was.
  var MAIL_HOST = 'https://inbox.coreswiftcrm.com';

  var S = {
    boxes: [], domains: [], messages: [], counts: {}, addresses: [],
    miscTotal: 0, view: 'inbox', address: 'all', sel: null, err: null
  };

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function fmt(d) {
    if (!d) return '';
    try {
      var t = new Date(d), now = new Date();
      return t.toDateString() === now.toDateString()
        ? t.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
        : t.toLocaleDateString([], { month: 'short', day: 'numeric' });
    } catch (e) { return String(d); }
  }
  function fmtBytes(n) {
    n = Number(n || 0);
    if (n >= 1073741824) return (n / 1073741824).toFixed(1) + ' GB';
    if (n >= 1048576) return (n / 1048576).toFixed(1) + ' MB';
    if (n >= 1024) return (n / 1024).toFixed(0) + ' KB';
    return n + ' B';
  }
  // The console declares `const API = {...}` at top level: a const does NOT become a window
  // property, so `window.API` is undefined while the bare name resolves. Reading the bare name
  // (with a window fallback for a build that does attach it) is what makes the tab work at all.
  function api() { return (typeof API !== 'undefined') ? API : window.API; }
  function fail(e) { return (e && (e.message || e.error)) || String(e); }
  function toastMsg(m, t) { if (window.toast) window.toast(m, t || 'info'); }

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Render
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  function inboxesHtml() {
    if (!S.boxes.length && S.domains.length && !S.domains.some(function (d) { return d.verified; })) {
      // Readiness rule (B211): an inbox cannot be CREATED until the domain's mail records are live.
      // Existing inboxes are never hidden behind this notice — a working address that receives mail
      // must stay visible and loggable even while another domain on the account is still waiting.
      return '<div class="empty-state" style="padding:16px;text-align:left">'
        + '<h4 style="margin:0 0 6px">Waiting for your mail records</h4>'
        + '<p style="margin:0 0 8px">Your domain <strong>' + esc(S.domains[0].domain) + '</strong> is not receiving mail yet, so an inbox on it would not work. '
        + 'Add the records at the company where your domain lives, wait until they show <strong>Active</strong>, then press <strong>Check again</strong>.</p>'
        + '<button class="btn btn-secondary btn-sm" onclick="peCheckDomain(\'' + esc(S.domains[0].id) + '\')">Check again</button>'
        + '<span id="peDomainMsg" style="margin-left:8px;opacity:.8"></span></div>';
    }
    if (!S.boxes.length) {
      return '<div class="empty-state" style="padding:16px;text-align:left">'
        + '<h4 style="margin:0 0 6px">No inboxes yet</h4>'
        + '<p style="margin:0 0 8px">An inbox is an address people can write to — <strong>info@</strong>, <strong>support@</strong>, or your own name. '
        + 'You need a domain that is already receiving mail before one can be created.</p>'
        + '<button class="btn btn-primary btn-sm" onclick="peCreateInbox()">Create your first inbox</button></div>';
    }
    var rows = S.boxes.map(function (b) {
      var dom = S.domains.filter(function (d) { return d.id === b.domain_id; })[0] || {};
      var count = (S.addresses.filter(function (a) { return a.address === b.email_address; })[0] || {}).total || 0;
      // An address that has actually RECEIVED mail is working, whatever a stale `verified` flag says
      // — the flag is Mailgun's word at the last check, and mail in the inbox is proof.
      var ready = !!dom.verified || count > 0;
      return '<tr>'
        + '<td style="padding:8px"><strong>' + esc(b.email_address) + '</strong>'
        + (b.creates_ticket ? ' <span style="background:#e0f2fe;color:#075985;padding:1px 6px;border-radius:8px;font-size:11px">tickets</span>' : '')
        + '</td>'
        + '<td style="padding:8px;opacity:.75">' + (ready ? 'Receiving' : 'Waiting for mail records') + '</td>'
        + '<td style="padding:8px;opacity:.75">' + count + '</td>'
        + '<td style="padding:8px;text-align:right;white-space:nowrap">'
        + '<label style="font-size:11px;opacity:.75;margin-right:10px;cursor:pointer"><input type="checkbox" '
        + (b.creates_ticket ? 'checked ' : '') + 'onchange="peToggleTicket(\'' + esc(b.id) + '\',this.checked)"> also a ticket</label>'
        + (ready
          ? '<button class="btn btn-primary btn-sm" onclick="peLogin(\'' + esc(b.email_address) + '\')">LOG IN</button>'
          : '<button class="btn btn-secondary btn-sm" disabled title="This address cannot receive mail yet">LOG IN</button>')
        + ' <button class="btn btn-ghost btn-xs" title="Delete this inbox" onclick="peDeleteBox(\'' + esc(b.id) + '\',\'' + esc(b.email_address) + '\')">Delete</button>'
        + '</td></tr>';
    }).join('');
    return '<table style="width:100%;border-collapse:collapse"><thead><tr style="text-align:left;opacity:.7">'
      + '<th style="padding:6px 8px">Inbox</th><th style="padding:6px 8px">Status</th><th style="padding:6px 8px">Messages</th><th></th>'
      + '</tr></thead><tbody>' + rows + '</tbody></table>';
  }

  function addressFilterHtml() {
    var opts = ['<option value="all"' + (S.address === 'all' ? ' selected' : '') + '>All addresses (' + (S.counts.inbox || 0) + ')</option>'];
    (S.addresses || []).forEach(function (a) {
      opts.push('<option value="' + esc(a.address) + '"' + (S.address === a.address ? ' selected' : '') + '>' + esc(a.address) + ' (' + a.total + ')</option>');
    });
    if (S.miscTotal > 0) {
      opts.push('<option value="misc"' + (S.address === 'misc' ? ' selected' : '') + '>Everything else / MISC (' + S.miscTotal + ')</option>');
    }
    return '<select onchange="peSetAddress(this.value)" style="margin-right:8px">' + opts.join('') + '</select>';
  }

  function messagesHtml() {
    var m = S.messages || [];
    if (!m.length) {
      return '<div class="empty-state" style="padding:20px"><h4 style="margin:0 0 6px">No mail here yet</h4>'
        + '<p style="margin:0">Every message that arrives at one of your addresses appears here — including mail to addresses that do not create tickets.</p></div>';
    }
    var rows = m.map(function (x) {
      return '<tr data-id="' + esc(x.id) + '" class="pe-row" style="cursor:pointer;border-bottom:1px solid rgba(128,128,128,.2);'
        + (x.is_read ? '' : 'font-weight:600;') + '">'
        + '<td style="padding:8px;width:26px" onclick="event.stopPropagation();peStar(\'' + esc(x.id) + '\',' + (!x.is_starred) + ')">'
        + (x.is_starred ? '★' : '<span style="opacity:.3">☆</span>') + '</td>'
        + '<td style="padding:8px">' + esc(x.from_name || x.from_address || 'Unknown sender') + '</td>'
        + '<td style="padding:8px">' + esc(x.subject || '(no subject)')
        + (x.is_ticket ? ' <span style="background:#dcfce7;color:#166534;padding:1px 6px;border-radius:8px;font-size:11px">TICKET</span>' : '')
        + '</td>'
        + '<td style="padding:8px;opacity:.7;font-size:12px">' + esc(x.address || '') + '</td>'
        + '<td style="padding:8px;opacity:.7;white-space:nowrap">' + esc(fmt(x.received_at)) + '</td>'
        + '</tr>';
    }).join('');
    return '<table style="width:100%;border-collapse:collapse"><thead><tr style="text-align:left;opacity:.7;font-size:12px">'
      + '<th></th><th style="padding:6px 8px">From</th><th style="padding:6px 8px">Subject</th><th style="padding:6px 8px">Arrived on</th><th style="padding:6px 8px">Received</th>'
      + '</tr></thead><tbody>' + rows + '</tbody></table>';
  }

  function render() {
    var c = S.counts || {};
    return '<div>'
      + '<div style="display:flex;justify-content:space-between;align-items:center;margin:0 0 12px">'
      + '<div><h3 style="margin:0">Mail</h3><span style="opacity:.7;font-size:13px">'
      + (c.unread || 0) + ' unread · ' + (c.tickets || 0) + ' turned into tickets · ' + fmtBytes(c.used_bytes) + ' of ' + fmtBytes(c.limit_bytes) + ' used'
      + '</span></div>'
      + '<button class="btn btn-primary btn-sm" onclick="peCreateInbox()">+ Create new inbox</button></div>'
      + (S.err ? '<div style="padding:8px;margin-bottom:8px;border-radius:6px;background:rgba(220,80,80,.15)">' + esc(S.err) + '</div>' : '')
      + '<div style="padding:14px;border:1px solid rgba(128,128,128,.3);border-radius:8px;margin-bottom:16px">'
      + '<h4 style="margin:0 0 10px">Your inboxes</h4>' + inboxesHtml() + '</div>'
      + '<div style="padding:14px;border:1px solid rgba(128,128,128,.3);border-radius:8px">'
      + '<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px">'
      + '<h4 style="margin:0">Recent mail</h4><div>' + addressFilterHtml()
      + '<button class="btn btn-ghost btn-xs" onclick="peLoad()">Refresh</button></div></div>'
      + '<div style="font-size:12px;opacity:.7;margin-bottom:8px">Mail is kept while you have storage. Nothing is deleted on a clock.</div>'
      + messagesHtml() + '</div>'
      + '<div id="peDetail"></div>'
      + '</div>';
  }

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Loading
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  async function load() {
    var el = document.getElementById('peWrap');
    if (!el) return;
    try {
      var results = await Promise.all([
        api().get('/private-email/boxes').catch(function () { return []; }),
        api().get('/private-email/domains').catch(function () { return []; }),
        api().get('/private-email/messages?folder=' + encodeURIComponent(S.view) + '&address=' + encodeURIComponent(S.address) + '&limit=50'),
        api().get('/private-email/messages/counts').catch(function () { return {}; }),
        api().get('/private-email/addresses').catch(function () { return {}; })
      ]);
      S.boxes = Array.isArray(results[0]) ? results[0] : (results[0].boxes || []);
      S.domains = Array.isArray(results[1]) ? results[1] : (results[1].domains || []);
      S.messages = (results[2] && results[2].messages) || [];
      S.counts = results[3] || {};
      S.addresses = (results[4] && results[4].addresses) || [];
      S.miscTotal = (results[4] && results[4].misc && results[4].misc.total) || 0;
      S.err = null;
    } catch (e) {
      S.err = fail(e);
      S.messages = [];
    }
    el.innerHTML = render();
    Array.prototype.forEach.call(document.querySelectorAll('.pe-row'), function (r) {
      r.onclick = function () { window.peOpenMessage(r.getAttribute('data-id')); };
    });
  }

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Inbox actions
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  window.peLogin = function (address) {
    var url = MAIL_HOST + '/?address=' + encodeURIComponent(address);
    var w = window.open(url, '_blank', 'noopener');
    if (!w) toastMsg('Allow pop-ups for this site to open your mail client', 'error');
  };

  window.peCheckDomain = async function (domainId) {
    var m = document.getElementById('peDomainMsg');
    if (m) m.textContent = 'Checking…';
    try {
      var r = await api().post('/private-email/domains/' + domainId + '/verify', {});
      if (m) m.textContent = r.verified ? 'Active — you can create an inbox now' : ('Still ' + (r.mailgun_state || 'waiting') + ' — the records have not gone live yet');
      if (r.verified) load();
    } catch (e) {
      if (m) m.textContent = fail(e);
    }
  };

  window.peCreateInbox = function () {
    var verified = S.domains.filter(function (d) { return d.verified; });
    if (!verified.length) {
      var waiting = S.domains.map(function (d) {
        return '<li>' + esc(d.domain) + ' — <button class="btn btn-ghost btn-xs" onclick="peCheckDomain(\'' + esc(d.id) + '\')">Check again</button></li>';
      }).join('');
      window.showModal('Not ready for an inbox yet',
        '<p>An inbox needs a domain that is already receiving mail, so nothing here can receive yet.</p>'
        + (waiting ? '<p>Domains on this account:</p><ul>' + waiting + '</ul>' : '<p>Add your domain first, then come back.</p>')
        + '<p style="opacity:.75">When the domain shows <strong>Active</strong>, this button will create the address for you.</p>',
        '<button class="btn btn-secondary" onclick="closeModal()">Close</button>');
      return;
    }
    var opts = verified.map(function (d) { return '<option value="' + esc(d.id) + '">' + esc(d.domain) + '</option>'; }).join('');
    window.showModal('Create a new inbox',
      '<div class="form-group"><label>Which domain?</label><select id="peNewDomain">' + opts + '</select></div>'
      + '<div class="form-group"><label>The address</label>'
      + '<div style="display:flex;align-items:center;gap:6px"><input id="peNewLocal" placeholder="info" style="flex:1"><span style="opacity:.75">@ your domain</span></div>'
      + '<p style="font-size:12px;opacity:.7;margin:6px 0 0">Common ones: <strong>info@</strong> for enquiries, <strong>support@</strong> for customers who need help, and your own name.</p></div>'
      + '<div id="peNewMsg" style="font-size:13px"></div>',
      '<button class="btn btn-secondary" onclick="closeModal()">Cancel</button>'
      + '<button class="btn btn-primary" onclick="peSubmitInbox()">Create inbox</button>');
  };

  window.peSubmitInbox = async function () {
    var d = document.getElementById('peNewDomain');
    var l = document.getElementById('peNewLocal');
    var m = document.getElementById('peNewMsg');
    var local = ((l && l.value) || '').trim().replace(/@.*$/, '');
    if (!local) { if (m) m.textContent = 'Enter the part before the @ sign.'; return; }
    if (m) m.textContent = 'Creating…';
    try {
      await api().post('/private-email/boxes', { domain_id: d.value, local_part: local });
      window.closeModal();
      toastMsg('Inbox created', 'success');
      load();
    } catch (e) {
      // A plan ceiling and a refusal both carry the server's own sentence; show it where the user
      // is looking rather than as a disappearing toast.
      if (m) m.textContent = fail(e); else toastMsg(fail(e), 'error');
    }
  };

  window.peToggleTicket = async function (boxId, want) {
    try {
      await api().patch('/private-email/boxes/' + boxId, { creates_ticket: !!want });
      toastMsg(want ? 'Mail to this address will also open a ticket' : 'Mail to this address lands in the inbox only', 'success');
    } catch (e) {
      toastMsg(fail(e), 'error');
      load();
    }
  };

  window.peDeleteBox = function (boxId, address) {
    if (!window.confirm('Delete the inbox ' + address + '?\n\nMail that already arrived stays in your inbox — only the address goes away.')) return;
    api().del('/private-email/boxes/' + boxId)
      .then(function () { toastMsg('Inbox deleted', 'success'); load(); })
      .catch(function (e) { toastMsg(fail(e), 'error'); });
  };

  window.peSetAddress = function (a) { S.address = a; load(); };

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Message actions
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  window.peStar = async function (id, want) {
    try {
      await api().patch('/private-email/messages/' + id, { is_starred: !!want });
      load();
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  window.peOpenMessage = async function (id) {
    var d = document.getElementById('peDetail');
    if (!d) return;
    d.innerHTML = '<div style="padding:12px;opacity:.7">Opening…</div>';
    try {
      var m = await api().get('/private-email/messages/' + id);
      S.sel = m;
      var fromOpts = S.boxes.map(function (b) {
        return '<option value="' + esc(b.email_address) + '"' + (m.address === b.email_address ? ' selected' : '') + '>' + esc(b.email_address) + '</option>';
      }).join('') || '<option value="">(no inbox yet)</option>';
      d.innerHTML = '<div style="margin-top:16px;padding:14px;border:1px solid rgba(128,128,128,.35);border-radius:8px">'
        + '<div style="display:flex;justify-content:space-between;gap:10px;align-items:flex-start">'
        + '<div><h4 style="margin:0 0 4px">' + esc(m.subject || '(no subject)') + '</h4>'
        + '<div style="opacity:.75;font-size:13px">From ' + esc(m.from_name || m.from_address)
        + ' · to ' + esc(m.address || '') + ' · ' + esc(fmt(m.received_at)) + '</div></div>'
        + '<div style="white-space:nowrap">'
        + '<button class="btn btn-ghost btn-xs" onclick="peStar(\'' + esc(m.id) + '\',' + (!m.is_starred) + ')">' + (m.is_starred ? '★ Starred' : '☆ Star') + '</button> '
        + '<button class="btn btn-ghost btn-xs" onclick="peArchive(\'' + esc(m.id) + '\')">Archive</button> '
        + '<button class="btn btn-ghost btn-xs" onclick="peDeleteMsg(\'' + esc(m.id) + '\',\'' + esc(m.folder || '') + '\')">Delete</button>'
        + '</div></div>'
        + (m.is_ticket
          ? '<div style="margin:10px 0;padding:6px 10px;border-radius:6px;background:#dcfce7;color:#166534;font-size:13px">In the work queue as a ticket.</div>'
          : '<div style="margin:10px 0;padding:6px 10px;border-radius:6px;background:#fef3c7;color:#92400e;font-size:13px;display:flex;justify-content:space-between;align-items:center">'
            + '<span>In the inbox only — nobody is working this yet.</span>'
            + '<button class="btn btn-primary btn-xs" onclick="peMakeTicket(\'' + esc(m.id) + '\')">Turn into a ticket</button></div>')
        + '<div style="white-space:pre-wrap;margin:12px 0;max-height:420px;overflow:auto">' + esc(m.body_plain || '(no text)') + '</div>'
        + '<label style="display:block;font-size:12px;opacity:.75;margin-bottom:4px">Reply as</label>'
        + '<select id="peFrom" style="margin-bottom:8px">' + fromOpts + '</select>'
        + '<textarea id="peText" rows="4" placeholder="Write your reply…" style="width:100%;margin-bottom:8px"></textarea>'
        + '<button class="btn btn-primary" id="peSend">Send reply</button> <span id="peMsg" style="margin-left:8px"></span>'
        + '</div>';
      document.getElementById('peSend').onclick = function () { window.peReply(m.id); };
      d.scrollIntoView({ behavior: 'smooth', block: 'start' });
    } catch (e) {
      d.innerHTML = '<div style="padding:12px;color:#b91c1c">' + esc(fail(e)) + '</div>';
    }
  };

  window.peReply = async function (id) {
    var from = (document.getElementById('peFrom') || {}).value || '';
    var body = ((document.getElementById('peText') || {}).value || '').trim();
    var m = document.getElementById('peMsg');
    if (!from) { if (m) m.textContent = 'Create an inbox first.'; return; }
    if (!body) { if (m) m.textContent = 'Write a reply first.'; return; }
    if (m) m.textContent = 'Sending…';
    try {
      await api().post('/private-email/messages/' + id + '/reply', { from_address: from, body: body });
      if (m) m.textContent = 'Sent';
      var t = document.getElementById('peText'); if (t) t.value = '';
    } catch (e) { if (m) m.textContent = fail(e); }
  };

  window.peMakeTicket = async function (id) {
    try {
      await api().post('/private-email/messages/' + id + '/ticket', {});
      toastMsg('Ticket created', 'success');
      window.peOpenMessage(id);
      load();
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  window.peArchive = async function (id) {
    try {
      await api().patch('/private-email/messages/' + id, { folder: 'archive' });
      var d = document.getElementById('peDetail'); if (d) d.innerHTML = '';
      toastMsg('Archived', 'success');
      load();
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  window.peDeleteMsg = function (id, folder) {
    var forever = folder === 'spam' || folder === 'trash';
    var msg = forever
      ? 'This message will be destroyed for good. Delete it?'
      : 'Move this message to Trash? It can be restored from there.';
    if (!window.confirm(msg)) return;
    api().del('/private-email/messages/' + id)
      .then(function () {
        var d = document.getElementById('peDetail'); if (d) d.innerHTML = '';
        toastMsg(forever ? 'Deleted' : 'Moved to Trash', 'success');
        load();
      })
      .catch(function (e) { toastMsg(fail(e), 'error'); });
  };

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Tab registration
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  window.renderPeInbox = function () { return '<div id="peWrap"><p>Loading your mail…</p></div>'; };
  window.loadPeInbox = load;
  window.peLoad = load;
  if (typeof TAB_RENDERERS !== 'undefined') { TAB_RENDERERS['inbox'] = window.renderPeInbox; }
  if (typeof TAB_LOADERS !== 'undefined') { TAB_LOADERS['inbox'] = window.loadPeInbox; }
})();
