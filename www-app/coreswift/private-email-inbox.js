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
    folders: { system: [], custom: [] },
    miscTotal: 0, view: 'inbox', address: 'all', sel: null, err: null,
    // The FREE mailbox's trial state (kanban t_2ef67217): read from the server, which computes it
    // from the same registry rows the send path enforces — so a countdown here can never promise
    // sending the send path would refuse.
    status: null,
    // First-run setup (BYOK): the verified-domain records checklist, why it could not be read, the
    // last refusal, and one busy flag shared by all three steps so a double click cannot add the
    // domain twice.
    records: null, recordsErr: null, setupErr: null, busy: false
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
    if (!S.boxes.length) return setupHtml();
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

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // First-run setup (David 2026-11-11 — BYOK from day one)
  //
  // There is NO CoreSwift-hosted trial address. The customer brings their own domain and their own
  // Mailgun account, and this checklist IS the onboarding: add the domain and the Mailgun key, add
  // the provider's records at their DNS host, then create the inbox. The 30-day free month starts
  // when the mailbox is created on a VERIFIED domain, so a slow DNS change never eats the trial.
  //
  // Nothing here renders an IP address: every record value is the customer's own provider's name or
  // one of our hostnames. The create button stays disabled until receiving AND sending proof verify,
  // so nobody can make an address that cannot receive mail.
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  function badgeFor(r) {
    if (r.optional && r.valid == null) return '<span style="background:#e5e7eb;color:#374151;padding:1px 7px;border-radius:8px;font-size:11px">Optional</span>';
    if (r.valid === true) return '<span style="background:#dcfce7;color:#166534;padding:1px 7px;border-radius:8px;font-size:11px">Active</span>';
    if (r.valid === false) return '<span style="background:#fef3c7;color:#92400e;padding:1px 7px;border-radius:8px;font-size:11px">Waiting</span>';
    return '<span style="background:#e0f2fe;color:#075985;padding:1px 7px;border-radius:8px;font-size:11px">Add this</span>';
  }

  function recordRowHtml(r) {
    return '<tr>'
      + '<td style="padding:6px 8px;white-space:nowrap"><strong>' + esc(r.type) + '</strong></td>'
      + '<td style="padding:6px 8px;font-family:monospace;font-size:12px;word-break:break-all">' + esc(r.name) + '</td>'
      + '<td style="padding:6px 8px;font-family:monospace;font-size:12px;word-break:break-all;max-width:420px">' + esc(r.value) + '</td>'
      + '<td style="padding:6px 8px;text-align:center">' + (r.priority == null ? '' : esc(String(r.priority))) + '</td>'
      + '<td style="padding:6px 8px;white-space:nowrap">' + badgeFor(r) + '</td>'
      + '<td style="padding:6px 8px;text-align:right"><button class="btn btn-ghost btn-xs" data-copy="'
      + esc(r.value) + '" onclick="peCopy(this)">Copy</button></td>'
      + '</tr>';
  }

  function recordsHtml(rec) {
    var out = '';
    var groups = rec.groups || [];
    groups.forEach(function (g) {
      var rows = (g.records || []).map(recordRowHtml).join('');
      if (!rows) return;
      out += '<h5 style="margin:14px 0 4px">' + esc(g.title) + '</h5>'
        + '<table style="width:100%;border-collapse:collapse;margin-bottom:4px"><thead><tr style="text-align:left;opacity:.6;font-size:11px">'
        + '<th style="padding:4px 8px">Type</th><th style="padding:4px 8px">Name</th><th style="padding:4px 8px">Value</th>'
        + '<th style="padding:4px 8px">Priority</th><th style="padding:4px 8px">Status</th><th></th>'
        + '</tr></thead><tbody>' + rows + '</tbody></table>';
    });
    return out;
  }

  function trapsHtml() {
    return '<div style="margin:12px 0;padding:10px 12px;border-left:3px solid #93c5fd;background:rgba(147,197,253,.12);font-size:13px">'
      + '<strong>If your domain is on Cloudflare, five things save you a support ticket:</strong><ul style="margin:6px 0 0 18px;padding:0">'
      + '<li>Every mail record must be <strong>DNS only (grey cloud)</strong> — the orange proxy breaks mail.</li>'
      + '<li><strong>MX priority</strong> is its own field; it is not part of the value.</li>'
      + '<li>The DKIM value is very long — add it as <strong>one</strong> record; Cloudflare splits long text itself.</li>'
      + '<li><strong>One SPF record per domain.</strong> If you already have one, merge <code>include:mailgun.org</code> into it — do not add a second.</li>'
      + '<li>Add the domain in <strong>your Mailgun account first</strong>, then paste these records, then press Check again.</li>'
      + '</ul></div>';
  }

  function setupHtml() {
    var d = (S.domains || [])[0];
    var rec = (d && S.records && String(S.records.domain_id) === String(d.id)) ? S.records : null;
    var busy = S.busy;
    var err = S.setupErr ? '<p style="margin:0 0 8px;color:#b45309">' + esc(S.setupErr) + '</p>' : '';

    // ── Step 1: the domain + the customer's own Mailgun key ──────────────────────────────────
    var step1;
    if (d) {
      step1 = '<div style="margin:4px 0 0"><strong>' + esc(d.domain) + '</strong> — '
        + (d.verified ? '<span style="color:#166534">verified</span>' : '<span style="color:#92400e">waiting for its records</span>')
        + '</div>';
    } else {
      step1 = '<p style="margin:6px 0 10px;font-size:13px;opacity:.85">You need two things you already control: a domain (from Cloudflare or your registrar) and a Mailgun account. Your mail runs through your own accounts — it never passes through ours.</p>'
        + '<div class="form-group"><label>Your domain</label><input id="peSetupDomain" placeholder="yourbusiness.com" autocomplete="off"></div>'
        + '<div class="form-group"><label>Mailgun region</label><select id="peSetupRegion"><option value="us">United States (api.mailgun.net)</option><option value="eu">Europe (api.eu.mailgun.net)</option></select></div>'
        + '<div class="form-group"><label>Mailgun API key</label><input id="peSetupKey" type="password" placeholder="key-…" autocomplete="off"><p style="font-size:12px;opacity:.7;margin:4px 0 0">In Mailgun: Settings → API keys. It is stored sealed and never shown again.</p></div>'
        + '<div class="form-group"><label>Mailgun webhook signing key <span style="opacity:.7">(so incoming mail can be verified)</span></label><input id="peSetupHook" type="password" placeholder="from Mailgun → Settings → Webhooks" autocomplete="off"></div>'
        + '<button class="btn btn-primary btn-sm" ' + (busy ? 'disabled' : '') + ' onclick="peSetupAddDomain()">' + (busy ? 'Adding…' : 'Add my domain') + '</button>'
        + '<p style="font-size:12px;opacity:.7;margin:8px 0 0">Nothing is charged and no inbox exists until your domain passes its check.</p>';
    }

    // ── Step 2: the records ──────────────────────────────────────────────────────────────────
    var step2;
    if (!d) {
      step2 = '<p style="margin:6px 0;opacity:.7">Your records appear here once the domain is added.</p>';
    } else if (rec) {
      step2 = recordsHtml(rec) + trapsHtml()
        + '<div style="display:flex;align-items:center;gap:10px;margin-top:6px">'
        + '<button class="btn btn-secondary btn-sm" ' + (busy ? 'disabled' : '') + ' onclick="peCheckAgain()">' + (busy ? 'Checking…' : 'Check again') + '</button>'
        + '<span id="peDomainMsg" style="opacity:.8;font-size:13px">'
        + (rec.can_create_mailbox ? 'Everything checks out — create your inbox below.' : 'Waiting for the records to go live. A failed check changes nothing.')
        + '</span></div>'
        + '<p style="font-size:12px;opacity:.7;margin:6px 0 0">Where do these go? Cloudflare: DNS → Records → Add record. Any other registrar: their DNS / “manage DNS” page. Use the Copy button on each row.</p>';
    } else {
      step2 = '<p style="margin:6px 0;opacity:.85">We could not read your records yet' + (S.recordsErr ? ': ' + esc(S.recordsErr) : '') + '.</p>'
        + '<button class="btn btn-secondary btn-sm" onclick="peCheckAgain()">Check again</button>';
    }

    // ── Step 3: the mailbox (and the start of the free month) ────────────────────────────────
    var ready = !!(d && rec && rec.can_create_mailbox);
    var step3 = '<div style="display:flex;align-items:center;gap:6px;margin-bottom:8px">'
      + '<input id="peSetupLocal" placeholder="info" style="flex:1;max-width:260px" ' + (ready ? '' : 'disabled') + '>'
      + '<span style="opacity:.75">@ ' + esc(d ? d.domain : 'your domain') + '</span></div>'
      + '<button class="btn btn-primary btn-sm" ' + (ready && !busy ? '' : 'disabled') + ' onclick="peSetupCreateInbox()">'
      + (ready ? 'Create my inbox' : 'Create my inbox — waiting for the records') + '</button>'
      + '<p style="font-size:12px;opacity:.7;margin:8px 0 0">Your free month starts when the inbox is created, not before — so time spent waiting on DNS costs you nothing. It runs for 30 days with full send and receive; after that the address keeps receiving and only sending stops, unless you keep it for $1/month or upgrade the CRM. No mail is ever deleted.</p>';

    return '<div style="padding:2px;text-align:left">'
      + '<h4 style="margin:0 0 4px">Set up your inbox</h4>'
      + '<p style="margin:0 0 12px;font-size:13px;opacity:.85">Your plan includes one private inbox, on your own domain, with your own Mailgun account. Three steps and it is live — this page stays here until it is done.</p>'
      + err
      + '<ol style="margin:0;padding-left:18px">'
      + '<li style="margin-bottom:14px"><strong>Tell us your domain</strong>' + (d ? '' : ' and paste your Mailgun keys') + '<div style="margin-top:6px">' + step1 + '</div></li>'
      + '<li style="margin-bottom:14px"><strong>Add the mail records at your DNS provider</strong><div style="margin-top:6px">' + step2 + '</div></li>'
      + '<li style="margin-bottom:6px"><strong>Create your inbox</strong><div style="margin-top:6px">' + step3 + '</div></li>'
      + '</ol></div>';
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

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // The FREE mailbox: trial countdown, and the receive-only state with BOTH upgrade doors.
  //
  // Every number comes from GET /private-email/mailbox-status, which the server computes from the
  // registry rows the send path itself enforces (`trial::send_access`) — so this banner cannot
  // promise "3 days left" over a send that would be refused, and it cannot say "receive-only" over a
  // mailbox that still sends.
  // ─────────────────────────────────────────────────────────────────────────────────────────────
  function trialBannerHtml() {
    var s = S.status;
    if (!s) return '';
    var hasBox = (s.mailboxes || []).length > 0;
    if (s.plan_sells_sending) return ''; // a paid plan: nothing to warn about
    if (!hasBox) return '';
    if (s.can_send) {
      var left = (s.days_left == null) ? 'a few' : s.days_left;
      var cap = s.send_cap_per_day ? (' Sending is capped at ' + s.send_cap_per_day + ' messages a day while you are on the free trial.') : '';
      return '<div style="padding:10px 14px;border-radius:8px;background:rgba(59,130,246,.12);border:1px solid rgba(59,130,246,.45);margin-bottom:14px">'
        + '<strong>Free inbox: ' + esc(left) + ' day' + (left === 1 ? '' : 's') + ' left</strong>'
        + '<div style="font-size:13px;opacity:.85;margin-top:4px">Send and receive is included until then. When the trial ends this address keeps receiving — it only stops sending, and nothing is deleted.' + esc(cap) + '</div>'
        + '<div style="margin-top:8px"><button class="btn btn-primary btn-sm" onclick="openUpgradeModal()">Keep the inbox — or upgrade the CRM</button></div></div>';
    }
    // Received-only: the brief's two doors. Both are real controls (the plan screen is where an
    // upgrade happens); the sentence says plainly that nothing was lost.
    return '<div style="padding:10px 14px;border-radius:8px;background:rgba(245,158,11,.14);border:1px solid rgba(245,158,11,.55);margin-bottom:14px">'
      + '<strong>This inbox is receive-only.</strong>'
      + '<div style="font-size:13px;opacity:.85;margin-top:4px">Mail is still arriving and nothing has been deleted — but sending has ended with the free trial. Keep just the inbox for $1/month, or upgrade the CRM; either one restores sending immediately.</div>'
      + '<div style="margin-top:8px;display:flex;gap:8px;flex-wrap:wrap">'
      + '<button class="btn btn-primary btn-sm" onclick="openUpgradeModal()">Keep inbox — $1/mo</button>'
      + '<button class="btn btn-secondary btn-sm" onclick="openUpgradeModal()">Upgrade CRM</button></div></div>';
  }

  function messagesHtml() {
    var m = S.messages || [];
    if (!m.length) {
      // A view-picker that always says "No mail" teaches nothing: an empty Spam is GOOD news and an
      // empty Trash means a delete really happened, so the sentence names the view it is about.
      var name = viewLabel();
      var hint = {
        spam: 'Messages the app treated as junk land here. Nothing here means no junk was caught.',
        trash: 'Deleted mail stays here until it is deleted for good from inside this view.',
        favorites: 'Press the star beside any message to keep it here.',
        archive: 'Archive a message and it leaves the inbox but stays searchable here.',
        drafts: 'Drafts you save while writing appear here.',
        sent: 'Replies and mail you send from this account appear here.'
      }[S.view] || 'Every message that arrives at one of your addresses appears here — including mail to addresses that do not create tickets.';
      return '<div class="empty-state" style="padding:20px"><h4 style="margin:0 0 6px">Nothing in ' + esc(name) + '</h4>'
        + '<p style="margin:0">' + esc(hint) + '</p></div>';
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

  // The folder RAIL. Every view below is a real server-side store the app already keeps
  // (`GET /private-email/folders` answers the eight system views with live counts plus any
  // subfolder the account created); this screen used to offer none of them, so Spam, Trash,
  // Archive and Favorites were unreachable from the CRM even though the mail was there.
  function railHtml() {
    var sys = S.folders.system || [];
    var custom = S.folders.custom || [];
    if (!sys.length) return '';
    function btn(f) {
      var active = (S.view === f.id);
      return '<button type="button" class="btn ' + (active ? 'btn-primary' : 'btn-ghost') + ' btn-xs"'
        + ' data-folder="' + esc(f.id) + '"'
        + ' aria-pressed="' + (active ? 'true' : 'false') + '"'
        + ' onclick="peSetView(\'' + esc(f.id) + '\')"'
        + ' style="margin:0 6px 6px 0">' + esc(f.name || f.id)
        + ' <span style="opacity:.75">' + Number(f.count || 0) + '</span></button>';
    }
    return '<div style="margin:0 0 10px">' + sys.map(btn).join('')
      + (custom.length
        ? '<div style="opacity:.7;font-size:12px;margin:2px 0 6px">Your folders</div>' + custom.map(btn).join('')
        : '')
      + '</div>';
  }

  function viewLabel() {
    var all = (S.folders.system || []).concat(S.folders.custom || []);
    var f = all.filter(function (x) { return x.id === S.view; })[0];
    return (f && f.name) ? f.name : 'Mail';
  }

  function render() {
    var c = S.counts || {};
    return '<div>'
      + '<div style="display:flex;justify-content:space-between;align-items:center;margin:0 0 12px">'
      + '<div><h3 style="margin:0">Mail</h3><span style="opacity:.7;font-size:13px">'
      + (c.unread || 0) + ' unread · ' + (c.tickets || 0) + ' turned into tickets · ' + fmtBytes(c.used_bytes) + ' of ' + fmtBytes(c.limit_bytes) + ' used'
      + '</span></div>'
      + '<button class="btn btn-primary btn-sm"' + (S.boxes.length ? '' : ' style="display:none"') + ' onclick="peCreateInbox()">+ Create new inbox</button></div>'
      + (S.err ? '<div style="padding:8px;margin-bottom:8px;border-radius:6px;background:rgba(220,80,80,.15)">' + esc(S.err) + '</div>' : '')
      + trialBannerHtml()
      + '<div style="padding:14px;border:1px solid rgba(128,128,128,.3);border-radius:8px;margin-bottom:16px">'
      + '<h4 style="margin:0 0 10px">Your inboxes</h4>' + inboxesHtml() + '</div>'
      + '<div style="padding:14px;border:1px solid rgba(128,128,128,.3);border-radius:8px">'
      + railHtml()
      + '<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px">'
      + '<h4 style="margin:0" data-view-label="' + esc(S.view) + '">' + esc(viewLabel()) + '</h4><div>' + addressFilterHtml()
      + '<button class="btn btn-ghost btn-xs" onclick="peLoad()">Refresh</button></div></div>'
      + '<div style="font-size:12px;opacity:.7;margin-bottom:8px">Mail stays here. A plan can limit how far back the history goes, but the free trial ending never deletes a message — it only stops sending.</div>'
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
        api().get('/private-email/addresses').catch(function () { return {}; }),
        // The rail the server already keeps: eight folder VIEWS with live counts plus any real
        // subfolder. Reading it here is what lets this screen offer Spam/Trash/Favorites at all —
        // without it the tab could only ever show the inbox, while mail in every other store was
        // unreachable from the CRM.
        api().get('/private-email/folders').catch(function () { return { system: [], custom: [] }; }),
        // The trial: countdown, whether sending is allowed, and the fair-use / storage meters. The
        // server derives all of it from the registry rows the send path enforces.
        api().get('/private-email/mailbox-status').catch(function () { return null; })
      ]);
      S.boxes = Array.isArray(results[0]) ? results[0] : (results[0].boxes || []);
      S.domains = Array.isArray(results[1]) ? results[1] : (results[1].domains || []);
      S.messages = (results[2] && results[2].messages) || [];
      S.counts = results[3] || {};
      S.addresses = (results[4] && results[4].addresses) || [];
      S.miscTotal = (results[4] && results[4].misc && results[4].misc.total) || 0;
      var rail = results[5] || {};
      S.folders = {
        system: Array.isArray(rail.system) ? rail.system : [],
        custom: Array.isArray(rail.custom) ? rail.custom : []
      };
      S.status = results[6] || null;
      S.err = null;
      // First-run setup (BYOK): with no mailbox yet, the checklist needs the domain's records. Read
      // them through the app's own endpoint — Mailgun's values, never a hardcoded DKIM — and keep a
      // failure in `recordsErr` so the page can say so instead of rendering an empty checklist.
      if (!S.boxes.length) {
        if (S.domains.length) {
          try {
            S.records = await api().get('/private-email/domains/' + S.domains[0].id + '/records');
            S.recordsErr = null;
          } catch (e) {
            S.records = null;
            S.recordsErr = fail(e);
          }
        } else {
          S.records = null;
          S.recordsErr = null;
        }
      }
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

  // Re-ask Mailgun for this domain's state, then reload (which re-reads the records checklist).
  // A failed check changes nothing — the row, the credential and any existing mailbox are untouched.
  window.peCheckDomain = async function (domainId) {
    var m = document.getElementById('peDomainMsg');
    if (m) m.textContent = 'Checking…';
    try {
      var r = await api().post('/private-email/domains/' + domainId + '/verify', {});
      if (m) m.textContent = r.verified
        ? 'Active — the records are live.'
        : ('Still ' + (r.mailgun_state || 'waiting') + ' — the records have not gone live yet. ');
      load();
    } catch (e) {
      if (m) m.textContent = fail(e);
    }
  };

  // The three setup doors (David 2026-11-11, BYOK). The server owns every rule — plan ceiling, key
  // validity, domain readiness — and answers with a sentence, which is shown where the user is
  // looking rather than as a disappearing toast.
  window.peCheckAgain = function () {
    var d = (S.domains || [])[0];
    if (!d) { load(); return; }
    window.peCheckDomain(d.id);
  };

  window.peSetupAddDomain = async function () {
    if (S.busy) return;
    var nameEl = document.getElementById('peSetupDomain');
    var keyEl = document.getElementById('peSetupKey');
    var hookEl = document.getElementById('peSetupHook');
    var regionEl = document.getElementById('peSetupRegion');
    var domain = ((nameEl && nameEl.value) || '').trim().toLowerCase()
      .replace(/^https?:\/\//, '').replace(/\/.*$/, '');
    var key = ((keyEl && keyEl.value) || '').trim();
    var hook = ((hookEl && hookEl.value) || '').trim();
    S.setupErr = null;
    if (!domain || domain.indexOf('.') < 0) {
      S.setupErr = 'Enter the domain you own, for example yourbusiness.com.';
      load(); return;
    }
    if (!key) {
      S.setupErr = 'Paste the Mailgun API key for this domain — it is what lets us read its mail records.';
      load(); return;
    }
    var payload = {
      domain: domain,
      provider_type: 'mailgun',
      mailgun_api_key: key,
      mailgun_region: (regionEl && regionEl.value) || 'us'
    };
    // Only sent when typed: without it the domain is send-only and inbound deliveries are refused,
    // so the setup page asks for it rather than discovering the gap on the first reply.
    if (hook) payload.webhook_signing_key = hook;
    S.busy = true;
    render();
    try {
      await api().post('/private-email/domains', payload);
      S.busy = false;
      toastMsg('Domain added — now add the records it lists', 'success');
      load();
    } catch (e) {
      S.busy = false;
      S.setupErr = fail(e);
      render();
    }
  };

  window.peCopy = function (btn) {
    var text = (btn && btn.getAttribute('data-copy')) || '';
    function done() { var old = btn.textContent; btn.textContent = 'Copied'; setTimeout(function () { btn.textContent = old; }, 1500); }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, function () { fallbackCopy(text, done); });
    } else {
      fallbackCopy(text, done);
    }
  };

  function fallbackCopy(text, done) {
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); done(); } catch (e) { toastMsg('Select the value and copy it by hand', 'error'); }
    document.body.removeChild(ta);
  }

  window.peSetupCreateInbox = async function () {
    if (S.busy) return;
    var d = (S.domains || [])[0];
    var inp = document.getElementById('peSetupLocal');
    var local = ((inp && inp.value) || '').trim().replace(/@.*$/, '');
    S.setupErr = null;
    if (!d) { load(); return; }
    if (!local) {
      S.setupErr = 'Choose the part before the @ sign — for example info or your own name.';
      load(); return;
    }
    S.busy = true;
    render();
    try {
      // The server starts the 30-day clock HERE, on a verified domain (never at signup), so a slow
      // DNS change costs the customer nothing.
      await api().post('/private-email/boxes', { domain_id: d.id, local_part: local });
      S.busy = false;
      toastMsg('Your inbox is live — send and receive is on for 30 days', 'success');
      load();
    } catch (e) {
      S.busy = false;
      S.setupErr = fail(e);
      render();
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

  // The rail's own control: the view is what the server's folder column filters on, so switching is a
  // reload — never a client-side hide, which would disagree with the counts beside the buttons.
  window.peSetView = function (v) {
    S.view = v || 'inbox';
    var d = document.getElementById('peDetail'); if (d) d.innerHTML = '';
    load();
  };

  // Move a message to a real store. `spam` and `trash` are where mail a customer calls junk or
  // unwanted belongs; both are restorable until deleted from inside their own view.
  window.peMove = async function (id, folder) {
    try {
      await api().patch('/private-email/messages/' + id, { folder: folder });
      var d = document.getElementById('peDetail'); if (d) d.innerHTML = '';
      toastMsg(folder === 'spam' ? 'Moved to Spam' : folder === 'trash' ? 'Moved to Trash' : 'Moved to Inbox', 'success');
      load();
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

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
        + (m.folder !== 'inbox' ? '<button class="btn btn-ghost btn-xs" onclick="peMove(\'' + esc(m.id) + '\',\'inbox\')">To Inbox</button> ' : '')
        + (m.folder !== 'spam' ? '<button class="btn btn-ghost btn-xs" onclick="peMove(\'' + esc(m.id) + '\',\'spam\')">Spam</button> ' : '')
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
