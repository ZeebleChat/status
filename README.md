# Zstatus — Service Health Dashboard

**Zstatus** is a lightweight monitoring dashboard for the Zeeble platform. It periodically checks the health of core backend services (Auth, DM, Cloud) and displays uptime, response times, and recent incidents on a simple web page.

## Features

- **Real-time status** — Automatically polls configured services every 60 seconds.
- **Uptime percentage** — Calculated from recent checks (default: 90-point history).
- **Incident tracking** — Records outages and resolutions.
- **Embedded HTML** — Self-contained single-page app with inline JavaScript; no external dependencies.
- **Customizable** — Easy to add more services by editing the `SERVICES` array.

## Quick Start

### Prerequisites

- Rust toolchain (if building from source)
- Docker (if using container)

### Running from Source

```bash
cd status
cargo run --release
```

The server starts on `0.0.0.0:8004` by default.

### Using Docker

```bash
cd status
docker build -t zstatus .
docker run -p 8004:8004 zstatus
```

You can also add it to the top-level `docker-compose.yml`.

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8004` | Port to bind the HTTP server. |
| `RUST_LOG` | `zstatus=info` | Log filter (tracing-subscriber). |

### Customizing Monitored Services

Edit `src/main.rs` and modify the `SERVICES` constant:

```rust
const SERVICES: &[ServiceCfg] = &[
    ServiceCfg { key: "api",   name: "Auth API",      host: "api.zeeble.xyz",   url: "http://zbeam:8001/health"   },
    ServiceCfg { key: "dm",    name: "Messaging",     host: "dm.zeeble.xyz",    url: "http://zpulse:3002/health"  },
    ServiceCfg { key: "cloud", name: "Cloud Servers", host: "cloud.zeeble.xyz", url: "http://zcloud:8003/health"  },
    // Add more:
    ServiceCfg { key: "server", name: "Chat Server", host: "server.zeeble.xyz", url: "http://phaselink:4000/health" },
    ServiceCfg { key: "livekit", name: "Voice", host: "livekit.zeeble.xyz", url: "http://livekit:7880/" },
];
```

Fields:
- `key` — Internal identifier (used in JSON output).
- `name` — Display name on the dashboard.
- `host` — Public hostname shown in the UI.
- `url` — Internal URL to poll for health (must return HTTP 2xx on success).

### Polling Interval

The check interval is hardcoded to 60 seconds (`CHECK_SECS`). To change, edit the constant in `src/main.rs`:

```rust
const CHECK_SECS: u64 = 60; // change as needed
```

### History & Incidents

- `HISTORY_MAX` — number of checks to retain per service (default 90). Older entries are evicted.
- `INCIDENT_MAX` — number of incidents to retain (default 10).

Adjust constants near the top of `src/main.rs`.

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | HTML dashboard (self-contained with embedded CSS/JS) |
| `GET` | `/api/status` | JSON status of all services |

### JSON Response Format

```json
{
  "checked_at": 1746960000000,
  "services": [
    {
      "key": "api",
      "name": "Auth API",
      "host": "api.zeeble.xyz",
      "status": "up",
      "uptime_pct": 99.9,
      "checks": [
        { "ts": 1746960000000, "ok": true, "ms": 42 }
      ],
      "incidents": [
        { "start_ts": 1746950000000, "end_ts": 1746950060000 }
      ]
    }
  ]
}
```

## Dashboard UI

The dashboard (`/`) displays:
- Overall status banner ("All Systems Operational", "Degraded", "Outage")
- Service cards with:
  - Status badge (Operational / Outage / Unknown)
  - Uptime percentage (last 90 checks)
  - Bar chart of recent checks (green/red)
  - Up to 5 recent incidents (outage start and resolution times)

The dashboard auto-refreshes every 30 seconds via `setInterval`.

## Security Considerations

- The status page is public by default. If you need to restrict access, place Zstatus behind authentication (e.g., basic auth in Caddy/Nginx) or add a shared secret query param.
- Ensure the health check URLs (`url` field) are only accessible from within the Docker network or internal network, not exposed to the public internet if they reveal internal IPs.
- The service does not require any special privileges beyond being able to reach the health endpoints of the monitored services.

## Adding Zstatus to the Zeeble Stack

1. Add `status` to your top-level `docker-compose.yml`:

```yaml
  zstatus:
    build: ./status
    ports:
      - "8004:8004"
    depends_on:
      - zbeam
      - zpulse
      - zcloud
```

2. Include it in the overall documentation and monitoring checks.

3. Optionally, set up an alerting system that queries `/api/status` and sends notifications (Slack, email) when a service goes down.

## Troubleshooting

- **No data / all unknown**: Check that Zstatus can reach the health endpoints (`url` in `SERVICES`). Use `curl` from inside the container to test connectivity.
- **High latency numbers**: The `ms` field is the round-trip time to the health endpoint. If it's high, investigate network latency or slow backend responses.
- **Services not showing**: Update the `SERVICES` array and restart Zstatus.

## Extending

- **Authentication**: Add middleware to protect endpoints.
- **Metrics**: Export Prometheus metrics by adding `prometheus` crate and `/metrics` endpoint.
- **Notifications**: Hook into incident start/end events to send webhooks.
- **Persistence**: Store history in a database (Redis, PostgreSQL) to survive restarts. Currently history is in-memory only.

## License

MIT
