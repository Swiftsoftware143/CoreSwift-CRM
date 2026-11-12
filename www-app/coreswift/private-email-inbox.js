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
    boxes: [], domains: [], messages: [], counts: {}, addresses: [], labels: [],
    folders: { system: [], custom: [] },
    // The people work can be handed to (kanban t_5f314e9a): the SAME list the Team screen shows,
    // read from `/private-email/members`, which only ever answers ACTIVE members of this account.
    members: [],
    miscTotal: 0, allTotal: null, view: 'inbox', address: 'all', sel: null, err: null,
    // The label that is filtering the list (a label row behaves like a folder view: the server
    // filters on it, so the count beside the label and the rows shown can never disagree).
    label: '',
    // How the list is ordered. 'date' is the server's own newest-first order; 'alpha' sorts the
    // rows this screen already has by subject/sender. Sort is a per-view choice, not stored state.
    sort: 'date',
    // The FREE mailbox's trial state (kanban t_2ef67217): read from the server, which computes it
    // from the same registry rows the send path enforces — so a countdown here can never promise
    // sending the send path would refuse.
    status: null,
    // First-run setup (BYOK): the verified-domain records checklist, why it could not be read, the
    // last refusal, and one busy flag shared by all three steps so a double click cannot add the
    // domain twice.
    records: null, recordsErr: null, setupErr: null, busy: false,
    // Per-domain records answers, keyed by domain id (kanban t_54b5d21b). ONE source for "can this
    // domain carry an inbox": the setup sentence, the setup control, the Mail tab's button and the
    // create modal all read it, so no surface can invite an action another one refuses.
    ready: {}, readyErr: {}
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
      // The LOG IN button opens the customer's OWN branded name when it really serves the mail
      // client, and the shared host while their CNAME is still spreading — a button that opens a
      // broken address is worse than one that opens a working one under our name.
      var branded = dom.branded === 'active' && !!dom.branded_url;
      var base = branded ? dom.branded_url : MAIL_HOST;
      return '<tr>'
        + '<td style="padding:8px"><strong>' + esc(b.email_address) + '</strong>'
        + (b.creates_ticket ? ' <span style="background:#e0f2fe;color:#075985;padding:1px 6px;border-radius:8px;font-size:11px">tickets</span>' : '')
        + '</td>'
        + '<td style="padding:8px;opacity:.75">' + (ready ? 'Receiving' : 'Waiting for mail records')
        + '<div style="font-size:11px;opacity:.8">'
        + (branded ? 'opens at inbox.' + esc(dom.domain) : 'opens at inbox.coreswiftcrm.com until inbox.' + esc(dom.domain || 'your domain') + ' is live')
        + '</div></td>'
        + '<td style="padding:8px;opacity:.75">' + count + '</td>'
        // WHO LOOKS AFTER THIS INBOX (kanban t_5f314e9a). The teammate is a real person in this
        // account; clearing it gives the address back to the whole workspace. The write goes to
        // PATCH /private-email/boxes/:id, the SAME `user_id` column this inbox was provisioned with.
        + '<td style="padding:8px">' + assignSelectHtml('box', b.id, b.user_id) + '</td>'
        + '<td style="padding:8px;text-align:right;white-space:nowrap">'
        + '<label style="font-size:11px;opacity:.75;margin-right:10px;cursor:pointer"><input type="checkbox" '
        + (b.creates_ticket ? 'checked ' : '') + 'onchange="peToggleTicket(\'' + esc(b.id) + '\',this.checked)"> also a ticket</label>'
        + (ready
          ? '<button class="btn btn-primary btn-sm" onclick="peLogin(\'' + esc(base) + '\',\'' + esc(b.email_address) + '\')">LOG IN</button>'
          : '<button class="btn btn-secondary btn-sm" disabled title="This address cannot receive mail yet">LOG IN</button>')
        + ' <button class="btn btn-ghost btn-xs" title="Delete this inbox" onclick="peDeleteBox(\'' + esc(b.id) + '\',\'' + esc(b.email_address) + '\')">Delete</button>'
        + '</td></tr>';
    }).join('');
    return '<table style="width:100%;border-collapse:collapse"><thead><tr style="text-align:left;opacity:.7">'
      + '<th style="padding:6px 8px">Inbox</th><th style="padding:6px 8px">Status</th><th style="padding:6px 8px">Messages</th><th style="padding:6px 8px">Assigned to</th><th></th>'
      + '</tr></thead><tbody>' + rows + '</tbody></table>';
  }

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // "Who is this for" — the ONE assignment control, used by an inbox row and by a conversation.
  //
  // Both write the same thing: a person in THIS account (the server refuses anybody else, and refuses
  // a removed teammate). An empty choice is a real answer, not "no change": it hands the inbox or the
  // conversation back to the whole workspace, which is what a set-only door could never do.
  // ─────────────────────────────────────────────────────────────────────────────────────────────
  function memberById(id) {
    if (!id) return null;
    return (S.members || []).filter(function (m) { return m.id === id; })[0] || null;
  }

  function memberLabel(m) {
    return (m && (m.name || m.email)) || 'someone';
  }

  function assignSelectHtml(kind, id, currentId) {
    var cur = currentId || '';
    var opts = '<option value="">Nobody — the whole workspace</option>'
      + (S.members || []).map(function (m) {
        return '<option value="' + esc(m.id) + '"' + (m.id === cur ? ' selected' : '') + '>'
          + esc(memberLabel(m)) + '</option>';
      }).join('');
    return '<select class="pe-assign" data-kind="' + esc(kind) + '" data-id="' + esc(id) + '" '
      + 'onchange="peAssign(this.getAttribute(\'data-kind\'),this.getAttribute(\'data-id\'),this.value)" '
      + 'style="max-width:180px;font-size:12px" title="Who looks after this">' + opts + '</select>';
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
    if (r.attention) return '<span style="background:#fee2e2;color:#991b1b;padding:1px 7px;border-radius:8px;font-size:11px">Needs attention</span>';
    if (r.optional && r.valid == null) return '<span style="background:#e5e7eb;color:#374151;padding:1px 7px;border-radius:8px;font-size:11px">Optional</span>';
    if (r.valid === true) return '<span style="background:#dcfce7;color:#166534;padding:1px 7px;border-radius:8px;font-size:11px">Active</span>';
    if (r.valid === false) return '<span style="background:#fef3c7;color:#92400e;padding:1px 7px;border-radius:8px;font-size:11px">Waiting</span>';
    return '<span style="background:#e0f2fe;color:#075985;padding:1px 7px;border-radius:8px;font-size:11px">Add this</span>';
  }

  function recordRowHtml(r) {
    // Every row carries ONE plain sentence about what it does (the card's rule) and the value's own
    // copy button. The branded CNAME adds its orange-cloud note, because it is the only record on
    // this page that must be proxied — the opposite of every mail record above it.
    return '<tr>'
      + '<td style="padding:6px 8px;white-space:nowrap;vertical-align:top"><strong>' + esc(r.type) + '</strong></td>'
      + '<td style="padding:6px 8px;font-family:monospace;font-size:12px;word-break:break-all;vertical-align:top">' + esc(r.name) + '</td>'
      + '<td style="padding:6px 8px;word-break:break-all;max-width:420px;vertical-align:top">'
      + '<span style="font-family:monospace;font-size:12px">' + esc(r.value) + '</span>'
      + (r.purpose ? '<div style="font-size:12px;opacity:.75;margin-top:2px">' + esc(r.purpose) + '</div>' : '')
      + (r.note ? '<div style="font-size:12px;color:#92400e;margin-top:2px">' + esc(r.note) + '</div>' : '')
      + '</td>'
      + '<td style="padding:6px 8px;text-align:center;vertical-align:top">' + (r.priority == null ? '' : esc(String(r.priority))) + '</td>'
      + '<td style="padding:6px 8px;white-space:nowrap;vertical-align:top">' + badgeFor(r) + '</td>'
      + '<td style="padding:6px 8px;text-align:right;vertical-align:top"><button class="btn btn-ghost btn-xs" data-copy="'
      + esc(r.value) + '" onclick="peCopy(this)">Copy</button></td>'
      + '</tr>';
  }

  function recordsHtml(rec) {
    var out = '';
    var groups = rec.groups || [];
    groups.forEach(function (g) {
      var rows = (g.records || []).map(recordRowHtml).join('');
      if (!rows) {
        // A group Mailgun returns nothing for (some verified domains have no echo of their DNS
        // entries). Say so, rather than rendering an empty heading the customer reads as a fault.
        out += '<h5 style="margin:14px 0 4px">' + esc(g.title) + '</h5>'
          + '<p style="margin:4px 0 10px;font-size:13px;opacity:.75">Nothing to add here — your mail provider reports no records for this group on this domain.</p>';
        return;
      }
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
      + '<li>The <strong>inbox CNAME</strong> (the branded address) is the ONE exception: it must be <strong>Proxied (orange cloud)</strong>, because your Cloudflare serves the padlock on that name.</li>'
      + '</ul></div>';
  }

  // The plan's real inbox allowance (kanban t_c55b3bd5). This block used to tell EVERY customer
  // "one private inbox", so a Starter workspace that bought three was told it had one. The number
  // comes from GET /private-email/mailbox-status, whose `mailbox_limit` is the same registry figure
  // the server's own ceiling refuses on (null there = the plan sells unlimited). Until that answer
  // arrives we print no number at all rather than a default that could be wrong.
  function mailboxAllowanceSentence() {
    var s = S.status;
    var n = (s && s.mailbox_limit !== undefined) ? s.mailbox_limit : undefined;
    if (n === undefined) return 'Your plan includes a private inbox on your own domain.';
    if (n === null) return 'Your plan includes unlimited private inboxes on your own domain.';
    n = Number(n);
    if (!isFinite(n)) return 'Your plan includes a private inbox on your own domain.';
    if (n <= 0) return 'Your current plan does not include a private inbox.';
    if (n === 1) return 'Your plan includes one private inbox on your own domain.';
    return 'Your plan includes ' + n + ' private inboxes on your own domain.';
  }

  // The records answer for a domain, when it has been read.
  function readyRec(d) {
    return (d && S.ready && S.ready[String(d.id)]) || null;
  }

  // Is this domain receiving mail, per the LIVE measurement rather than the stored row flag?
  function domainVerified(d) {
    var rec = readyRec(d);
    return rec ? !!rec.verified : !!(d && d.verified);
  }

  // May an inbox be created on this domain? True exactly when the server's write door accepts:
  // the records answer's own can_create_mailbox, or a stored verification (the same fallback the
  // write door uses when the records cannot be read at all).
  function canCreateOn(d) {
    if (!d) return false;
    var rec = readyRec(d);
    if (rec) return !!rec.can_create_mailbox || !!d.verified;
    return !!d.verified;
  }

  function setupHtml() {
    var d = (S.domains || [])[0];
    var rec = (d && S.records && String(S.records.domain_id) === String(d.id)) ? S.records : null;
    // The control, the sentence and (below) the button all read this ONE value, so the page cannot
    // tell a customer everything checks out while disabling the button that acts on it.
    var canCreate = canCreateOn(d);
    var busy = S.busy;
    var err = S.setupErr ? '<p style="margin:0 0 8px;color:#b45309">' + esc(S.setupErr) + '</p>' : '';

    // ── Step 1: the domain + the customer's own Mailgun key ──────────────────────────────────
    var step1;
    if (d) {
      // The two choices stay visible (and switchable) after the domain exists: a customer who picked
      // "send only" by mistake otherwise has no way back that does not delete the domain and every
      // mailbox on it. Sending is shown, not offered — changing it needs the other service's
      // credentials, which is a different form.
      var sendLabel = d.provider_type === 'smtp' ? 'your own SMTP service' : 'Mailgun';
      var recvLabel = d.inbound_provider === 'sendgrid' ? 'SendGrid'
        : (d.inbound_provider === 'none' ? 'nowhere (this domain only sends)' : 'Mailgun');
      step1 = '<div style="margin:4px 0 0"><strong>' + esc(d.domain) + '</strong> — '
        + (domainVerified(d) ? '<span style="color:#166534">verified</span>' : '<span style="color:#92400e">waiting for its records</span>')
        + '</div>'
        + '<p style="font-size:13px;margin:8px 0 0;opacity:.85">Sending: <strong>' + esc(sendLabel)
        + '</strong> &middot; Receiving: <strong>' + esc(recvLabel) + '</strong></p>'
        + '<div class="form-group" style="margin-top:10px"><label>Change how this domain receives</label>'
        + '<select id="peSetupReceiving2" onchange="peSetupChangeReceiving(this.value)">'
        + '<option value="mailgun"' + (d.inbound_provider === 'mailgun' ? ' selected' : '') + '>Mailgun</option>'
        + '<option value="sendgrid"' + (d.inbound_provider === 'sendgrid' ? ' selected' : '') + '>SendGrid — Inbound Parse</option>'
        + '<option value="none"' + (d.inbound_provider === 'none' ? ' selected' : '') + '>Nowhere — this domain only sends</option>'
        + '</select>'
        + '<p style="font-size:12px;opacity:.7;margin:4px 0 0">Sending is unchanged by this. Switching to SendGrid gives you the exact Inbound Parse URL to paste into your SendGrid account; switching to Mailgun needs its webhook signing key, which you can paste on the Mailgun side of this page.</p></div>';
    } else {
      // TWO separate choices (David 2026-11-11, providers beyond Mailgun): how mail LEAVES and how
      // it ARRIVES. They are independent on purpose — sending through Amazon SES while receiving
      // through Mailgun is a supported setup — so the page never implies one requires the other.
      step1 = '<p style="margin:6px 0 10px;font-size:13px;opacity:.85">You need a domain you already control (from Cloudflare or your registrar) and a way to send and receive its mail. Those are two separate choices: you can send through Amazon SES and receive through Mailgun, or the other way round. Nothing passes through our accounts.</p>'
        + '<div class="form-group"><label>Your domain</label><input id="peSetupDomain" placeholder="yourbusiness.com" autocomplete="off"></div>'
        + '<div class="form-group"><label>How should your mail leave? (sending)</label>'
        + '<select id="peSetupSending" onchange="peSetupChoiceChanged()">'
        + '<option value="mailgun">Mailgun — my own Mailgun account</option>'
        + '<option value="smtp">Any SMTP service — Amazon SES, SendGrid, Postmark, Google Workspace</option>'
        + '</select></div>'
        + '<div id="peSetupSmtpBox" style="display:none">'
        + '<div class="form-group"><label>Mail server (hostname)</label><input id="peSetupSmtpHost" placeholder="email-smtp.us-east-1.amazonaws.com" autocomplete="off"></div>'
        + '<div class="form-group"><label>Port</label><input id="peSetupSmtpPort" placeholder="587" autocomplete="off"></div>'
        + '<div class="form-group"><label>Username</label><input id="peSetupSmtpUser" autocomplete="off"></div>'
        + '<div class="form-group"><label>Password</label><input id="peSetupSmtpPass" type="password" autocomplete="off"><p style="font-size:12px;opacity:.7;margin:4px 0 0">For Amazon SES this is the SMTP username and password it gives you for your sending credentials — not your console login.</p></div>'
        + '<div class="form-group" style="display:flex;gap:8px;align-items:center"><input type="checkbox" id="peSetupSmtpTls" checked style="width:auto"><label style="margin:0">Use a secure connection (TLS) — leave this on unless your server says otherwise</label></div>'
        + '</div>'
        + '<div class="form-group"><label>How should your mail arrive? (receiving)</label>'
        + '<select id="peSetupReceiving" onchange="peSetupChoiceChanged()">'
        + '<option value="mailgun">Mailgun — my own Mailgun account</option>'
        + '<option value="sendgrid">SendGrid — Inbound Parse</option>'
        + '<option value="none">Nowhere — this domain only sends</option>'
        + '</select>'
        + '<p id="peSetupRecvNote" style="font-size:12px;opacity:.7;margin:4px 0 0"></p></div>'
        + '<div id="peSetupMailgunBox">'
        + '<div class="form-group"><label>Mailgun region</label><select id="peSetupRegion"><option value="us">United States (api.mailgun.net)</option><option value="eu">Europe (api.eu.mailgun.net)</option></select></div>'
        + '<div class="form-group"><label>Mailgun API key</label><input id="peSetupKey" type="password" placeholder="key-…" autocomplete="off"><p style="font-size:12px;opacity:.7;margin:4px 0 0">In Mailgun: Settings → API keys. It is stored sealed and never shown again.</p></div>'
        + '</div>'
        + '<div class="form-group" id="peSetupHookBox"><label>Mailgun webhook signing key <span style="opacity:.7">(so incoming mail can be verified)</span></label><input id="peSetupHook" type="password" placeholder="from Mailgun → Settings → Webhooks" autocomplete="off"></div>'
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
        + (canCreate ? 'Everything checks out — create your inbox below.' : 'Waiting for the records to go live. A failed check changes nothing.')
        + '</span></div>'
        + '<p style="font-size:12px;opacity:.7;margin:6px 0 0">Where do these go? Cloudflare: DNS → Records → Add record. Any other registrar: their DNS / “manage DNS” page. Use the Copy button on each row.</p>';
    } else {
      step2 = '<p style="margin:6px 0;opacity:.85">We could not read your records yet' + (S.recordsErr ? ': ' + esc(S.recordsErr) : '') + '.</p>'
        + '<button class="btn btn-secondary btn-sm" onclick="peCheckAgain()">Check again</button>';
    }

    // ── Step 3: the mailbox (and the start of the free month) ────────────────────────────────
    var ready = canCreate;
    var step3 = '<div style="display:flex;align-items:center;gap:6px;margin-bottom:8px">'
      + '<input id="peSetupLocal" placeholder="info" style="flex:1;max-width:260px" ' + (ready ? '' : 'disabled') + '>'
      + '<span style="opacity:.75">@ ' + esc(d ? d.domain : 'your domain') + '</span></div>'
      + '<button class="btn btn-primary btn-sm" ' + (ready && !busy ? '' : 'disabled') + ' onclick="peSetupCreateInbox()">'
      + (ready ? 'Create my inbox' : 'Create my inbox — waiting for the records') + '</button>'
      + '<p style="font-size:12px;opacity:.7;margin:8px 0 0">Your free month starts at your first verified inbox — when you create it on a domain whose records are Active, not when you signed up — so time spent waiting on DNS costs you nothing. It runs for 30 days with full send and receive; after that the address keeps receiving and only sending stops, unless you keep it for $1/month or upgrade the CRM. No mail is ever deleted.</p>';

    return '<div style="padding:2px;text-align:left">'
      + '<h4 style="margin:0 0 4px">Set up your inbox</h4>'
      + '<p style="margin:0 0 12px;font-size:13px;opacity:.85">' + mailboxAllowanceSentence() + ' You choose how it SENDS and how it RECEIVES — those are separate answers, so you can keep the service that already sends your mail. Three steps and it is live; this page stays here until it is done.</p>'
      + err
      + '<ol style="margin:0;padding-left:18px">'
      + '<li style="margin-bottom:14px"><strong>Tell us your domain</strong>' + (d ? '' : ' and choose how it sends and receives') + '<div style="margin-top:6px">' + step1 + '</div></li>'
      + '<li style="margin-bottom:14px"><strong>Add the mail records at your DNS provider</strong><div style="margin-top:6px">' + step2 + '</div></li>'
      + '<li style="margin-bottom:6px"><strong>Create your inbox</strong><div style="margin-top:6px">' + step3 + '</div></li>'
      + '</ol></div>';
  }

  function addressFilterHtml() {
    // The server answers an `all` total for exactly this filter (every message not in Trash), so the
    // count beside "All addresses" is that number rather than the Inbox badge, which counts a
    // different set of rows.
    var allTotal = (S.allTotal == null) ? (S.counts.inbox || 0) : S.allTotal;
    var opts = ['<option value="all"' + (S.address === 'all' ? ' selected' : '') + '>All addresses (' + allTotal + ')</option>'];
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

  // STORAGE (B206, David 2026-11-11). The meter is always on screen; the warning block appears at
  // 50 / 75 / 99 / 100 and stays until space is freed. Both come from GET /messages/counts, whose
  // numbers are the same `storage::usage` the server refuses a new file with — so this panel cannot
  // show room the server would refuse. Nothing here is dismissible.
  function storagePanelHtml() {
    var st = S.counts && S.counts.storage;
    if (!st) return '';
    var pct = Math.max(0, Math.min(100, Number(st.percent || 0)));
    var colour = pct >= 100 ? '#b91c1c' : (pct >= 75 ? '#b45309' : '#2563eb');
    var html = '<div style="padding:12px 14px;border:1px solid rgba(128,128,128,.3);border-radius:8px;margin-bottom:16px">'
      + '<div style="display:flex;justify-content:space-between;align-items:baseline;gap:10px">'
      + '<strong style="font-size:13px">Storage</strong>'
      + '<span style="font-size:12px;opacity:.8">' + esc(st.used_human || '') + ' of '
      + esc(st.quota_human || '') + ' used (' + pct + '%)</span></div>'
      + '<div style="height:8px;border-radius:999px;background:rgba(128,128,128,.25);margin:8px 0 6px;overflow:hidden">'
      + '<i style="display:block;height:100%;width:' + pct + '%;background:' + colour + '"></i></div>'
      + '<div style="font-size:12px;opacity:.75">Incoming mail is never refused and nothing is deleted to make room.</div>';
    if (st.banner) {
      var b = st.banner;
      var bg = b.level >= 100 ? 'rgba(220,38,38,.14)' : (b.level >= 75 ? 'rgba(245,158,11,.16)' : 'rgba(59,130,246,.12)');
      var bd = b.level >= 100 ? 'rgba(220,38,38,.5)' : (b.level >= 75 ? 'rgba(245,158,11,.55)' : 'rgba(59,130,246,.45)');
      html += '<div style="margin-top:10px;padding:10px 12px;border-radius:8px;background:' + bg + ';border:1px solid ' + bd + '">'
        + '<strong>' + esc(b.headline) + '</strong>'
        + '<div style="font-size:12.5px;opacity:.85;margin-top:3px">' + esc(b.action) + '</div>'
        + '<div style="margin-top:8px"><button class="btn btn-primary btn-sm" onclick="openUpgradeModal()">'
        + (b.level >= 100 ? 'Get more storage' : 'See plans') + '</button></div></div>';
    }
    var ret = (S.counts && S.counts.retention && S.counts.retention.sentence) || '';
    if (ret) html += '<div style="font-size:11.5px;opacity:.6;margin-top:8px">' + esc(ret) + '</div>';
    return html + '</div>';
  }

  function messagesHtml() {
    var m = (S.messages || []).slice();
    if (S.sort === 'alpha') {
      // Alphabetical is a CLIENT-side order over the page the server returned: the server's own
      // order is by arrival (newest first) and it has no sort parameter. Sorting the rows in hand
      // keeps the promise the menu makes without pretending to sort mail it never fetched.
      m.sort(function (a, b) {
        var ka = String(a.subject || a.from_name || a.from_address || '').toLowerCase();
        var kb = String(b.subject || b.from_name || b.from_address || '').toLowerCase();
        return ka < kb ? -1 : ka > kb ? 1 : 0;
      });
    }
    if (!m.length) {
      // A view-picker that always says "No mail" teaches nothing: an empty Spam is GOOD news and an
      // empty Trash means a delete really happened, so the sentence names the view it is about.
      var name = viewLabel();
      var hint = S.label
        ? 'No message carries this label yet. Open any message and pick it under Labels.'
        : ({
        spam: 'Messages the app treated as junk land here. Nothing here means no junk was caught.',
        trash: 'Deleted mail stays here until it is deleted for good from inside this view.',
        favorites: 'Press the star beside any message to keep it here.',
        archive: 'Archive a message and it leaves the inbox but stays searchable here.',
        drafts: 'Drafts you save while writing appear here.',
        sent: 'Replies and mail you send from this account appear here.'
      }[S.view] || 'Every message that arrives at one of your addresses appears here — including mail to addresses that do not create tickets.');
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
        // Who is working this thread — the same assignment the ticket queue shows (kanban t_5f314e9a).
        + (x.assigned_user_id
          ? ' <span style="background:#e0e7ff;color:#3730a3;padding:1px 6px;border-radius:8px;font-size:11px">'
            + esc(memberLabel(memberById(x.assigned_user_id))) + '</span>'
          : '')
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
    // A folder that is a real STORE can be acted on (make a subfolder, mark all read/unread,
    // empty it). Unread and Favorites are VIEWS over other stores: a bulk action there would
    // silently reach mail the user cannot see in that view, so no menu is offered.
    var STORES = { inbox: 1, drafts: 1, sent: 1, archive: 1, spam: 1, trash: 1 };
    function btn(f, isCustom) {
      var raw = f.id;
      var active = !S.label && (S.view === raw);
      return '<span style="display:inline-flex;align-items:center;margin:0 6px 6px 0">'
        + '<button type="button" class="btn ' + (active ? 'btn-primary' : 'btn-ghost') + ' btn-xs"'
        + ' data-folder="' + esc(raw) + '"'
        + ' aria-pressed="' + (active ? 'true' : 'false') + '"'
        + ' onclick="peSetView(\'' + esc(raw) + '\')">' + esc(f.name || f.id)
        + ' <span style="opacity:.75">' + Number(f.count || 0) + '</span></button>'
        + ((isCustom || STORES[f.id])
          ? '<button type="button" class="btn btn-ghost btn-xs" style="padding:0 4px"'
            + ' title="' + esc((f.name || f.id) + ' actions') + '"'
            + ' aria-label="' + esc((f.name || f.id) + ' actions') + '"'
            + ' onclick="peFolderMenu(\'' + esc(raw) + '\',' + (isCustom ? 'true' : 'false') + ')">&#8942;</button>'
          : '')
        + '</span>';
    }
    return '<div style="margin:0 0 10px">' + sys.map(function (f) { return btn(f, false); }).join('')
      + (custom.length
        ? '<div style="opacity:.7;font-size:12px;margin:2px 0 6px">Your folders</div>'
          + custom.map(function (f) { return btn(f, true); }).join('')
        : '')
      + labelsRailHtml()
      + '</div>';
  }

  // The labels rail: one row per label, its colour, how much mail carries it, and a menu to edit
  // or delete it. Clicking the row filters the list by that label (the server-side filter above),
  // so a label behaves like a folder view rather than a decoration.
  function labelsRailHtml() {
    var labels = S.labels || [];
    var head = '<div style="opacity:.7;font-size:12px;margin:6px 0 6px;display:flex;align-items:center;gap:8px">'
      + '<span>Labels</span>'
      + '<button type="button" class="btn btn-ghost btn-xs" style="padding:0 4px" title="Create a label"'
      + ' onclick="peLabelNew()">+</button></div>';
    if (!labels.length) {
      return head + '<div style="opacity:.6;font-size:12px;margin:0 0 6px">None yet — press + to make one, then tag any message.</div>';
    }
    var rows = labels.map(function (l) {
      var active = S.label === l.id;
      return '<span style="display:inline-flex;align-items:center;margin:0 6px 6px 0">'
        + '<button type="button" class="btn ' + (active ? 'btn-primary' : 'btn-ghost') + ' btn-xs"'
        + ' data-label="' + esc(l.id) + '" aria-pressed="' + (active ? 'true' : 'false') + '"'
        + ' onclick="peSetLabel(\'' + esc(l.id) + '\')">'
        + '<i style="display:inline-block;width:9px;height:9px;border-radius:999px;margin-right:6px;background:' + esc(l.color || '#64748b') + '"></i>'
        + esc(l.name) + ' <span style="opacity:.75">' + Number(l.used || 0) + '</span></button>'
        + '<button type="button" class="btn btn-ghost btn-xs" style="padding:0 4px"'
        + ' title="' + esc(l.name + ' actions') + '" aria-label="' + esc(l.name + ' actions') + '"'
        + ' onclick="peLabelMenu(\'' + esc(l.id) + '\')">&#8942;</button>'
        + '</span>';
    }).join('');
    return head + '<div>' + rows + '</div>';
  }


  function viewLabel() {
    if (S.label) {
      var l = (S.labels || []).filter(function (x) { return x.id === S.label; })[0];
      if (l) return 'Label: ' + l.name;
    }
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
      + '<button class="btn btn-primary btn-sm"' + (S.boxes.length ? '' : ' style="display:none"') + ' '
      + ((S.domains || []).some(canCreateOn)
        ? 'onclick="peCreateInbox()"'
        : 'disabled title="Available once your mail records verify"')
      + '>+ Create new inbox</button>'
      + ((S.boxes.length && !(S.domains || []).some(canCreateOn))
        ? '<div style="font-size:12px;opacity:.75;margin-top:4px">New inboxes unlock once your mail records verify. Nothing is charged before then.</div>'
        : '')
      + '</div>'
      + (S.err ? '<div style="padding:8px;margin-bottom:8px;border-radius:6px;background:rgba(220,80,80,.15)">' + esc(S.err) + '</div>' : '')
      + trialBannerHtml()
      + storagePanelHtml()
      + '<div style="padding:14px;border:1px solid rgba(128,128,128,.3);border-radius:8px;margin-bottom:16px">'
      + '<h4 style="margin:0 0 10px">Your inboxes</h4>' + inboxesHtml() + '</div>'
      + '<div style="padding:14px;border:1px solid rgba(128,128,128,.3);border-radius:8px">'
      + railHtml()
      + '<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px">'
      + '<h4 style="margin:0" data-view-label="' + esc(viewLabel()) + '">' + esc(viewLabel())
      + ' <span style="font-size:12px;opacity:.6;font-weight:400">' + (S.sort === 'alpha' ? 'A–Z' : 'newest first') + '</span></h4>'
      + '<div>' + (S.label ? '<button class="btn btn-ghost btn-xs" onclick="peSetLabel(\'\')">Clear label</button> ' : '')
      + addressFilterHtml()
      + '<button class="btn btn-ghost btn-xs" onclick="peLoad()">Refresh</button></div></div>'
      + '<div style="font-size:12px;opacity:.7;margin-bottom:8px">Mail stays here and is never deleted to make room for new mail. A plan sets how much storage you have; the free trial ending never deletes a message — it only stops sending. The time-based backstop, if any applies to this account, is stated with the meter above.</div>'
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
        // A label is a filter over the whole mailbox (the server's `folder=all` excludes Spam and
        // Trash exactly as the All view does), so picking a label leaves the folder view rather
        // than intersecting two filters the user cannot see at once.
        api().get('/private-email/messages?folder=' + encodeURIComponent(S.label ? 'all' : S.view)
          + '&address=' + encodeURIComponent(S.address)
          + (S.label ? '&label=' + encodeURIComponent(S.label) : '')
          + '&limit=50'),
        api().get('/private-email/messages/counts').catch(function () { return {}; }),
        api().get('/private-email/addresses').catch(function () { return {}; }),
        // The rail the server already keeps: eight folder VIEWS with live counts plus any real
        // subfolder. Reading it here is what lets this screen offer Spam/Trash/Favorites at all —
        // without it the tab could only ever show the inbox, while mail in every other store was
        // unreachable from the CRM.
        api().get('/private-email/folders').catch(function () { return { system: [], custom: [] }; }),
        // The trial: countdown, whether sending is allowed, and the fair-use / storage meters. The
        // server derives all of it from the registry rows the send path enforces.
        api().get('/private-email/mailbox-status').catch(function () { return null; }),
        // Labels are account-level name+colour rows with a live usage count; the rail and the
        // message screen both read this one list so the two can never disagree.
        api().get('/private-email/labels').catch(function () { return { labels: [] }; }),
        // WHO WORK CAN BE HANDED TO (kanban t_5f314e9a). Active members of this account only — the
        // same roster the Team screen lists, so a removed teammate disappears from every picker.
        api().get('/private-email/members').catch(function () { return { members: [] }; })
      ]);
      S.boxes = Array.isArray(results[0]) ? results[0] : (results[0].boxes || []);
      S.domains = Array.isArray(results[1]) ? results[1] : (results[1].domains || []);
      S.messages = (results[2] && results[2].messages) || [];
      S.counts = results[3] || {};
      S.addresses = (results[4] && results[4].addresses) || [];
      S.miscTotal = (results[4] && results[4].misc && results[4].misc.total) || 0;
      S.allTotal = (results[4] && results[4].all && results[4].all.total != null) ? results[4].all.total : null;
      var rail = results[5] || {};
      S.folders = {
        system: Array.isArray(rail.system) ? rail.system : [],
        custom: Array.isArray(rail.custom) ? rail.custom : []
      };
      S.status = results[6] || null;
      S.labels = (results[7] && Array.isArray(results[7].labels)) ? results[7].labels : [];
      S.members = (results[8] && Array.isArray(results[8].members)) ? results[8].members : [];
      // A label that was deleted elsewhere (or never existed) must not keep filtering the list to
      // an empty page: drop the filter when it names a label this account no longer has.
      if (S.label && !S.labels.some(function (l) { return l.id === S.label; })) S.label = '';
      S.err = null;
      // The records answer for EVERY domain that can receive — read through the app's own endpoint
      // (the provider's values, never a hardcoded DKIM) and kept per domain. Both halves need it:
      // the first-run checklist renders it, and the Mail tab's "+ Create new inbox" gates on it, so
      // the button and the sentence beside it can never be reading two different facts.
      var recDomains = (S.domains || []).filter(function (d) {
        return d.inbound_provider === 'mailgun' || d.inbound_provider === 'sendgrid';
      });
      var recResults = await Promise.all(recDomains.map(function (d) {
        return api().get('/private-email/domains/' + d.id + '/records').then(
          function (r) { return { id: String(d.id), rec: r }; },
          function (e) { return { id: String(d.id), err: fail(e) }; }
        );
      }));
      S.ready = {};
      S.readyErr = {};
      recResults.forEach(function (x) {
        if (x.rec) S.ready[x.id] = x.rec; else S.readyErr[x.id] = x.err;
      });
      var firstId = S.domains.length ? String(S.domains[0].id) : null;
      S.records = firstId ? (S.ready[firstId] || null) : null;
      S.recordsErr = firstId ? (S.readyErr[firstId] || null) : null;
    } catch (e) {
      S.err = fail(e);
      S.messages = [];
    }
    paint();
  }

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Inbox actions
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  window.peLogin = function (base, address) {
    // `base` is the mailbox's own branded name when it is live, otherwise the shared mail host.
    // Either way the chosen address travels in the query string, so the client opens THAT mailbox
    // instead of a chooser the user already answered.
    var host = (typeof base === 'string' && base.indexOf('http') === 0) ? base : MAIL_HOST;
    var url = host + '/?address=' + encodeURIComponent(address || '');
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

  // Show exactly the fields the two choices need, and say what the receiving choice means. A field
  // that is on screen for a combination that cannot use it is how a setup page lies — so sending /
  // receiving drive visibility here, and the server refuses a combination it cannot honour.
  window.peSetupChoiceChanged = function () {
    var sEl = document.getElementById('peSetupSending');
    var rEl = document.getElementById('peSetupReceiving');
    if (!sEl || !rEl) return; // the saved-domain view has no add form
    var sending = sEl.value || 'mailgun';
    var receiving = rEl.value || 'mailgun';
    var show = function (id, on) { var el = document.getElementById(id); if (el) el.style.display = on ? '' : 'none'; };
    show('peSetupSmtpBox', sending === 'smtp');
    show('peSetupMailgunBox', sending === 'mailgun' || receiving === 'mailgun');
    show('peSetupHookBox', receiving === 'mailgun');
    var note = document.getElementById('peSetupRecvNote');
    if (note) {
      if (receiving === 'sendgrid') note.textContent = 'SendGrid posts every message it receives for you to an address we give you on the next step. No extra key is needed — we show you the exact URL to paste into SendGrid → Settings → Inbound Parse.';
      else if (receiving === 'none') note.textContent = 'Mail addressed to this domain will not arrive in CoreSwift — you keep receiving it wherever it goes today. Sending (and everything else) still works.';
      else note.textContent = 'Mailgun delivers your mail to us. You will need its webhook signing key so we can prove each message really came from your account.';
    }
  };

  // Switch an EXISTING domain's receiving provider. Sending is untouched: the two are separate
  // choices, and this is the one the customer can change without re-entering their sending keys.
  window.peSetupChangeReceiving = async function (value) {
    var d = (S.domains || [])[0];
    if (!d) return;
    S.setupErr = null;
    try {
      await api().patch('/private-email/domains/' + d.id, { receiving_provider: value });
      toastMsg('Receiving is now ' + (value === 'none' ? 'off for this domain' : value), 'success');
      load();
    } catch (e) {
      S.setupErr = fail(e);
      render();
    }
  };

  window.peSetupAddDomain = async function () {
    if (S.busy) return;
    var val = function (id) { var el = document.getElementById(id); return el ? String(el.value || '').trim() : ''; };
    var domain = val('peSetupDomain').toLowerCase().replace(/^https?:\/\//, '').replace(/\/.*$/, '');
    var sending = val('peSetupSending') || 'mailgun';
    var receiving = val('peSetupReceiving') || 'mailgun';
    var key = val('peSetupKey');
    var hook = val('peSetupHook');
    S.setupErr = null;
    if (!domain || domain.indexOf('.') < 0) {
      S.setupErr = 'Enter the domain you own, for example yourbusiness.com.';
      load(); return;
    }
    var needsMailgunKey = (sending === 'mailgun' || receiving === 'mailgun');
    if (needsMailgunKey && !key) {
      S.setupErr = sending === 'mailgun'
        ? 'Paste the Mailgun API key for this domain — it is what lets us send its mail.'
        : 'Receiving through Mailgun needs the Mailgun API key for this domain — it is what lets us show its records and check them.';
      load(); return;
    }
    if (sending === 'smtp' && (!val('peSetupSmtpHost') || !val('peSetupSmtpUser') || !val('peSetupSmtpPass'))) {
      S.setupErr = 'Fill in your mail server, username and password so we can send through it.';
      load(); return;
    }
    if (receiving === 'mailgun' && !hook) {
      S.setupErr = 'Paste the Mailgun webhook signing key — without it every incoming message is refused.';
      load(); return;
    }
    var payload = {
      domain: domain,
      sending_provider: sending,
      receiving_provider: receiving
    };
    if (needsMailgunKey) {
      payload.mailgun_api_key = key;
      payload.mailgun_region = val('peSetupRegion') || 'us';
    }
    if (receiving === 'mailgun') payload.webhook_signing_key = hook;
    if (sending === 'smtp') {
      payload.smtp_host = val('peSetupSmtpHost');
      payload.smtp_port = parseInt(val('peSetupSmtpPort') || '587', 10) || 587;
      payload.smtp_username = val('peSetupSmtpUser');
      payload.smtp_password = val('peSetupSmtpPass');
      var tlsEl = document.getElementById('peSetupSmtpTls');
      payload.smtp_tls = !tlsEl || !!tlsEl.checked;
    }
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
    // The domains the button was enabled for, by the same rule — never a second definition of
    // "ready", or the modal refuses a domain the page has just invited (kanban t_54b5d21b).
    var verified = S.domains.filter(canCreateOn);
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
      + '<p style="font-size:12px;opacity:.7;margin:6px 0 0">Common ones: <strong>info@</strong> for enquiries, <strong>support@</strong> for customers who need help, and your own name. ' + mailboxAllowanceSentence() + '</p></div>'
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
    // Choosing a folder leaves a label filter: otherwise the heading would say one thing and the
    // rows would still be filtered by a label picked earlier.
    S.label = '';
    var d = document.getElementById('peDetail'); if (d) d.innerHTML = '';
    load();
  };

  // Pick a label to filter by (or pass '' to clear it). The label is a server-side filter, so the
  // rows and the number beside the label row come from the same place.
  window.peSetLabel = function (id) {
    var want = id || '';
    // Remember the folder the user was in, so clearing the label puts them back where they were
    // instead of dumping them into the All view.
    if (want && !S.label) S.prevView = S.view;
    S.label = want;
    S.view = want ? 'all' : (S.prevView || 'inbox');
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
  // Folder menus — the bulk actions the server already serves (B205)
  //
  // Every item here posts to a route that exists: POST /folders (create a subfolder) and
  // POST /folders/action (read | unread | empty | to_inbox | trash_all). The destructive ones say
  // the real count first and report what the server did afterwards, so "Empty folder" is a counted
  // act rather than a promise. Only real STORES get a menu; Unread and Favorites are views.
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  function allFolders() { return (S.folders.system || []).concat(S.folders.custom || []); }

  function paint() {
    var el = document.getElementById('peWrap');
    if (!el) return;
    el.innerHTML = render();
    // Re-apply the two choices' field visibility after every paint: the selects are re-created by the
    // markup above, so a hidden field must not reappear as a visible one on a re-render.
    if (typeof window.peSetupChoiceChanged === 'function') window.peSetupChoiceChanged();
    Array.prototype.forEach.call(document.querySelectorAll('.pe-row'), function (r) {
      r.onclick = function () { window.peOpenMessage(r.getAttribute('data-id')); };
    });
  }

  window.peFolderMenu = function (folder, isCustom) {
    var f = allFolders().filter(function (x) { return x.id === folder; })[0]
      || { id: folder, name: folder, count: 0 };
    var isArchive = folder === 'archive';
    var isHolding = folder === 'spam' || folder === 'trash';
    var items = [
      ['sub', 'Create subfolder', 'Make a folder inside ' + f.name + ' and file mail into it'],
      ['read', 'Mark all as read', 'Every message in ' + f.name + ' stops showing as new'],
      ['unread', 'Mark all as unread', 'Puts every message in ' + f.name + ' back as new'],
      ['sort', S.sort === 'alpha' ? 'Sort newest first' : 'Sort alphabetically',
        S.sort === 'alpha' ? 'Go back to the order mail arrived' : 'Order this list by subject']
    ];
    if (isArchive) {
      items.push(['to_inbox', 'Move all to Inbox', 'Puts every archived message back in the Inbox']);
      items.push(['trash_all', 'Delete all messages', 'Moves every archived message to Trash — restorable from there']);
    }
    if (isHolding) {
      items.push(['empty', 'Empty ' + f.name, 'Destroys ' + Number(f.count || 0) + ' message'
        + (Number(f.count || 0) === 1 ? '' : 's') + ' for good. This cannot be undone.']);
    }
    var body = '<p style="margin:0 0 10px;opacity:.8">Actions for <strong>' + esc(f.name) + '</strong>'
      + (Number(f.count || 0) ? ' · ' + Number(f.count || 0) + ' message' + (Number(f.count || 0) === 1 ? '' : 's') : '')
      + '</p><div style="display:flex;flex-direction:column;gap:6px">'
      + items.map(function (it) {
        return '<button type="button" class="btn btn-ghost" style="text-align:left" data-fa="' + esc(it[0]) + '">'
          + '<strong>' + esc(it[1]) + '</strong>'
          + '<div style="font-size:12px;opacity:.7;font-weight:400">' + esc(it[2]) + '</div></button>';
      }).join('') + '</div>';
    window.showModal('Folder actions', body, '<button class="btn btn-secondary" onclick="closeModal()">Close</button>');
    Array.prototype.forEach.call(document.querySelectorAll('[data-fa]'), function (b) {
      b.onclick = function () { window.peFolderDo(folder, b.getAttribute('data-fa')); };
    });
  };

  window.peFolderDo = async function (folder, action) {
    if (action === 'sort') {
      S.sort = (S.sort === 'alpha') ? 'date' : 'alpha';
      window.closeModal();
      paint();
      return;
    }
    if (action === 'sub') { window.peSubfolderModal(folder); return; }
    if (action === 'empty') {
      var f = allFolders().filter(function (x) { return x.id === folder; })[0] || {};
      var n = Number(f.count || 0);
      var where = (f.name || folder);
      if (!window.confirm('Empty ' + where + '?\n\nThis destroys ' + n + ' message' + (n === 1 ? '' : 's')
        + ' for good. It cannot be undone.\n\n' + (folder === 'spam'
          ? 'Spam mail is never moved to Trash — it is deleted on the spot.'
          : 'Trash mail is deleted on the spot.'))) return;
      try {
        var r = await api().post('/private-email/folders/action', { folder: folder, action: 'empty' });
        window.closeModal();
        toastMsg('Destroyed ' + Number(r.destroyed || 0) + ' message' + (Number(r.destroyed || 0) === 1 ? '' : 's') + ' from ' + where, 'success');
        load();
      } catch (e) { toastMsg(fail(e), 'error'); }
      return;
    }
    try {
      var res = await api().post('/private-email/folders/action', { folder: folder, action: action });
      window.closeModal();
      if (action === 'read' || action === 'unread') {
        toastMsg('Marked ' + Number(res.updated || 0) + ' message' + (Number(res.updated || 0) === 1 ? '' : 's') + ' as ' + action, 'success');
      } else if (action === 'to_inbox' || action === 'trash_all') {
        toastMsg('Moved ' + Number(res.moved || 0) + ' message' + (Number(res.moved || 0) === 1 ? '' : 's') + ' to ' + (res.to === 'trash' ? 'Trash' : 'Inbox'), 'success');
      }
      if (S.view === folder) { /* stay in the folder the user is looking at */ }
      load();
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  window.peSubfolderModal = function (folder) {
    window.showModal('Create a subfolder',
      '<div class="form-group"><label>Folder name</label>'
      + '<input id="peSubName" placeholder="clients" style="width:100%">'
      + '<p style="font-size:12px;opacity:.7;margin:6px 0 0">It appears under your folders in the rail. '
      + 'Mail can be moved into it from any message.</p></div>'
      + '<div id="peSubMsg" style="font-size:13px"></div>',
      '<button class="btn btn-secondary" onclick="closeModal()">Cancel</button>'
      + '<button class="btn btn-primary" onclick="peSubfolderCreate(\'' + esc(folder) + '\')">Create</button>');
  };

  window.peSubfolderCreate = async function (folder) {
    var inp = document.getElementById('peSubName');
    var msg = document.getElementById('peSubMsg');
    var name = ((inp && inp.value) || '').trim();
    if (!name) { if (msg) msg.textContent = 'Give the folder a name.'; return; }
    if (msg) msg.textContent = 'Creating…';
    try {
      await api().post('/private-email/folders', { parent: folder, name: name });
      window.closeModal();
      toastMsg('Folder created', 'success');
      load();
    } catch (e) { if (msg) msg.textContent = fail(e); else toastMsg(fail(e), 'error'); }
  };

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Labels — name + colour, per account: GET/POST /labels, PATCH/DELETE /labels/:id, and
  // POST|DELETE /labels/:id/apply with message_ids. The rail's label rows filter the list through
  // the server's own `label` parameter, so a label never becomes a client-side decoration.
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  function labelById(id) { return (S.labels || []).filter(function (l) { return l.id === id; })[0]; }

  function labelChipsHtml(m) {
    var applied = (m && m.labels) || [];
    if (!applied.length) {
      return '<span style="font-size:12px;opacity:.6">No labels on this message yet.</span>';
    }
    return applied.map(function (l) {
      return '<span style="display:inline-flex;align-items:center;gap:6px;background:rgba(128,128,128,.14);'
        + 'border-radius:999px;padding:2px 8px;margin:0 6px 6px 0;font-size:12px">'
        + '<i style="display:inline-block;width:8px;height:8px;border-radius:999px;background:' + esc(l.color || '#64748b') + '"></i>'
        + esc(l.name)
        + '<button type="button" class="btn btn-ghost btn-xs" style="padding:0 2px" title="Remove this label"'
        + ' onclick="peLabelToggle(\'' + esc(m.id) + '\',\'' + esc(l.id) + '\',false)">&#215;</button></span>';
    }).join('');
  }

  function labelPickerHtml(m) {
    var applied = ((m && m.labels) || []).map(function (l) { return l.id; });
    var free = (S.labels || []).filter(function (l) { return applied.indexOf(l.id) < 0; });
    var opts = '<option value="">Add a label…</option>' + free.map(function (l) {
      return '<option value="' + esc(l.id) + '">' + esc(l.name) + '</option>';
    }).join('');
    return '<div style="display:flex;gap:6px;align-items:center;margin-top:6px">'
      + '<select id="peLabelPick" style="width:auto" onchange="peLabelApply(this.value)">' + opts + '</select>'
      + '<button type="button" class="btn btn-ghost btn-xs" onclick="peLabelNew()">+ New label</button></div>';
  }

  window.peLabelApply = async function (labelId) {
    if (!labelId || !S.sel) return;
    try {
      await api().post('/private-email/labels/' + labelId + '/apply', { message_ids: [S.sel.id] });
      var id = S.sel.id;
      await load();
      window.peOpenMessage(id);
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  window.peLabelToggle = async function (messageId, labelId, want) {
    try {
      if (want) {
        await api().post('/private-email/labels/' + labelId + '/apply', { message_ids: [messageId] });
      } else {
        // DELETE with a body: the route takes { message_ids } and API.call forwards a body on any
        // verb, while API.del(path) sends none.
        await api().call('DELETE', '/private-email/labels/' + labelId + '/apply', { message_ids: [messageId] });
      }
      await load();
      window.peOpenMessage(messageId);
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  window.peLabelNew = function () { window.peLabelForm(null); };

  window.peLabelForm = function (label) {
    var editing = !!label;
    window.showModal(editing ? 'Edit label' : 'New label',
      '<div class="form-group"><label>Name</label>'
      + '<input id="peLabelName" maxlength="64" placeholder="Urgent" value="' + esc(editing ? label.name : '') + '"></div>'
      + '<div class="form-group"><label>Colour</label>'
      + '<input id="peLabelColor" type="color" value="' + esc(editing && label.color ? label.color : '#64748b') + '" style="width:80px;padding:2px"></div>'
      + '<div id="peLabelMsg" style="font-size:13px"></div>',
      '<button class="btn btn-secondary" onclick="closeModal()">Cancel</button>'
      + '<button class="btn btn-primary" onclick="peLabelSave(' + (editing ? '\'' + esc(label.id) + '\'' : 'null') + ')">'
      + (editing ? 'Save' : 'Create label') + '</button>');
  };

  window.peLabelSave = async function (labelId) {
    var nameEl = document.getElementById('peLabelName');
    var colorEl = document.getElementById('peLabelColor');
    var msg = document.getElementById('peLabelMsg');
    var name = ((nameEl && nameEl.value) || '').trim();
    var color = (colorEl && colorEl.value) || '#64748b';
    if (!name) { if (msg) msg.textContent = 'Give the label a name.'; return; }
    if (msg) msg.textContent = 'Saving…';
    try {
      if (labelId) {
        await api().patch('/private-email/labels/' + labelId, { name: name, color: color });
      } else {
        await api().post('/private-email/labels', { name: name, color: color });
      }
      window.closeModal();
      toastMsg(labelId ? 'Label saved' : 'Label created', 'success');
      await load();
      if (S.sel) window.peOpenMessage(S.sel.id);
    } catch (e) { if (msg) msg.textContent = fail(e); }
  };

  window.peLabelMenu = function (id) {
    var l = labelById(id);
    if (!l) return;
    window.showModal('Label: ' + l.name,
      '<p style="margin:0 0 10px;opacity:.8">' + Number(l.used || 0) + ' message'
      + (Number(l.used || 0) === 1 ? '' : 's') + ' carry this label.</p>'
      + '<div style="display:flex;flex-direction:column;gap:6px">'
      + '<button type="button" class="btn btn-ghost" style="text-align:left" onclick="closeModal();peLabelFormById(\'' + esc(id) + '\')">'
      + '<strong>Edit label</strong><div style="font-size:12px;opacity:.7;font-weight:400">Rename it or change its colour</div></button>'
      + '<button type="button" class="btn btn-ghost" style="text-align:left;color:#b91c1c" onclick="peLabelDelete(\'' + esc(id) + '\')">'
      + '<strong>Delete label</strong><div style="font-size:12px;opacity:.7;font-weight:400">The mail stays; only the tag goes away</div></button>'
      + '</div>',
      '<button class="btn btn-secondary" onclick="closeModal()">Close</button>');
  };

  window.peLabelFormById = function (id) {
    var l = labelById(id);
    if (l) window.peLabelForm(l);
  };

  window.peLabelDelete = async function (id) {
    var l = labelById(id) || {};
    if (!window.confirm('Delete the label "' + (l.name || '') + '"?\n\n'
      + Number(l.used || 0) + ' message' + (Number(l.used || 0) === 1 ? '' : 's')
      + ' keep their mail — only the tag is removed.')) return;
    try {
      await api().del('/private-email/labels/' + id);
      window.closeModal();
      if (S.label === id) { S.label = ''; S.view = 'inbox'; }
      toastMsg('Label deleted', 'success');
      await load();
      if (S.sel) window.peOpenMessage(S.sel.id);
    } catch (e) { toastMsg(fail(e), 'error'); }
  };

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Summarise — POST /messages/:id/summarize. The server refuses with a plain sentence when the
  // account has no AI key, and that sentence is shown where the button is; the button itself is
  // only rendered when /counts says a key exists.
  // ─────────────────────────────────────────────────────────────────────────────────────────────

  window.peSummarize = async function (id) {
    var box = document.getElementById('peSummaryBox');
    var btn = document.getElementById('peSumBtn');
    if (box) box.innerHTML = '<span style="opacity:.7">Summarising…</span>';
    if (btn) btn.disabled = true;
    try {
      var r = await api().post('/private-email/messages/' + id + '/summarize', {});
      if (box) {
        box.innerHTML = '<div style="padding:8px 10px;border-left:3px solid #93c5fd;background:rgba(147,197,253,.12);'
          + 'white-space:pre-wrap">' + esc(r.summary || '(no summary returned)') + '</div>';
      }
    } catch (e) {
      if (box) box.innerHTML = '<div style="padding:8px 10px;border-left:3px solid #f59e0b;background:rgba(245,158,11,.12)">'
        + esc(fail(e)) + '</div>';
    } finally {
      if (btn) btn.disabled = false;
    }
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

  // ─────────────────────────────────────────────────────────────────────────────────────────────
  // Assign (kanban t_5f314e9a) — ONE writer for both objects.
  //
  // `box` writes `private_email_boxes.user_id` (who looks after an address) and `message` writes
  // `private_email_messages.assigned_user_id` (who owns a conversation) — the same teammate, the same
  // roster, and the conversation's answer is also what the ticket queue reads through its ticket
  // link. `''` (the "Nobody" option) is sent as an explicit `null`, which the server treats as "give
  // it back to the workspace" rather than "leave it alone".
  // ─────────────────────────────────────────────────────────────────────────────────────────────
  window.peAssign = async function (kind, id, userId) {
    var who = userId ? memberLabel(memberById(userId)) : '';
    try {
      if (kind === 'box') {
        await api().patch('/private-email/boxes/' + id, { user_id: userId || null });
        toastMsg(userId ? 'This inbox is ' + who + "'s now" : 'This inbox is back with the whole workspace', 'success');
        await load();
        return;
      }
      await api().patch('/private-email/messages/' + id, { assigned_user_id: userId || null });
      toastMsg(userId ? 'Assigned to ' + who : 'Back with the whole workspace', 'success');
      await load();
      window.peOpenMessage(id);
    } catch (e) {
      toastMsg(fail(e), 'error');
      // Repaint from the server so a refused assignment cannot leave the control showing a value
      // the database never took.
      await load();
    }
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
        + '<button class="btn btn-ghost btn-xs" onclick="peDeleteMsg(\'' + esc(m.id) + '\',\'' + esc(m.folder || '') + '\')">'
        + ((m.folder === 'spam' || m.folder === 'trash') ? 'Delete forever' : 'Delete') + '</button>'
        + '</div></div>'
        + (m.is_ticket
          ? '<div style="margin:10px 0;padding:6px 10px;border-radius:6px;background:#dcfce7;color:#166534;font-size:13px">In the work queue as a ticket.</div>'
          : '<div style="margin:10px 0;padding:6px 10px;border-radius:6px;background:#fef3c7;color:#92400e;font-size:13px;display:flex;justify-content:space-between;align-items:center">'
            + '<span>In the inbox only — nobody is working this yet.</span>'
            + '<button class="btn btn-primary btn-xs" onclick="peMakeTicket(\'' + esc(m.id) + '\')">Turn into a ticket</button></div>')
        + '<div style="margin:10px 0;padding:8px 10px;border:1px solid rgba(128,128,128,.25);border-radius:6px;display:flex;gap:10px;align-items:center;flex-wrap:wrap">'
        + '<div style="font-size:12px;opacity:.7">Assigned to</div>'
        + assignSelectHtml('message', m.id, m.assigned_user_id)
        + '<div style="font-size:12px;opacity:.7">'
        + (m.assigned_user_id
          ? 'This thread is ' + esc(memberLabel(memberById(m.assigned_user_id))) + '\'s. The ticket queue shows the same person.'
          : 'Nobody owns this thread yet — the whole workspace can see it in the shared inbox.')
        + '</div></div>'
        + '<div style="margin:10px 0;padding:8px 10px;border:1px solid rgba(128,128,128,.25);border-radius:6px">'
        + '<div style="font-size:12px;opacity:.7;margin-bottom:6px">Labels</div>'
        + '<div>' + labelChipsHtml(m) + '</div>'
        + labelPickerHtml(m)
        + '</div>'
        + (S.counts.ai_summary
          ? '<div style="margin:10px 0"><button class="btn btn-secondary btn-sm" id="peSumBtn" onclick="peSummarize(\'' + esc(m.id) + '\')">Summarise this message</button>'
            + '<div id="peSummaryBox" style="margin-top:8px;font-size:13px"></div></div>'
          : '')
        + '<div style="white-space:pre-wrap;margin:12px 0;max-height:420px;overflow:auto">' + esc(m.body_plain || '(no text)') + '</div>'
        + '<label style="display:block;font-size:12px;opacity:.75;margin-bottom:4px">Reply as</label>'
        + '<select id="peFrom" style="margin-bottom:8px">' + fromOpts + '</select>'
        + '<textarea id="peText" rows="4" placeholder="Write your reply…" style="width:100%;margin-bottom:8px"></textarea>'
        // The signature is added by the server on every reply (kanban t_b671a0f7), exactly as Compose
        // does. The box starts ticked so the writer sees the truth, and unticking it drops the
        // signature for this one reply.
        + '<label style="display:flex;align-items:flex-start;gap:8px;font-size:13px;margin-bottom:8px;opacity:.9">'
        + '<input type="checkbox" id="peSig" checked style="width:auto;margin-top:2px">'
        + '<span>Add my signature <span style="opacity:.7">(your logo and details, set per address in the mail client)</span></span></label>'
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
    var sigOn = document.getElementById('peSig');
    try {
      await api().post('/private-email/messages/' + id + '/reply', {
        from_address: from,
        body: body,
        include_signature: sigOn ? !!sigOn.checked : true
      });
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
