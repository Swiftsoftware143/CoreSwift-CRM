/*
 * CoreSwift Mail — the mail client (B205 / B206 / B210 / B211).
 *
 * WHY IT LIVES ON ITS OWN HOST
 * David's decision (B211): the private-mail client is its own front door, inbox.<domain>, while the
 * CRM keeps ticketing. Both read the SAME endpoints and the SAME database — this file is a
 * presentation layer, never a second backend.
 *
 * WHAT IT MUST DO, in his words:
 *   setup page (plain English, numbered, copy buttons) -> LOG IN -> choose a mailbox -> inbox;
 *   folders Inbox/Unread/Favorites/Drafts/Sent/Archive/Spam/Trash with three-dot menus that differ
 *   per folder; labels with a colour and a three-dot menu; Compose with a From selector; a reader
 *   with reply/reply-all/forward/star/archive/delete/assign/summarise; an address filter including
 *   MISC; and mail is ACCOUNT-level, never workspace-level.
 *
 * NO OPTION APPEARS UNLESS IT WORKS. Anything the server cannot do (AI summaries with no provider
 * key, an inbox on a domain that is not receiving yet) is not drawn as a live control.
 */
(function () {
  'use strict';

  var TOKEN_KEY = 'csmail_token';
  var S = {
    token: localStorage.getItem(TOKEN_KEY) || '',
    me: null,
    boxes: [], domains: [], folders: { system: [], custom: [] }, counts: {},
    labels: [], messages: [], addresses: [], misc: 0, members: [],
    folder: 'inbox', address: 'all', label: '', sel: null, view: 'mail', err: '', busy: false
  };

  var $ = function (id) { return document.getElementById(id); };
  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function fmt(d) {
    if (!d) return '';
    try {
      var t = new Date(d), now = new Date();
      if (t.toDateString() === now.toDateString()) return t.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
      return t.toLocaleDateString([], { month: 'short', day: 'numeric' });
    } catch (e) { return String(d); }
  }
  function bytes(n) {
    n = Number(n || 0);
    if (n >= 1073741824) return (n / 1073741824).toFixed(1) + ' GB';
    if (n >= 1048576) return (n / 1048576).toFixed(1) + ' MB';
    if (n >= 1024) return (n / 1024).toFixed(0) + ' KB';
    return n + ' B';
  }
  function fail(e) { return (e && (e.message || e.error)) || String(e); }
  function toast(msg, kind) {
    var t = document.createElement('div');
    t.textContent = msg;
    t.style.cssText = 'position:fixed;left:50%;transform:translateX(-50%);bottom:22px;z-index:99;padding:10px 16px;border-radius:10px;'
      + 'font-size:13px;color:#fff;background:' + (kind === 'error' ? '#b91c1c' : '#111827') + ';box-shadow:0 6px 24px rgba(0,0,0,.18)';
    document.body.appendChild(t);
    setTimeout(function () { t.remove(); }, 3600);
  }

  // ── API ────────────────────────────────────────────────────────────────────────────────────────
  async function api(method, path, body) {
    var headers = { 'Content-Type': 'application/json' };
    if (S.token) headers.Authorization = 'Bearer ' + S.token;
    var res = await fetch('/api' + path, {
      method: method, headers: headers, body: body ? JSON.stringify(body) : undefined
    });
    var text = await res.text();
    var data = null;
    try { data = text ? JSON.parse(text) : null; } catch (e) { data = { message: text }; }
    if (res.status === 401) {
      signOut(true);
      throw new Error('Your session ended. Sign in again.');
    }
    if (!res.ok) {
      var msg = (data && (data.message || data.error)) || ('Request failed (' + res.status + ')');
      throw new Error(msg);
    }
    return data;
  }

  // ── Auth ──────────────────────────────────────────────────────────────────────────────────────
  function signOut(quiet) {
    S.token = ''; localStorage.removeItem(TOKEN_KEY);
    S.me = null; S.boxes = []; S.domains = []; S.messages = []; S.sel = null; S.view = 'setup';
    render();
    if (!quiet) toast('Signed out');
  }

  async function doLogin(email, password) {
    var r = await api('POST', '/auth/login', { email: email, password: password });
    S.token = r.access_token;
    localStorage.setItem(TOKEN_KEY, S.token);
    S.me = r.team_member || null;
    await boot();
  }

  // ── Boot / data ───────────────────────────────────────────────────────────────────────────────
  async function boot() {
    try {
      var rs = await Promise.all([
        api('GET', '/private-email/boxes').catch(function () { return []; }),
        api('GET', '/private-email/domains').catch(function () { return []; }),
        api('GET', '/private-email/folders').catch(function () { return { system: [], custom: [] }; }),
        api('GET', '/private-email/messages/counts').catch(function () { return {}; }),
        api('GET', '/private-email/labels').catch(function () { return { labels: [] }; }),
        api('GET', '/private-email/members').catch(function () { return { members: [] }; }),
        api('GET', '/private-email/addresses').catch(function () { return {}; })
      ]);
      S.boxes = Array.isArray(rs[0]) ? rs[0] : (rs[0].boxes || []);
      S.domains = Array.isArray(rs[1]) ? rs[1] : (rs[1].domains || []);
      S.folders = rs[2] || { system: [], custom: [] };
      S.counts = rs[3] || {};
      S.labels = (rs[4] && rs[4].labels) || [];
      S.members = (rs[5] && rs[5].members) || [];
      S.addresses = (rs[6] && rs[6].addresses) || [];
      S.misc = (rs[6] && rs[6].misc && rs[6].misc.total) || 0;
      S.err = '';
      // A mailbox chosen from the CRM's LOG IN button arrives as ?address=. With no inboxes at all
      // there is nothing to read, so the user lands on the setup page instead.
      if (!S.boxes.length) S.view = 'setup';
      else if (S.view !== 'mail') S.view = 'mail';
      if (S.view === 'mail') await loadMessages();
    } catch (e) {
      S.err = fail(e);
      S.view = 'setup';
    }
    render();
  }

  // Every view the rail offers is one GET: the server's eight own views, plus the path of a folder
  // this account made — a made path is a view of its own (the exact path and everything filed below
  // it), so nothing is filtered here. Before CoreSwift's message query learned that, this client
  // paged the widest view and kept the rows sitting in the path, which capped a folder at 1,000
  // rows; that workaround went with the server fix (kanban t_d43ee278).
  async function loadMessages() {
    try {
      var q = '/private-email/messages?folder=' + encodeURIComponent(S.folder) + '&address=' + encodeURIComponent(S.address)
        + '&label=' + encodeURIComponent(S.label || '') + '&limit=100';
      var r = await api('GET', q);
      S.messages = (r && r.messages) || [];
    } catch (e) {
      S.messages = [];
      S.err = fail(e);
    }
  }

  // ── Render ───────────────────────────────────────────────────────────────────────────────────
  function render() {
    var app = $('app');
    if (!S.token) { app.innerHTML = setupPage(); bindSetup(); return; }
    if (S.view === 'setup') { app.innerHTML = setupPage(); bindSetup(); return; }
    app.innerHTML = mailPage();
    bindMail();
  }

  // Setup page: readable signed out (B205), and the place a signed-in account without mailboxes
  // finishes its setup.
  function setupPage() {
    var signedIn = !!S.token;
    var domain = S.domains[0] || null;
    var step3 = '';
    if (signedIn && domain) {
      step3 = '<div class="card"><h3>3. Add these records where your domain lives</h3>'
        + '<p style="color:var(--muted);margin-top:0">Your domain stays at your own provider. These exact values prove your mail is really yours. Press <strong>Show my records</strong>.</p>'
        + '<button class="btn" id="smRecords">Show my records</button> <span class="ok" id="smRecMsg"></span>'
        + '<div id="smRecList"></div></div>';
    } else {
      step3 = '<div class="card"><h3>3. Add these records where your domain lives</h3>'
        + '<p style="color:var(--muted);margin-top:0">Your domain stays at your own provider (Cloudflare, your registrar, anywhere). Two kinds of record are needed:</p>'
        + '<div class="rec"><strong>MX</strong><span><code>mxa.mailgun.org</code><br><span style="color:var(--muted)">Where the internet delivers your mail.</span></span><span></span></div>'
        + '<div class="rec"><strong>MX</strong><span><code>mxb.mailgun.org</code><br><span style="color:var(--muted)">The second delivery server, used if the first is busy.</span></span><span></span></div>'
        + '<div class="rec"><strong>TXT</strong><span><code>SPF and DKIM values</code> — shown to you after you add your domain, with a copy button beside each one.<br><span style="color:var(--muted)">SPF proves you are allowed to send; DKIM signs your mail so it is not marked as spam.</span></span><span></span></div></div>';
    }

    return '<div class="setup">'
      + '<div style="display:flex;justify-content:space-between;align-items:center;gap:12px">'
      + '<div><div style="font-weight:700;font-size:18px">CoreSwift Mail</div>'
      + '<div style="color:var(--muted);font-size:13px">Your business mail, read and answered here.</div></div>'
      + (signedIn
        ? '<div><button class="btn" id="smSignOut">Sign out</button> '
          + (S.boxes.length ? '<button class="btn btn-primary" id="smOpen">Open my mail</button>' : '') + '</div>'
        : '<button class="btn btn-primary" id="smLogin">Log In</button>')
      + '</div>'
      + (S.err ? '<div class="card err">' + esc(S.err) + '</div>' : '')
      + '<div class="card"><h3>How your mail gets here</h3>'
      + '<p style="margin:0;color:var(--muted)">Six short steps. Nothing here needs a technician, and nothing is charged until you have a mailbox that receives.</p></div>'
      + '<div class="card"><h3>1. Add your domain</h3>'
      + '<p style="color:var(--muted);margin-top:0">The domain you receive mail on — for example <strong>yourbusiness.com</strong>. It stays yours; we never take it over.</p>'
      + (signedIn
        ? (S.domains.length
          ? '<div>' + S.domains.map(function (d) {
            return '<div class="rec"><strong>' + esc(d.domain) + '</strong><span>'
              + (d.verified ? '<span class="ok">Active — receiving mail</span>' : '<span style="color:var(--warn)">Waiting for its records</span>')
              + '</span><button class="btn btn-ghost" onclick="csmCheckDomain(\'' + esc(d.id) + '\')">Check again</button></div>';
          }).join('') + '</div>'
          : '<label class="f">Your domain</label><input id="smDomain" placeholder="yourbusiness.com">'
            + '<label class="f">Your Mailgun key</label><input id="smKey" placeholder="key-…" type="password">'
            + '<div style="margin-top:10px"><button class="btn btn-primary" id="smAddDomain">Add my domain</button> <span id="smDomMsg" class="ok"></span></div>'
            + '<p style="color:var(--muted);font-size:12px">You will find the key in your Mailgun account under Settings → API keys. It is stored encrypted and used only to send and receive your own mail.</p>')
        : '<p style="color:var(--muted)">Press <strong>Log In</strong> to connect your domain.</p>')
      + '</div>'
      + '<div class="card"><h3>2. Paste your Mailgun key</h3>'
      + '<p style="margin:0;color:var(--muted)">Mailgun is the mail service that carries your messages. Your key stays yours: sending is charged to your own Mailgun account and nothing here shares it.</p></div>'
      + step3
      + '<div class="card"><h3>4. Wait until it shows Active, then press Check again</h3>'
      + '<p style="margin:0;color:var(--muted)">Records take a few minutes to spread. When your domain says <strong>Active</strong>, you can create mailboxes.</p></div>'
      + '<div class="card"><h3>5. Create your mailboxes</h3>'
      + '<p style="color:var(--muted);margin-top:0">An address people can write to:</p>'
      + '<ul style="margin:0 0 10px 18px;color:var(--muted)"><li><strong>info@</strong> — enquiries from the public</li>'
      + '<li><strong>support@</strong> — customers who need help (this one also opens a ticket in your CRM)</li>'
      + '<li>a personal one, such as your own name</li></ul>'
      + (signedIn && S.boxes.length
        ? '<div>' + S.boxes.map(function (b) {
          return '<div class="rec"><strong>' + esc(b.email_address) + '</strong><span>' + (b.creates_ticket ? 'also opens a ticket' : 'inbox only') + '</span><span></span></div>';
        }).join('') + '</div>'
        : '<p style="margin:0;color:var(--muted)">Mailboxes are created on your mail screen, once your domain is Active.</p>')
      + '</div>'
      + '<div class="card"><h3>6. Turn on catch-all if you want one inbox to receive every address</h3>'
      + '<p style="margin:0;color:var(--muted)">With catch-all on, mail to any address at your domain that you have not created still arrives — and it is shown under <strong>Everything else</strong> in the address filter so you can see it.</p>'
      + (signedIn && S.domains.length
        ? '<label style="display:flex;gap:8px;align-items:center;margin-top:10px;cursor:pointer"><input type="checkbox" id="smCatchAll" style="width:auto" '
          + (S.domains[0].catch_all_enabled ? 'checked' : '') + '> Catch-all is on for ' + esc(S.domains[0].domain) + '</label>'
        : '')
      + '</div>'
      + '<p style="color:var(--muted);font-size:12px">Mail is kept while you have storage (' + (S.counts.storage_gb || 2) + ' GB on this plan, upgraded with your plan). '
      + 'Past that, mail older than your plan\'s retention period is removed — never silently, and never to make room for new mail: incoming mail is always accepted.</p>'
      + '</div>';
  }

  function bindSetup() {
    var lg = $('smLogin');
    if (lg) lg.onclick = loginModal;
    var so = $('smSignOut');
    if (so) so.onclick = function () { signOut(); };
    var op = $('smOpen');
    if (op) op.onclick = function () { S.view = 'mail'; render(); };
    var ad = $('smAddDomain');
    if (ad) ad.onclick = async function () {
      var d = ($('smDomain') || {}).value || '';
      var k = ($('smKey') || {}).value || '';
      var m = $('smDomMsg');
      if (!d.trim()) { if (m) { m.className = 'err'; m.textContent = 'Type your domain first.'; } return; }
      if (m) { m.className = 'ok'; m.textContent = 'Adding…'; }
      try {
        await api('POST', '/private-email/domains', { domain: d.trim().toLowerCase(), mailgun_api_key: k.trim() });
        await boot();
      } catch (e) { if (m) { m.className = 'err'; m.textContent = fail(e); } }
    };
    var rb = $('smRecords');
    if (rb) rb.onclick = async function () {
      var m = $('smRecMsg'), box = $('smRecList');
      m.textContent = 'Reading from Mailgun…';
      try {
        var r = await api('GET', '/private-email/domains/' + S.domains[0].id + '/records');
        m.textContent = '';
        box.innerHTML = r.records.length
          ? r.records.map(function (x) {
            return '<div class="rec"><strong>' + esc(x.type) + '</strong><span><code>' + esc(x.name) + '</code><br>'
              + '<code>' + esc(x.value) + '</code>'
              + (x.priority != null ? ' <span style="color:var(--muted)">priority ' + esc(x.priority) + '</span>' : '')
              + '<br><span style="color:var(--muted)">' + esc(x.purpose) + '</span></span>'
              + '<button class="btn btn-ghost" onclick="csmCopy(this,\'' + esc(x.value) + '\')">Copy</button></div>';
          }).join('')
          : '<p class="err">Mailgun returned no records for this domain.</p>';
      } catch (e) { m.textContent = fail(e); }
    };
    var ca = $('smCatchAll');
    if (ca) ca.onchange = async function () {
      var want = this.checked, box = this;
      box.disabled = true;
      try { await api('PATCH', '/private-email/domains/' + S.domains[0].id, { catch_all_enabled: want }); toast('Saved'); }
      catch (e) { box.checked = !want; toast(fail(e), 'error'); }
      box.disabled = false;
    };
  }

  // A copy button that reports what it did — a silent clipboard write reads as a dead button.
  window.csmCopy = function (btn, text) {
    var done = function () { btn.textContent = 'Copied'; setTimeout(function () { btn.textContent = 'Copy'; }, 1600); };
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, function () { fallback(); });
    } else { fallback(); }
    function fallback() {
      var t = document.createElement('textarea');
      t.value = text; document.body.appendChild(t); t.select();
      try { document.execCommand('copy'); done(); } catch (e) { toast('Copy failed — select the value and copy it', 'error'); }
      t.remove();
    }
  };

  window.csmCheckDomain = async function (id) {
    try {
      var r = await api('POST', '/private-email/domains/' + id + '/verify', {});
      toast(r.verified ? 'Domain is active — receiving mail' : ('Still ' + (r.mailgun_state || 'waiting')), r.verified ? 'ok' : 'error');
      await boot();
    } catch (e) { toast(fail(e), 'error'); }
  };

  function loginModal() {
    modal('Log in', '<label class="f">Email</label><input id="lmEmail" autocomplete="username">'
      + '<label class="f">Password</label><input id="lmPass" type="password" autocomplete="current-password">'
      + '<div class="err" id="lmErr"></div>',
      '<button class="btn" onclick="csmCloseModal()">Cancel</button><button class="btn btn-primary" id="lmGo">Log In</button>',
      function () {
        $('lmGo').onclick = async function () {
          var btn = this; btn.disabled = true;
          try { await doLogin(($('lmEmail').value || '').trim(), $('lmPass').value || ''); closeModal(); }
          catch (e) { $('lmErr').textContent = fail(e); btn.disabled = false; }
        };
      });
  }

  // ── The mail screen ──────────────────────────────────────────────────────────────────────────
  function railHtml() {
    var sys = S.folders.system || [];
    var custom = S.folders.custom || [];
    var rows = sys.map(function (f) {
      return '<div class="folder' + (S.folder === f.id ? ' active' : '') + '" data-folder="' + esc(f.id) + '">'
        + '<span>' + esc(f.name) + '</span>'
        + '<span style="display:flex;gap:4px;align-items:center">'
        + (f.count ? '<span class="count">' + f.count + '</span>' : '')
        + '<button class="btn-ghost" data-menu="' + esc(f.id) + '" title="Folder actions">&#8942;</button>'
        + '</span></div>';
    }).join('');
    var customRows = custom.map(function (f) {
      return '<div class="folder' + (S.folder === f.path ? ' active' : '') + '" data-folder="' + esc(f.path) + '">'
        + '<span style="padding-left:12px">' + esc(f.name) + '</span>'
        + '<span class="count">' + (f.count || '') + '</span></div>';
    }).join('');
    var labelRows = S.labels.map(function (l) {
      return '<div class="folder' + (S.label === l.id ? ' active' : '') + '" data-label="' + esc(l.id) + '">'
        + '<span><i style="display:inline-block;width:9px;height:9px;border-radius:999px;margin-right:6px;background:' + esc(l.color) + '"></i>' + esc(l.name) + '</span>'
        + '<button class="btn-ghost" data-labelmenu="' + esc(l.id) + '" title="Label actions">&#8942;</button></div>';
    }).join('');

    var used = S.counts.used_bytes || 0, limit = S.counts.limit_bytes || 0;
    var pct = limit ? Math.min(100, Math.round(used / limit * 100)) : 0;

    return '<div class="rail">'
      + '<div class="brand">CoreSwift Mail</div>'
      + '<button class="btn btn-primary" id="mCompose" style="width:100%">Compose</button>'
      + '<div style="margin-top:12px">' + rows + '</div>'
      + '<h5>Folders you made</h5>' + (customRows || '<div style="color:var(--muted);font-size:12px;padding:0 9px">None yet — use a folder\'s menu.</div>')
      + '<h5>Labels <button class="btn-ghost" id="mNewLabel" title="Create label">+</button></h5>' + (labelRows || '<div style="color:var(--muted);font-size:12px;padding:0 9px">None yet.</div>')
      + '<div class="storage" style="margin-top:auto;padding-top:12px">'
      + '<div style="font-size:11px;color:var(--muted)">' + bytes(used) + ' of ' + bytes(limit) + ' used</div>'
      + '<div class="meter"><i style="width:' + pct + '%"></i></div>'
      + '<div style="font-size:11px;color:var(--muted)">Mail is kept while you have storage. Incoming mail is never refused.</div>'
      + '</div></div>';
  }

  function addressOptions() {
    var opts = ['<option value="all"' + (S.address === 'all' ? ' selected' : '') + '>All addresses</option>'];
    S.boxes.forEach(function (b) {
      opts.push('<option value="' + esc(b.email_address) + '"' + (S.address === b.email_address ? ' selected' : '') + '>' + esc(b.email_address) + '</option>');
    });
    if (S.misc > 0) opts.push('<option value="misc"' + (S.address === 'misc' ? ' selected' : '') + '>Everything else (MISC)</option>');
    return opts.join('');
  }

  function listHtml() {
    var msgs = S.messages || [];
    var head = '<div class="list-head">'
      + '<select id="mAddress" style="width:auto;flex:1">' + addressOptions() + '</select>'
      + '<button class="btn btn-ghost" id="mRefresh" title="Refresh">Refresh</button></div>';
    if (!msgs.length) {
      return head + '<div class="empty">No mail in this folder.<br><span style="font-size:12px">Every message that arrives appears here, including mail to addresses that do not open tickets.</span></div>';
    }
    var rows = msgs.map(function (m) {
      var chips = (m.is_ticket ? '<span class="chip ticket">ticket</span>' : '');
      return '<div class="row' + (m.is_read ? '' : ' unread') + (S.sel && S.sel.id === m.id ? ' sel' : '') + '" data-msg="' + esc(m.id) + '">'
        + '<span data-star="' + esc(m.id) + '" style="cursor:pointer;color:' + (m.is_starred ? '#b45309' : '#c7cbd1') + '">' + (m.is_starred ? '★' : '☆') + '</span>'
        + '<span style="min-width:0"><span class="subj">' + esc(m.subject || '(no subject)') + chips + '</span>'
        + '<span class="from">' + esc(m.from_name || m.from_address) + ' · ' + esc(m.address || '') + '</span></span>'
        + '<span class="when">' + esc(fmt(m.received_at)) + '</span></div>';
    }).join('');
    return head + '<div class="rows">' + rows + '</div>';
  }

  function readerHtml() {
    if (!S.sel) return '<div class="empty">No mail selected</div>';
    var m = S.sel;
    if (m.folder === 'drafts') {
      return '<div><h2>Draft</h2>'
        + '<div class="meta">To ' + esc(m.to_address || '(nobody yet)') + ' · ' + esc(fmt(m.received_at)) + '</div>'
        + '<div class="body">' + esc(m.body_plain || '(empty)') + '</div>'
        + '<div class="actions"><button class="btn btn-primary" data-act="continue">Continue writing</button>'
        + '<button class="btn btn-ghost" data-act="move">Move to…</button>'
        + '<button class="btn btn-ghost" data-act="delete">Discard</button></div></div>';
    }
    var assignOpts = '<option value="">Assign to…</option>' + S.members.map(function (u) {
      return '<option value="' + esc(u.id) + '"' + (m.assigned_user_id === u.id ? ' selected' : '') + '>' + esc(u.name || u.email) + '</option>';
    }).join('');
    var spam = m.folder === 'spam', trash = m.folder === 'trash';
    return '<div>'
      + '<h2>' + esc(m.subject || '(no subject)') + '</h2>'
      + '<div class="meta">From <strong>' + esc(m.from_name || m.from_address) + '</strong>'
      + ' &lt;' + esc(m.from_address) + '&gt; · to ' + esc(m.address || '')
      + ' · ' + esc(new Date(m.received_at).toLocaleString()) + '</div>'
      + '<div class="actions">'
      + '<button class="btn" data-act="reply">Reply</button>'
      + '<button class="btn" data-act="reply_all">Reply all</button>'
      + '<button class="btn" data-act="forward">Forward</button>'
      + '<button class="btn btn-ghost" data-act="star">' + (m.is_starred ? '★ Starred' : '☆ Star') + '</button>'
      + '<button class="btn btn-ghost" data-act="archive">Archive</button>'
      + '<button class="btn btn-ghost" data-act="move">Move to…</button>'
      + '<button class="btn btn-ghost" data-act="delete">' + (spam || trash ? 'Delete forever' : 'Delete') + '</button>'
      + (S.counts.ai_summary ? '<button class="btn btn-ghost" data-act="summarize">Summarise</button>' : '')
      + '<select id="mAssign" style="width:auto">' + assignOpts + '</select>'
      + '</div>'
      + (m.is_ticket
        ? '<div class="badge ticket"><span>In your CRM work queue as a ticket.</span></div>'
        : '<div class="badge open"><span>In the inbox only — nobody is working this yet.</span>'
          + '<button class="btn" data-act="ticket">Turn into a ticket</button></div>')
      + '<div id="mSummary"></div>'
      + '<div style="margin:6px 0 10px"><div style="font-size:11px;color:var(--muted);margin-bottom:4px">Labels</div><div id="mLabelBox"></div></div>'
      + '<div class="body">' + esc(m.body_plain || '(no text)') + '</div>'
      + '<div id="mReplyBox"></div>'
      + '</div>';
  }

  function mailPage() {
    return '<div class="mail">' + railHtml()
      + '<div class="main"><div class="list">' + listHtml() + '</div>'
      + '<div class="reader">' + readerHtml() + '</div></div></div>';
  }

  // ── Bindings ────────────────────────────────────────────────────────────────────────────────
  function bindMail() {
    var c = $('mCompose');
    // Called with the CLICK EVENT if passed directly, which made `mode` an Event and silently
    // suppressed the draft button; name the mode explicitly.
    if (c) c.onclick = function () { composeModal('new'); };
    var nl = $('mNewLabel');
    if (nl) nl.onclick = function () { labelModal(); };
    var ad = $('mAddress');
    if (ad) ad.onchange = async function () { S.address = this.value; await loadMessages(); render(); };
    var rf = $('mRefresh');
    if (rf) rf.onclick = async function () { await boot(); };

    Array.prototype.forEach.call(document.querySelectorAll('[data-folder]'), function (el) {
      el.onclick = function (e) {
        if (e.target.hasAttribute('data-menu')) return;
        S.folder = el.getAttribute('data-folder'); S.label = ''; S.sel = null;
        loadMessages().then(render);
      };
    });
    Array.prototype.forEach.call(document.querySelectorAll('[data-menu]'), function (el) {
      el.onclick = function (e) { e.stopPropagation(); folderMenu(el.getAttribute('data-menu')); };
    });
    Array.prototype.forEach.call(document.querySelectorAll('[data-labelmenu]'), function (el) {
      el.onclick = function (e) { e.stopPropagation(); labelMenu(el.getAttribute('data-labelmenu')); };
    });
    Array.prototype.forEach.call(document.querySelectorAll('[data-label]'), function (el) {
      el.onclick = function (e) {
        if (e.target.hasAttribute('data-labelmenu')) return;
        S.label = el.getAttribute('data-label'); S.sel = null;
        loadMessages().then(render);
      };
    });
    Array.prototype.forEach.call(document.querySelectorAll('[data-msg]'), function (el) {
      el.onclick = function (e) {
        if (e.target.hasAttribute('data-star')) return;
        openMessage(el.getAttribute('data-msg'));
      };
    });
    Array.prototype.forEach.call(document.querySelectorAll('[data-star]'), function (el) {
      el.onclick = async function (e) {
        e.stopPropagation();
        var id = el.getAttribute('data-star');
        var m = (S.messages.filter(function (x) { return x.id === id; })[0] || {});
        try { await api('PATCH', '/private-email/messages/' + id, { is_starred: !m.is_starred }); await boot(); }
        catch (err) { toast(fail(err), 'error'); }
      };
    });
    var sel = $('mAssign');
    if (sel) sel.onchange = async function () {
      if (!this.value || !S.sel) return;
      try {
        await api('PATCH', '/private-email/messages/' + S.sel.id, { assigned_user_id: this.value });
        toast('Assigned');
        openMessage(S.sel.id);
      } catch (e) { toast(fail(e), 'error'); }
    };
    Array.prototype.forEach.call(document.querySelectorAll('[data-act]'), function (b) {
      b.onclick = function () { messageAction(b.getAttribute('data-act')); };
    });
    if (S.sel) applyLabelControl();
  }

  async function openMessage(id) {
    try {
      S.sel = await api('GET', '/private-email/messages/' + id);
      // Reading it marks it read on the server; refresh the counts and the row without moving focus.
      api('GET', '/private-email/messages/counts').then(function (c) { S.counts = c || S.counts; }).catch(function () {});
      render();
    } catch (e) { toast(fail(e), 'error'); }
  }

  async function messageAction(act) {
    var m = S.sel;
    if (!m) return;
    try {
      if (act === 'star') { await api('PATCH', '/private-email/messages/' + m.id, { is_starred: !m.is_starred }); return openMessage(m.id); }
      if (act === 'archive') { await api('PATCH', '/private-email/messages/' + m.id, { folder: 'archive' }); S.sel = null; await boot(); toast('Archived'); return; }
      if (act === 'move') { moveModal(); return; }
      if (act === 'delete') {
        var forever = m.folder === 'spam' || m.folder === 'trash';
        if (!confirm(forever ? 'This message will be destroyed for good. Delete it?' : 'Move this message to Trash? It can be restored from there.')) return;
        await api('DELETE', '/private-email/messages/' + m.id);
        S.sel = null; await boot(); toast(forever ? 'Deleted' : 'Moved to Trash'); return;
      }
      if (act === 'ticket') {
        var t = await api('POST', '/private-email/messages/' + m.id + '/ticket', {});
        toast(t.created ? 'Ticket created' : 'This message already had a ticket');
        return openMessage(m.id);
      }
      if (act === 'summarize') {
        var box = $('mSummary');
        if (box) box.innerHTML = '<div class="badge open">Summarising…</div>';
        try {
          var s = await api('POST', '/private-email/messages/' + m.id + '/summarize', {});
          if (box) box.innerHTML = '<div class="badge" style="background:#eef2ff;color:#1e3a8a;display:block">'
            + '<strong>Summary</strong><div style="margin-top:4px;white-space:pre-wrap">' + esc(s.summary) + '</div></div>';
        } catch (e) { if (box) box.innerHTML = '<div class="badge open">' + esc(fail(e)) + '</div>'; }
        return;
      }
      if (act === 'continue') { composeModal('new'); return; }
      if (act === 'reply' || act === 'reply_all' || act === 'forward') { composeModal(act); return; }
    } catch (e) { toast(fail(e), 'error'); }
  }

  // ── Compose ─────────────────────────────────────────────────────────────────────────────────
  function composeModal(mode) {
    mode = mode || 'new';
    var m = S.sel;
    var fromOpts = S.boxes.map(function (b) {
      var sel = (m && m.address === b.email_address) || (!m && S.address !== 'all' && S.address === b.email_address);
      return '<option value="' + esc(b.email_address) + '"' + (sel ? ' selected' : '') + '>' + esc(b.email_address) + '</option>';
    }).join('');
    if (!fromOpts) { toast('Create an inbox before sending mail', 'error'); return; }

    var to = '', subject = '', body = '';
    if (mode === 'new') { subject = ''; }
    if (mode === 'reply' || mode === 'reply_all') {
      to = (m && m.from_address) || '';
      subject = /^re:/i.test((m && m.subject) || '') ? m.subject : 'Re: ' + ((m && m.subject) || '');
      body = '\n\n— ' + ((m && m.from_name) || '') + ' wrote:\n' + ((m && m.body_plain) || '');
    }
    if (mode === 'forward') {
      subject = 'Fwd: ' + ((m && m.subject) || '');
      body = '\n\n— forwarded —\nFrom: ' + ((m && m.from_address) || '') + '\n' + ((m && m.body_plain) || '');
    }

    modal(mode === 'new' ? 'New message' : (mode === 'forward' ? 'Forward' : 'Reply'),
      '<label class="f">From</label><select id="cFrom">' + fromOpts + '</select>'
      + '<label class="f">To</label><input id="cTo" value="' + esc(to) + '" placeholder="someone@example.com">'
      + '<div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">'
      + '<div><label class="f">Cc</label><input id="cCc" placeholder="optional"></div>'
      + '<div><label class="f">Bcc</label><input id="cBcc" placeholder="optional"></div></div>'
      + '<label class="f">Subject</label><input id="cSubject" value="' + esc(subject) + '">'
      + '<label class="f">Message</label><textarea id="cBody" rows="9">' + esc(body) + '</textarea>'
      + '<div class="err" id="cErr"></div>',
      '<button class="btn" onclick="csmCloseModal()">Cancel</button>'
      + (mode === 'new' || (m && m.folder === 'drafts') ? '<button class="btn" id="cDraft">Save draft</button>' : '')
      + '<button class="btn btn-primary" id="cSend">Send</button>',
      function () {
        var db = $('cDraft');
        if (db) db.onclick = async function () {
          try {
            await api('POST', '/private-email/messages/drafts', {
              id: (m && m.folder === 'drafts') ? m.id : undefined,
              from_address: $('cFrom').value,
              to: $('cTo').value || '',
              subject: $('cSubject').value || '',
              body: $('cBody').value || ''
            });
            closeModal(); toast('Draft saved'); await boot();
          } catch (e) { $('cErr').textContent = fail(e); }
        };
        $('cSend').onclick = async function () {
          var btn = this;
          var to2 = ($('cTo').value || '').trim();
          if (!to2) { $('cErr').textContent = 'Add who this goes to.'; return; }
          btn.disabled = true;
          try {
            await api('POST', '/private-email/send', {
              from_address: $('cFrom').value,
              to: to2,
              cc: splitAddrs($('cCc').value),
              bcc: splitAddrs($('cBcc').value),
              subject: $('cSubject').value || '',
              body: $('cBody').value || ''
            });
            // A draft that has been sent is no longer a draft: the row goes with the send, so the
            // Drafts folder never accumulates unsent-looking copies of mail that already left.
            if (m && m.folder === 'drafts') {
              try { await api('DELETE', '/private-email/messages/' + m.id); } catch (e) {}
            }
            closeModal(); toast('Sent'); S.sel = null; await boot();
          } catch (e) { $('cErr').textContent = fail(e); btn.disabled = false; }
        };
      });
  }
  function splitAddrs(v) {
    return String(v || '').split(/[,;]/).map(function (s) { return s.trim(); }).filter(Boolean);
  }

  // ── Folder menus (per-folder, exactly as B205 lists them) ────────────────────────────────────
  function folderMenu(folder) {
    var items = [['subfolder', 'Create subfolder'], ['read', 'Mark all as read'], ['unread', 'Mark all as unread']];
    if (folder === 'archive') {
      items.push(['to_inbox', 'Move all to Inbox']);
      items.push(['trash_all', 'Delete all messages']);
    }
    if (folder === 'spam' || folder === 'trash') items.push(['empty', 'Empty folder']);
    items.push(['sort', 'Sort alphabetically']);

    var html = '<div>' + items.map(function (it) {
      return '<button class="btn" style="display:block;width:100%;text-align:left;margin:6px 0" data-fact="' + it[0] + '">' + it[1] + '</button>';
    }).join('') + '<div class="err" id="fmErr"></div></div>';

    modal('Folder: ' + folder, html, '<button class="btn" onclick="csmCloseModal()">Close</button>', function () {
      Array.prototype.forEach.call(document.querySelectorAll('[data-fact]'), function (b) {
        b.onclick = function () { folderAction(folder, b.getAttribute('data-fact')); };
      });
    });
  }

  async function folderAction(folder, act) {
    var err = $('fmErr');
    try {
      if (act === 'subfolder') {
        var name = prompt('Name for the subfolder inside "' + folder + '"?');
        if (!name || !name.trim()) return;
        // Lower case, because the server stores every message's folder path lower-cased: a folder
        // made as "Clients" would never match (or count) the mail filed into it.
        await api('POST', '/private-email/folders', { parent: folder, name: name.trim().toLowerCase() });
        closeModal(); toast('Folder created'); await boot(); return;
      }
      if (act === 'sort') {
        S.messages.sort(function (a, b) { return String(a.subject || '').localeCompare(String(b.subject || '')); });
        closeModal(); render(); return;
      }
      if (act === 'empty') {
        var n = (S.folders.system.filter(function (f) { return f.id === folder; })[0] || {}).count || 0;
        if (!n) { closeModal(); toast('That folder is already empty'); return; }
        if (!confirm('Destroy all ' + n + ' messages in ' + folder + '? This cannot be undone.')) return;
      }
      var r = await api('POST', '/private-email/folders/action', { folder: folder, action: act });
      closeModal();
      toast(act === 'empty' ? ('Destroyed ' + (r.destroyed || 0)) : 'Done');
      await boot();
    } catch (e) { if (err) err.textContent = fail(e); else toast(fail(e), 'error'); }
  }

  // ── Move a message into a folder ────────────────────────────────────────────────────────────
  // The server accepts any system folder or any subfolder path (PATCH /messages/:id {folder}), and
  // the rail lists the account's own subfolders — but nothing in this client could put a message
  // INTO one, so "Folders you made" could only ever be filled by mail arriving on a filter.
  var SYSTEM_ORDER = [['inbox', 'Inbox'], ['archive', 'Archive'], ['spam', 'Spam'], ['trash', 'Trash'], ['drafts', 'Drafts']];

  function moveModal() {
    var m = S.sel;
    if (!m) return;
    var custom = (S.folders.custom || []).filter(function (f) { return f.path !== m.folder; });
    var sys = SYSTEM_ORDER.filter(function (f) { return f[0] !== m.folder; });
    var html = '<p style="margin:0 0 8px;color:var(--muted)">Where should this message go?</p>'
      + '<div style="display:flex;flex-wrap:wrap;gap:6px">'
      + sys.map(function (f) { return '<button class="btn" data-mto="' + f[0] + '">' + f[1] + '</button>'; }).join('')
      + '</div>'
      + (custom.length
        ? '<div style="margin-top:12px"><div style="font-size:11px;color:var(--muted);margin-bottom:4px">Folders you made</div>'
          + '<div style="display:flex;flex-wrap:wrap;gap:6px">'
          + custom.map(function (f) {
              return '<button class="btn" data-mto="' + esc(f.path) + '">' + esc(f.name || f.path) + '</button>';
            }).join('')
          + '</div></div>'
        : '<div style="margin-top:12px;font-size:12px;color:var(--muted)">You have no folders of your own yet — make one below and the message goes straight in.</div>')
      + '<label class="f" style="margin-top:14px">New folder name</label>'
      + '<input id="mvNewName" placeholder="clients">'
      + '<div style="font-size:11px;color:var(--muted);margin-top:4px">Stored in lower case, so the folder and the mail filed in it always agree.</div>'
      + '<div class="err" id="mvErr"></div>';

    modal('Move to folder', html,
      '<button class="btn" onclick="csmCloseModal()">Cancel</button><button class="btn btn-primary" id="mvNewGo">Create and move</button>',
      function () {
        Array.prototype.forEach.call(document.querySelectorAll('[data-mto]'), function (b) {
          b.onclick = function () { moveTo(b.getAttribute('data-mto')); };
        });
        $('mvNewGo').onclick = async function () {
          var name = ($('mvNewName').value || '').trim().toLowerCase();
          if (!name) { $('mvErr').textContent = 'Type a name for the new folder first.'; return; }
          if (name.indexOf('/') !== -1) { $('mvErr').textContent = 'A folder name cannot contain a slash.'; return; }
          try {
            var f = await api('POST', '/private-email/folders', { parent: 'archive', name: name });
            await moveTo(f.path, true);
          } catch (e) { $('mvErr').textContent = fail(e); }
        };
      });
  }

  async function moveTo(path, made) {
    var m = S.sel;
    if (!m) { closeModal(); return; }
    try {
      await api('PATCH', '/private-email/messages/' + m.id, { folder: path });
      closeModal();
      S.sel = null;
      await boot();
      toast(made ? ('Folder made and message moved to ' + path) : ('Moved to ' + path));
    } catch (e) {
      var err = $('mvErr');
      if (err) err.textContent = fail(e); else toast(fail(e), 'error');
    }
  }

  // ── Labels ──────────────────────────────────────────────────────────────────────────────────
  var COLORS = ['#ef4444', '#f59e0b', '#10b981', '#3b82f6', '#8b5cf6', '#ec4899', '#64748b'];

  function labelModal(label) {
    var chosen = (label && label.color) || COLORS[3];
    modal(label ? 'Edit label' : 'Create label',
      '<label class="f">Label name</label><input id="lbName" value="' + esc(label ? label.name : '') + '" placeholder="Label name">'
      + '<label class="f">Colour</label><div class="swatches" id="lbSw">'
      + COLORS.map(function (c) { return '<span class="sw' + (c === chosen ? ' on' : '') + '" data-color="' + c + '" style="background:' + c + '"></span>'; }).join('')
      + '</div><div class="err" id="lbErr"></div>',
      '<button class="btn" onclick="csmCloseModal()">Cancel</button><button class="btn btn-primary" id="lbSave">' + (label ? 'Save' : 'Create') + '</button>',
      function () {
        var pick = chosen;
        Array.prototype.forEach.call(document.querySelectorAll('[data-color]'), function (s) {
          s.onclick = function () {
            pick = s.getAttribute('data-color');
            Array.prototype.forEach.call(document.querySelectorAll('[data-color]'), function (x) { x.classList.remove('on'); });
            s.classList.add('on');
          };
        });
        $('lbSave').onclick = async function () {
          var name = ($('lbName').value || '').trim();
          if (!name) { $('lbErr').textContent = 'Give the label a name.'; return; }
          try {
            if (label) await api('PATCH', '/private-email/labels/' + label.id, { name: name, color: pick });
            else await api('POST', '/private-email/labels', { name: name, color: pick });
            closeModal(); toast('Saved'); await boot();
            if (S.sel) applyLabelControl();
          } catch (e) { $('lbErr').textContent = fail(e); }
        };
      });
  }

  function labelMenu(id) {
    var l = S.labels.filter(function (x) { return x.id === id; })[0] || {};
    modal('Label: ' + (l.name || ''), '<p style="color:var(--muted);margin-top:0">A label is attached to individual messages, so this only changes the label itself.</p>',
      '<button class="btn btn-danger" id="lbDel">Delete</button><button class="btn" onclick="csmCloseModal()">Close</button><button class="btn btn-primary" id="lbEdit">Edit</button>',
      function () {
        $('lbEdit').onclick = function () { closeModal(); labelModal(l); };
        $('lbDel').onclick = async function () {
          if (!confirm('Delete the label "' + (l.name || '') + '"? Messages are not touched.')) return;
          try { await api('DELETE', '/private-email/labels/' + id); closeModal(); toast('Label deleted'); await boot(); }
          catch (e) { toast(fail(e), 'error'); }
        };
      });
  }

  // Applying a label to the OPEN message: choose one, it attaches; it can be removed the same way.
  function applyLabelControl() {
    var box = $('mLabelBox');
    if (!box || !S.sel) return;
    var attached = (S.sel.labels || []).map(function (l) { return l.id; });
    box.innerHTML = S.labels.length
      ? S.labels.map(function (l) {
        var on = attached.indexOf(l.id) >= 0;
        return '<button class="btn" data-toggle="' + esc(l.id) + '" style="margin:2px 4px 2px 0">'
          + '<i style="display:inline-block;width:9px;height:9px;border-radius:999px;margin-right:6px;background:' + esc(l.color) + '"></i>'
          + esc(l.name) + (on ? ' ✓' : '') + '</button>';
      }).join('')
      : '<span style="color:var(--muted);font-size:12px">No labels yet — create one in the sidebar.</span>';
    Array.prototype.forEach.call(box.querySelectorAll('[data-toggle]'), function (b) {
      b.onclick = async function () {
        var id = b.getAttribute('data-toggle');
        var on = attached.indexOf(id) >= 0;
        try {
          if (on) await api('DELETE', '/private-email/labels/' + id + '/apply', { message_ids: [S.sel.id] });
          else await api('POST', '/private-email/labels/' + id + '/apply', { message_ids: [S.sel.id] });
          await openMessage(S.sel.id);
        } catch (e) { toast(fail(e), 'error'); }
      };
    });
  }

  // ── Modal ───────────────────────────────────────────────────────────────────────────────────
  function modal(title, body, footer, after) {
    closeModal();
    var bg = document.createElement('div');
    bg.className = 'modal-bg';
    bg.innerHTML = '<div class="modal"><h3>' + esc(title) + '</h3>' + body + '<div class="modal-foot">' + (footer || '') + '</div></div>';
    document.body.appendChild(bg);
    bg.addEventListener('click', function (e) { if (e.target === bg) closeModal(); });
    if (after) after();
  }
  function closeModal() {
    Array.prototype.forEach.call(document.querySelectorAll('.modal-bg'), function (n) { n.remove(); });
  }
  window.csmCloseModal = closeModal;

  // ── Start ───────────────────────────────────────────────────────────────────────────────────
  // A mailbox chosen on the CRM's Inbox tab arrives as ?address=, so the user lands where they
  // pressed instead of on a chooser they already answered (B211).
  var params = new URLSearchParams(location.search);
  if (params.get('address')) S.address = params.get('address');

  if (S.token) { boot(); } else { render(); }
})();
