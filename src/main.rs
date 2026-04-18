use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{Router, extract::State, response::{Html, Json}, routing::get};
use serde::Serialize;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

// ── Config ────────────────────────────────────────────────────────────────────

struct ServiceCfg {
    key:  &'static str,
    name: &'static str,
    host: &'static str,       // shown on the status page
    url:  &'static str,       // internal health-check URL
}

const SERVICES: &[ServiceCfg] = &[
    ServiceCfg { key: "api",   name: "Auth API",      host: "api.zeeble.xyz",   url: "http://zbeam:8001/health"   },
    ServiceCfg { key: "dm",    name: "Messaging",     host: "dm.zeeble.xyz",    url: "http://zpulse:3002/health"  },
    ServiceCfg { key: "cloud", name: "Cloud Servers", host: "cloud.zeeble.xyz", url: "http://zcloud:8003/health"  },
];

const HISTORY_MAX: usize  = 90;   // checks to keep per service
const INCIDENT_MAX: usize = 10;   // incidents to keep per service
const CHECK_SECS:   u64   = 60;   // how often to poll

// ── State types ───────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct Check {
    ts: u64,           // unix ms
    ok: bool,
    ms: Option<u64>,   // latency
}

#[derive(Clone, Serialize)]
struct Incident {
    start_ts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_ts: Option<u64>,
}

#[derive(Clone, Serialize)]
struct ServiceStatus {
    key:        String,
    name:       String,
    host:       String,
    status:     String,        // "up" | "down" | "unknown"
    uptime_pct: Option<f64>,
    checks:     Vec<Check>,   // most-recent last
    incidents:  Vec<Incident>,
}

#[derive(Clone, Serialize)]
struct StatusResponse {
    checked_at: u64,
    services:   Vec<ServiceStatus>,
}

type Shared = Arc<RwLock<Vec<ServiceRecord>>>;

/// Internal mutable state per service.
struct ServiceRecord {
    key:       &'static str,
    name:      &'static str,
    host:      &'static str,
    url:       &'static str,
    checks:    VecDeque<Check>,
    incidents: Vec<Incident>,
}

impl ServiceRecord {
    fn new(cfg: &'static ServiceCfg) -> Self {
        Self {
            key: cfg.key, name: cfg.name, host: cfg.host, url: cfg.url,
            checks: VecDeque::new(),
            incidents: Vec::new(),
        }
    }

    fn push(&mut self, ok: bool, ms: Option<u64>) {
        let ts = now_ms();
        let prev_ok = self.checks.back().map(|c| c.ok);

        // Incident tracking
        if prev_ok == Some(true) && !ok {
            // Went down
            self.incidents.push(Incident { start_ts: ts, end_ts: None });
            if self.incidents.len() > INCIDENT_MAX {
                self.incidents.remove(0);
            }
        } else if prev_ok == Some(false) && ok {
            // Came back up — close the last open incident
            if let Some(open) = self.incidents.iter_mut().rev().find(|i| i.end_ts.is_none()) {
                open.end_ts = Some(ts);
            }
        }

        self.checks.push_back(Check { ts, ok, ms });
        if self.checks.len() > HISTORY_MAX {
            self.checks.pop_front();
        }
    }

    fn to_status(&self) -> ServiceStatus {
        let status = match self.checks.back() {
            Some(c) if c.ok => "up",
            Some(_)          => "down",
            None             => "unknown",
        }.to_string();

        let uptime_pct = if self.checks.is_empty() {
            None
        } else {
            let up = self.checks.iter().filter(|c| c.ok).count();
            Some((up as f64 / self.checks.len() as f64) * 100.0)
        };

        ServiceStatus {
            key:        self.key.to_string(),
            name:       self.name.to_string(),
            host:       self.host.to_string(),
            status,
            uptime_pct,
            checks:     self.checks.iter().cloned().collect(),
            incidents:  self.incidents.clone(),
        }
    }
}

// ── Health poller ─────────────────────────────────────────────────────────────

async fn poll_once(client: &reqwest::Client, record: &mut ServiceRecord) {
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        client.get(record.url).send(),
    ).await;

    let (ok, ms) = match result {
        Ok(Ok(resp)) => (resp.status().is_success(), Some(start.elapsed().as_millis() as u64)),
        _            => (false, None),
    };

    record.push(ok, ms);
}

