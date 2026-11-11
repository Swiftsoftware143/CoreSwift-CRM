(function(){
  var S={tickets:[],boxes:[],sel:null,err:null};
  function esc(s){return String(s==null?'':s).replace(/[&<>"]/g,function(c){return {'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c];});}
  function fmt(d){try{return d?new Date(d).toLocaleString():'';}catch(e){return String(d||'');}}
  function who(t){return t.from_name||t.from_email||t.contact_email||t.email||'Unknown sender';}
  function render(){
    var tk=S.tickets;
    var rows=tk.map(function(t){
      return '<tr class="pe-row" data-id="'+esc(t.id)+'" style="cursor:pointer;border-bottom:1px solid rgba(128,128,128,.25)">'
        +'<td style="padding:8px">'+esc(who(t))+'</td>'
        +'<td style="padding:8px">'+esc(t.subject||'(no subject)')+'</td>'
        +'<td style="padding:8px;white-space:nowrap;opacity:.75">'+esc(fmt(t.created_at||t.received_at))+'</td>'
        +'<td style="padding:8px;opacity:.75">'+esc(t.status||'')+'</td></tr>';
    }).join('');
    var empty='<div style="padding:18px;border:1px dashed rgba(128,128,128,.4);border-radius:8px">'
      +'<h4 style="margin:0 0 6px">No mail yet</h4>'
      +'<p style="margin:0 0 8px">Incoming email becomes a ticket and appears here. Two things make that happen:</p>'
      +'<ol style="margin:0 0 8px 18px"><li>Add your domain in Mailgun, then point its MX records at <code>mxa.mailgun.org</code> and <code>mxb.mailgun.org</code>.</li>'
      +'<li>Wait until Mailgun shows the domain as Active, then create a mailbox below and email it.</li></ol>'
      +'<p style="margin:0;opacity:.75">Sending and receiving are separate setups — sending can work while receiving is still off.</p></div>';
    return '<div>'
      +'<div style="display:flex;justify-content:space-between;align-items:center;margin:0 0 10px">'
      +'<h3 style="margin:0">Shared Inbox</h3><span style="opacity:.7">'+tk.length+' message'+(tk.length===1?'':'s')+' · every address in one box</span></div>'
      +(S.err?'<div style="padding:8px;margin-bottom:8px;border-radius:6px;background:rgba(220,80,80,.15)">'+esc(S.err)+'</div>':'')
      +(tk.length?'<table style="width:100%;border-collapse:collapse"><thead><tr style="text-align:left;opacity:.7">'
        +'<th style="padding:6px 8px">From</th><th style="padding:6px 8px">Subject</th><th style="padding:6px 8px">Received</th><th style="padding:6px 8px">Status</th></tr></thead><tbody>'+rows+'</tbody></table>':empty)
      +'<div id="peDetail"></div></div>';
  }
  async function load(){
    var el=document.getElementById('peWrap');
    if(!el) return;
    try{
      var r=await Promise.all([API.get('/tickets'),API.get('/private-email/boxes').catch(function(){return [];}),API.get('/private-email/domains').catch(function(){return [];})]);
      var t=r[0],b=r[1],d=r[2];
      S.tickets=(t&&(t.tickets||t.items))||(Array.isArray(t)?t:[])||[];
      S.boxes=Array.isArray(b)?b:((b&&(b.boxes||b.items))||[]);
      S.domains=Array.isArray(d)?d:((d&&(d.domains||d.items))||[]);
      S.err=null;
    }catch(e){S.err=(e&&e.message)||String(e);}
    el.innerHTML=render()+setupHtml();
    var rs=document.querySelectorAll('.pe-row');
    for(var i=0;i<rs.length;i++){rs[i].onclick=function(){openT(this.getAttribute('data-id'));};}
    bindSetup();
  }
  function setupHtml(){
    var domOpts=(S.domains||[]).map(function(x){return '<option value="'+esc(x.id)+'">'+esc(x.domain)+'</option>';}).join('');
    var domList=(S.domains&&S.domains.length)?S.domains.map(function(x){return esc(x.domain);}).join(', '):'none yet';
    var boxList=(S.boxes&&S.boxes.length)?S.boxes.map(function(x){return esc(x.email_address);}).join(', '):'none yet';
    return '<div style="margin-top:18px;padding:14px;border:1px solid rgba(128,128,128,.35);border-radius:8px">'
      +'<h4 style="margin:0 0 10px">Set up your mailbox</h4>'
      +'<div style="margin-bottom:16px"><strong>1. Add a sending domain</strong>'
      +'<div style="opacity:.75;margin:2px 0 6px">One key covers both directions: it receives inbound mail and sends your replies. Mailgun needs MX (receiving) and SPF+DKIM (sending).</div>'
      +'<input id="peDom" placeholder="yourcompany.com" style="margin-right:6px">'
      +'<input id="peKey" placeholder="Mailgun sending key" style="width:230px;margin-right:6px">'
      +'<button class="btn btn-primary" id="peAddDom">Add domain</button><span id="peDomMsg" style="margin-left:8px"></span></div>'
      +'<div><strong>2. Add a reply-to inbox</strong>'
      +'<div style="opacity:.75;margin:2px 0 6px">A public address people write to — info@, support@. All of them share this one inbox.</div>'
      +((S.domains&&S.domains.length)?('<select id="peDomSel" style="margin-right:6px">'+domOpts+'</select>'):'<span style="opacity:.7">Add a domain first.</span> ')
      +'<input id="peLocal" placeholder="info" style="width:120px;margin-right:6px">'
      +'<button class="btn btn-primary" id="peAddBox">Add inbox</button><span id="peBoxMsg" style="margin-left:8px"></span></div>'
      +'<div style="margin-top:12px;opacity:.75">Domains: '+domList+'<br>Inboxes: '+boxList+'</div>'
      +((S.domains&&S.domains.length)?('<div style="margin-top:10px"><label style="cursor:pointer"><input type="checkbox" id="peCatchAll"'+((S.domains[0]&&S.domains[0].catch_all_enabled)?' checked':'')+'> <strong>Catch-all</strong> — deliver mail sent to any address on this domain, including ones you have not created, into this one inbox.</label></div>'):'')
      +'</div>';
  }
  function bindSetup(){
    var ad=document.getElementById('peAddDom');
    if(ad) ad.onclick=async function(){
      var de=document.getElementById('peDom'),ke=document.getElementById('peKey'),m=document.getElementById('peDomMsg');
      var dom=((de&&de.value)||'').trim();
      if(!dom){m.textContent='Enter a domain';return;}
      this.disabled=true;m.textContent='Adding…';
      try{await API.post('/private-email/domains',{domain:dom,mailgun_api_key:((ke&&ke.value)||'').trim()||null});m.textContent='Added ✓';await load();}
      catch(e){m.textContent='Failed: '+((e&&e.message)||e);}
      this.disabled=false;
    };
    var ab=document.getElementById('peAddBox');
    if(ab) ab.onclick=async function(){      var s=document.getElementById('peDomSel'),l=document.getElementById('peLocal'),m=document.getElementById('peBoxMsg');
      if(!s){m.textContent='Add a domain first';return;}
      var lp=((l&&l.value)||'').trim();
      if(!lp){m.textContent='Enter a mailbox name';return;}
      this.disabled=true;m.textContent='Adding…';
      try{await API.post('/private-email/boxes',{domain_id:s.value,local_part:lp});m.textContent='Added ✓';await load();}
      catch(e){m.textContent='Failed: '+((e&&e.message)||e);}
      this.disabled=false;
    };
    var ca=document.getElementById('peCatchAll');
    if(ca) ca.onchange=async function(){
      var d0=(S.domains||[])[0];
      if(!d0){ this.checked=!this.checked; return; }
      var want=this.checked, box=this;
      box.disabled=true;
      try{ await API.patch('/private-email/domains/'+d0.id,{catch_all_enabled:want}); d0.catch_all_enabled=want; }
      catch(e){ box.checked=!want; alert('Could not save catch-all: '+((e&&e.message)||e)); }
      box.disabled=false;
    };
  }
  function openT(id){
    var t=null;for(var i=0;i<S.tickets.length;i++){if(String(S.tickets[i].id)===String(id))t=S.tickets[i];}
    var d=document.getElementById('peDetail');
    if(!t||!d) return;
    var opts=S.boxes.map(function(x){return '<option value="'+esc(x.email_address)+'">'+esc(x.email_address)+'</option>';}).join('')
      ||'<option value="">(no mailbox yet)</option>';
    d.innerHTML='<div style="margin-top:14px;padding:14px;border:1px solid rgba(128,128,128,.35);border-radius:8px">'
      +'<h4 style="margin:0 0 4px">'+esc(t.subject||'(no subject)')+'</h4>'
      +'<div style="opacity:.75;margin-bottom:10px">From '+esc(who(t))+'</div>'
      +'<div style="white-space:pre-wrap;margin-bottom:12px">'+esc(t.description||t.body||t.message||'')+'</div>'
      +'<label style="display:block;font-size:12px;opacity:.75;margin-bottom:4px">Reply as</label>'
      +'<select id="peFrom" style="margin-bottom:8px">'+opts+'</select>'
      +'<textarea id="peText" rows="4" placeholder="Write your reply…" style="width:100%;margin-bottom:8px"></textarea>'
      +'<button class="btn btn-primary" id="peSend">Send reply</button> <span id="peMsg" style="margin-left:8px"></span></div>';
    document.getElementById('peSend').onclick=async function(){
      var btn=this,from=document.getElementById('peFrom').value
        ,body=document.getElementById('peText').value.trim()
        ,to=t.from_email||t.contact_email||t.email||''
        ,m=document.getElementById('peMsg');
      if(!from){m.textContent='Create a mailbox first';return;}
      if(!body){m.textContent='Write a reply first';return;}
      btn.disabled=true;m.textContent='Sending…';
      try{
        await API.post('/private-email/send',{from_address:from,to:to,subject:'Re: '+(t.subject||''),body:body,in_reply_to:t.id});
        m.textContent='Sent ✓';document.getElementById('peText').value='';
      }catch(e){m.textContent='Failed: '+((e&&e.message)||e);}
      btn.disabled=false;
    };
  }
  window.renderPeInbox=function(){return '<div id="peWrap"><p>Loading shared inbox…</p></div>';};
  window.loadPeInbox=load;
  if(typeof TAB_RENDERERS!=='undefined'){TAB_RENDERERS['inbox']=window.renderPeInbox;}
  if(typeof TAB_LOADERS!=='undefined'){TAB_LOADERS['inbox']=window.loadPeInbox;}
  /*PART2*/
})();
