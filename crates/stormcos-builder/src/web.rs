//! Web GUI — one self-contained dashboard page that drives the REST API.

use axum::{Router, response::Html, routing::get};

pub fn router() -> Router {
    Router::new().route("/", get(|| async { Html(INDEX) }))
}

const INDEX: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>stormcos builder</title>
<style>
 :root{color-scheme:light dark}
 body{font:14px/1.5 system-ui,sans-serif;margin:0;background:#0b0e14;color:#d6deeb}
 header{padding:14px 20px;background:#11151f;border-bottom:1px solid #222a3a}
 h1{margin:0;font-size:17px;letter-spacing:.02em}
 h1 span{color:#7fdbca}
 main{max-width:1000px;margin:0 auto;padding:20px;display:grid;gap:22px}
 section{background:#11151f;border:1px solid #222a3a;border-radius:10px;padding:16px}
 h2{margin:0 0 10px;font-size:14px;color:#82aaff;text-transform:uppercase;letter-spacing:.06em}
 table{width:100%;border-collapse:collapse;font-size:13px}
 td,th{text-align:left;padding:6px 8px;border-bottom:1px solid #1b2130;vertical-align:top}
 th{color:#8892b0;font-weight:600}
 button{font:inherit;background:#1d2433;color:#d6deeb;border:1px solid #2e3a52;border-radius:6px;padding:5px 10px;cursor:pointer}
 button:hover{background:#26304a}
 button.p{background:#21543f;border-color:#2e7d5b;color:#c8ffe6}
 button.d{background:#5a2330;border-color:#8a3546;color:#ffd6dd}
 a{color:#7fdbca}
 input,select{font:inherit;background:#0b0e14;color:#d6deeb;border:1px solid #2e3a52;border-radius:6px;padding:5px 8px}
 .row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
 .tag{font-size:11px;color:#8892b0}
 .ph{font-size:11px;padding:1px 7px;border-radius:20px;border:1px solid #2e3a52}
 .ready{color:#c8ffe6;border-color:#2e7d5b}.failed{color:#ffd6dd;border-color:#8a3546}
 .busy{color:#ffe8b3;border-color:#8a7020}
 code{color:#addb67}
</style></head><body>
<header><h1>storm<span>cos</span> builder <span style="color:#8892b0;font-size:12px">· boot images · on-demand clusters</span></h1></header>
<main>
 <section><h2>Flavors</h2><table id="flavors"><tbody></tbody></table></section>
 <section><h2>Releases</h2><table id="releases"><thead><tr><th>release</th><th>flavor</th><th>created</th><th>downloads</th><th>net-boot</th></tr></thead><tbody></tbody></table></section>
 <section><h2>Provision a cluster</h2>
   <div class="row">
     <input id="cname" placeholder="name (dns-safe)" size="14">
     <input id="cdns" placeholder="dns name (default name.g8.lo)" size="20">
     <select id="cflavor"></select>
     <select id="cboot"><option value="local-disk">local-disk</option><option value="iscsi">iscsi</option><option value="nvme-tcp">nvme-tcp</option></select>
     <button class="p" onclick="createCluster()">Create</button>
   </div>
 </section>
 <section><h2>Clusters</h2><table id="clusters"><thead><tr><th>name</th><th>dns</th><th>phase</th><th>ip</th><th>release</th><th></th></tr></thead><tbody></tbody></table></section>
 <section><h2>Recent builds</h2><table id="builds"><thead><tr><th>id</th><th>status</th><th>reason</th><th>started</th></tr></thead><tbody></tbody></table></section>
</main>
<script>
const $=s=>document.querySelector(s), api=(p,o)=>fetch('/api/v1'+p,o).then(r=>r.json());
function ph(p){const c=p==='ready'?'ready':(p==='failed'?'failed':'busy');return `<span class="ph ${c}">${p}</span>`}
async function refresh(){
 const fl=(await api('/flavors')).flavors||[];
 $('#flavors tbody').innerHTML=fl.map(f=>`<tr><td><b>${f.name}</b>${f.extends?` <span class=tag>extends ${f.extends}</span>`:''}<div class=tag>${f.description||''}</div></td><td class=tag>${(f.assets||[]).join(', ')}</td><td><button onclick="build('${f.name}')">Build</button></td></tr>`).join('');
 const sel=$('#cflavor');sel.innerHTML=fl.map(f=>`<option>${f.name}</option>`).join('');
 const rels=(await api('/releases')).releases||[];
 $('#releases tbody').innerHTML=rels.map(r=>{
   const dl=(r.artifacts||[]).map(a=>`<a href="/api/v1/releases/${r.id}/download/${a.format}">${a.format}</a>`).join(' · ')||'<span class=tag>none</span>';
   const nb=(r.targets||[]).map(t=>`<span class=tag>${t.transport}</span>`).join(' ')||'<span class=tag>—</span>';
   return `<tr><td><code>${r.id}</code></td><td>${r.flavor}</td><td class=tag>${r.created}</td><td>${dl}</td><td>${nb}</td></tr>`}).join('');
 const cl=(await api('/clusters')).clusters||[];
 $('#clusters tbody').innerHTML=cl.map(c=>`<tr><td><b>${c.name}</b></td><td class=tag>${c.dns_name}</td><td>${ph(c.phase)}</td><td>${c.ip||''}</td><td class=tag>${c.release_id}</td><td class=row><button onclick="rebuild('${c.name}')">Rebuild</button><button class=d onclick="del('${c.name}')">Delete</button></td></tr>`).join('');
 const bs=(await api('/builds')).builds||[];
 $('#builds tbody').innerHTML=bs.slice(-8).reverse().map(b=>`<tr><td><code>${b.id}</code></td><td>${ph(b.status)}</td><td class=tag>${b.reason}</td><td class=tag>${b.started}</td></tr>`).join('');
}
function build(f){api('/flavors/'+f+'/build',{method:'POST'}).then(refresh)}
function rebuild(n){if(confirm('Wipe + rebuild '+n+'?'))api('/clusters/'+n+'/rebuild',{method:'POST'}).then(refresh)}
function del(n){if(confirm('Delete '+n+'?'))api('/clusters/'+n,{method:'DELETE'}).then(refresh)}
function createCluster(){
 const body={name:$('#cname').value.trim(),dns_name:$('#cdns').value.trim()||null,flavor:$('#cflavor').value,boot_method:$('#cboot').value};
 api('/clusters',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)})
   .then(r=>{if(r.error)alert(r.error);$('#cname').value='';$('#cdns').value='';refresh()});
}
refresh();setInterval(refresh,5000);
</script></body></html>"#;