async fn poller(state: Shared) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");

    loop {
        {
            let mut records = state.write().await;
            for rec in records.iter_mut() {
                poll_once(&client, rec).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(CHECK_SECS)).await;
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn status_json(State(state): State<Shared>) -> Json<StatusResponse> {
    let records = state.read().await;
    Json(StatusResponse {
        checked_at: now_ms(),
        services:   records.iter().map(|r| r.to_status()).collect(),
    })
}

async fn status_page() -> Html<&'static str> {
    Html(STATUS_HTML)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let state: Shared = Arc::new(RwLock::new(
        SERVICES.iter().map(ServiceRecord::new).collect(),
    ));

    // Run one immediate check before serving
    {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let mut records = state.write().await;
        for rec in records.iter_mut() {
            poll_once(&client, rec).await;
        }
    }

    // Background poller
    tokio::spawn(poller(Arc::clone(&state)));

    let app = Router::new()
        .route("/", get(status_page))
        .route("/api/status", get(status_json))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = "0.0.0.0:8004";
    println!("Zstatus running on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ── Embedded HTML ─────────────────────────────────────────────────────────────

const STATUS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8"/>
  <meta name="viewport" content="width=device-width,initial-scale=1.0"/>
  <title>Zeeble Status</title>
  <link rel="preconnect" href="https://fonts.googleapis.com"/>
  <link href="https://fonts.googleapis.com/css2?family=Inter:wght@300;400;500;600&family=Plus+Jakarta+Sans:wght@600;700;800&display=swap" rel="stylesheet"/>
  <style>
    *,*::before,*::after{margin:0;padding:0;box-sizing:border-box}
    :root{
      --bg:#16181c;--panel:#212328;--panel2:#26282e;
      --accent:#6366f1;--accent-dim:rgba(99,102,241,.12);--accent-glow:rgba(99,102,241,.25);
      --t1:#f3f4f6;--t2:#9ca3af;--t3:#6b7280;
      --border:rgba(255,255,255,.07);
      --green:#10b981;--green-dim:rgba(16,185,129,.13);
      --red:#ef4444;--red-dim:rgba(239,68,68,.13);
      --yellow:#f59e0b;--yellow-dim:rgba(245,158,11,.13);
      --rl:16px;--rm:12px;--rs:8px;
    }
    body{font-family:'Inter',-apple-system,sans-serif;background:var(--bg);color:var(--t1);min-height:100vh;padding:2rem 1rem 4rem}

    /* header */
    header{max-width:700px;margin:0 auto 2.5rem;display:flex;align-items:center;justify-content:space-between}
    .logo{display:flex;align-items:center;gap:.75rem}
    .logo-icon{width:40px;height:40px;border-radius:12px;background:linear-gradient(135deg,#6366f1,#4f46e5);display:flex;align-items:center;justify-content:center;font-size:20px;font-weight:800;color:#fff;font-family:'Plus Jakarta Sans',sans-serif;flex-shrink:0}
    .logo-name{font-family:'Plus Jakarta Sans',sans-serif;font-weight:800;font-size:1.2rem}
    .logo-sub{font-size:.7rem;font-weight:500;color:var(--t3);letter-spacing:.05em;text-transform:uppercase}
    .refresh-btn{background:var(--panel);border:1px solid var(--border);border-radius:var(--rs);color:var(--t2);font-size:.8rem;padding:.4rem .9rem;cursor:pointer;transition:background .15s,color .15s;font-family:inherit}
    .refresh-btn:hover{background:var(--panel2);color:var(--t1)}

    /* overall banner */
    .overall{max-width:700px;margin:0 auto 1.25rem;background:var(--panel);border:1px solid var(--border);border-radius:var(--rl);padding:1.2rem 1.5rem;display:flex;align-items:center;justify-content:space-between;gap:1rem}
    .overall-left{display:flex;align-items:center;gap:.75rem}
    .pulse{width:10px;height:10px;border-radius:50%;background:var(--green);flex-shrink:0}
    .pulse.up{animation:pulseGreen 2.5s ease-in-out infinite}
    .pulse.down{background:var(--red);animation:none}
    .pulse.degraded{background:var(--yellow);animation:none}
    .pulse.unknown{background:var(--t3);animation:none}
    @keyframes pulseGreen{0%,100%{opacity:1;box-shadow:0 0 0 0 rgba(16,185,129,.4)}50%{opacity:.8;box-shadow:0 0 0 5px rgba(16,185,129,0)}}
    .overall-label{font-family:'Plus Jakarta Sans',sans-serif;font-weight:700;font-size:1rem}
    .overall-ts{font-size:.72rem;color:var(--t3);white-space:nowrap}

    /* service cards */
    .services{max-width:700px;margin:0 auto;display:flex;flex-direction:column;gap:1rem}
    .card{background:var(--panel);border:1px solid var(--border);border-radius:var(--rl);padding:1.35rem 1.5rem}
    .card-header{display:flex;align-items:center;justify-content:space-between;margin-bottom:1.1rem}
    .card-name{font-family:'Plus Jakarta Sans',sans-serif;font-weight:700;font-size:.95rem}
    .card-host{font-size:.72rem;color:var(--t3);margin-top:2px}
    .badge{display:inline-flex;align-items:center;gap:.35rem;font-size:.72rem;font-weight:600;padding:.25rem .7rem;border-radius:999px;white-space:nowrap}
    .badge .dot{width:6px;height:6px;border-radius:50%;background:currentColor}
    .b-up{background:var(--green-dim);color:var(--green);border:1px solid rgba(16,185,129,.2)}
    .b-down{background:var(--red-dim);color:var(--red);border:1px solid rgba(239,68,68,.2)}
    .b-unknown{background:var(--panel2);color:var(--t3)}

    /* uptime bars */
    .bars-wrap{display:flex;align-items:center;gap:.75rem;margin-bottom:.5rem}
    .bars{flex:1;display:flex;gap:2px;height:30px;align-items:stretch}
    .bar{flex:1;border-radius:3px;background:var(--panel2);cursor:default;position:relative}
    .bar.up{background:var(--green);opacity:.65}
    .bar.down{background:var(--red);opacity:.8}
    .bar.up:hover{opacity:1}
    .bar.down:hover{opacity:1}
    .bar[title]:hover::after{content:attr(title);position:absolute;bottom:calc(100% + 6px);left:50%;transform:translateX(-50%);background:#1b1d21;border:1px solid var(--border);border-radius:6px;padding:4px 8px;font-size:.68rem;color:var(--t1);white-space:nowrap;pointer-events:none;z-index:10}
    .uptime-pct{font-size:.8rem;font-weight:600;min-width:44px;text-align:right}
    .bar-labels{display:flex;justify-content:space-between;font-size:.65rem;color:var(--t3);margin-bottom:.85rem}

    /* incidents */
    .incidents{border-top:1px solid var(--border);padding-top:.85rem}
    .inc-title{font-size:.65rem;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:var(--t3);margin-bottom:.5rem}
    .inc-item{display:flex;align-items:baseline;justify-content:space-between;gap:1rem;font-size:.78rem;padding:.3rem 0}
    .inc-item+.inc-item{border-top:1px solid var(--border)}
    .inc-msg{color:var(--t2)}
    .pill{display:inline-block;border-radius:4px;padding:0 5px;font-size:.68rem;margin-right:4px;vertical-align:1px}
    .pill.out{background:var(--red-dim);color:var(--red)}
    .pill.res{background:var(--green-dim);color:var(--green)}
    .inc-ts{color:var(--t3);white-space:nowrap;font-size:.7rem}
    .no-inc{font-size:.78rem;color:var(--t3)}

    /* footer */
    footer{max-width:700px;margin:2rem auto 0;display:flex;justify-content:space-between;font-size:.7rem;color:var(--t3)}
    footer a{color:var(--accent);text-decoration:none}
    footer a:hover{text-decoration:underline}

    .loading{text-align:center;color:var(--t3);padding:4rem;font-size:.9rem}
    .error{text-align:center;color:var(--red);padding:2rem;font-size:.85rem;background:var(--panel);border-radius:var(--rl);border:1px solid var(--border);max-width:700px;margin:0 auto}
  </style>
</head>
<body>

<header>
  <div class="logo">
    <div class="logo-icon">Z</div>
    <div>
      <div class="logo-name">Zeeble</div>
      <div class="logo-sub">System Status</div>
    </div>
  </div>
  <button class="refresh-btn" onclick="load()">↻ Refresh</button>
</header>

<div id="overall" class="overall" style="display:none">
  <div class="overall-left">
    <div class="pulse unknown" id="pulse"></div>
    <div class="overall-label" id="overall-label">Loading…</div>
  </div>
  <div class="overall-ts" id="overall-ts"></div>
</div>

<div id="services" class="services">
  <div class="loading">Fetching service status…</div>
</div>

<footer>
  <span>Checks run server-side every 60 s · History kept in memory</span>
  <span><a href="https://zeeble.xyz">zeeble.xyz</a></span>
</footer>

<script>
const AUTO_MS = 30_000;

function fmtAgo(ts) {
  const s = Math.round((Date.now() - ts) / 1000);
  if (s < 60)   return s + 's ago';
  if (s < 3600) return Math.round(s/60) + 'm ago';
  if (s < 86400) return Math.round(s/3600) + 'h ago';
  return Math.round(s/86400) + 'd ago';
}
function fmtTime(ts) {
  return new Date(ts).toLocaleString(undefined,{month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'});
}
function durStr(ms) {
  const s = Math.round(ms/1000);
  if (s < 60) return s+'s';
  if (s < 3600) return Math.round(s/60)+'m';
  return (s/3600).toFixed(1)+'h';
}

function renderBadge(status) {
  const map = {
    up:      ['b-up',      'Operational'],
    down:    ['b-down',    'Outage'],
    unknown: ['b-unknown', 'Unknown'],
  };
  const [cls, label] = map[status] || map.unknown;
  return `<span class="badge ${cls}"><span class="dot"></span>${label}</span>`;
}

function renderBars(checks) {
  const MAX = 90;
  const pad = Math.max(0, MAX - checks.length);
  let html = '';
  for (let i = 0; i < pad; i++) html += `<div class="bar" title="No data"></div>`;
  for (const c of checks) {
    const cls  = c.ok ? 'up' : 'down';
    const time = fmtTime(c.ts);
    const extra = c.ok ? (c.ms != null ? ` · ${c.ms}ms` : '') : ' · Outage';
    html += `<div class="bar ${cls}" title="${time}${extra}"></div>`;
  }
  return html;
}

function renderIncidents(incidents) {
  if (!incidents.length) return '<span class="no-inc">No incidents recorded</span>';
  return [...incidents].reverse().slice(0, 5).map(inc => {
    const res = inc.end_ts != null;
    const dur = res ? ` · down for ${durStr(inc.end_ts - inc.start_ts)}` : '';
    return `
      <div class="inc-item">
        <span class="inc-msg">
          <span class="pill ${res ? 'res' : 'out'}">${res ? 'resolved' : 'outage'}</span>
          ${res ? 'Service recovered' + dur : 'Outage started'}
        </span>
        <span class="inc-ts">${fmtTime(inc.start_ts)}</span>
      </div>`;
  }).join('');
}

function renderCard(svc) {
  const oldest = svc.checks.length ? fmtAgo(svc.checks[0].ts) : '—';
  const pct = svc.uptime_pct != null ? svc.uptime_pct.toFixed(2) + '%' : '—';
  const pctColor = svc.uptime_pct == null ? '' :
    svc.uptime_pct >= 99 ? 'color:var(--green)' :
    svc.uptime_pct >= 90 ? 'color:var(--yellow)' : 'color:var(--red)';

  return `
    <div class="card">
      <div class="card-header">
        <div>
          <div class="card-name">${svc.name}</div>
          <div class="card-host">${svc.host}</div>
        </div>
        ${renderBadge(svc.status)}
      </div>
      <div class="bars-wrap">
        <div class="bars">${renderBars(svc.checks)}</div>
        <div class="uptime-pct" style="${pctColor}">${pct}</div>
      </div>
      <div class="bar-labels">
        <span>${oldest}</span>
        <span>Now</span>
      </div>
      <div class="incidents">
        <div class="inc-title">Recent incidents</div>
        ${renderIncidents(svc.incidents)}
      </div>
    </div>`;
}

function renderOverall(services) {
  const downs = services.filter(s => s.status === 'down').length;
  const pulse = document.getElementById('pulse');
  const label = document.getElementById('overall-label');
  pulse.className = 'pulse ' + (downs === 0 ? 'up' : downs < services.length ? 'degraded' : 'down');
  label.textContent = downs === 0
    ? 'All Systems Operational'
    : downs === services.length
    ? 'Major Outage'
    : `${downs} service${downs > 1 ? 's' : ''} disrupted`;
}

async function load() {
  try {
    const res = await fetch('/api/status', { cache: 'no-store' });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const data = await res.json();

    const overall = document.getElementById('overall');
    overall.style.display = 'flex';
    document.getElementById('overall-ts').textContent = 'Updated ' + new Date().toLocaleTimeString();
    renderOverall(data.services);

    document.getElementById('services').innerHTML = data.services.map(renderCard).join('');
  } catch (e) {
    document.getElementById('services').innerHTML =
      `<div class="error">Could not reach status API: ${e.message}</div>`;
  }
}

load();
setInterval(load, AUTO_MS);
</script>
</body>
</html>"#;
