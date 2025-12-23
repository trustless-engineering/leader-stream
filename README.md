# Leader Stream

A Rust/Axum service plus WASM client that serves a live view of upcoming Solana slot leaders and their TPU endpoints, with REST + SSE APIs.

## Project layout
- `leader-stream/src/main.rs` – Axum server, APIs, SSE.
- `leader-stream/src/lib.rs`, `leader-stream/src/wasm_app.rs` – WASM client logic.
- `leader-stream/public/` – static assets (CSS, og images, bundled WASM/JS, docs).
- `k8s/` – Kubernetes manifests (ingress, deployment, config/secret generators).

## Getting started
```bash
cd leader-stream
cargo run        # dev server at http://localhost:3000
```

### Build WASM assets manually
```bash
cd leader-stream
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown --lib
wasm-bindgen --target no-modules --out-name app --out-dir ./public ./target/wasm32-unknown-unknown/release/leader_stream.wasm
# ensure wasm_bindgen loader is appended once
if ! grep -q 'wasm_bindgen("/app_bg.wasm");' ./public/app.js; then
  printf '\nwasm_bindgen("/app_bg.wasm");\n' >> ./public/app.js
fi
```
Alternatively, use the Dockerfile or `make wasm` (recreates `public/app.js` & `app_bg.wasm`).

### Run via Docker
```bash
docker build -t leader-stream .
docker run -p 3000:3000 --env-file .env leader-stream
```

## Environment variables
| Name | Purpose | Default |
| --- | --- | --- |
| `SOLANA_RPC_URL` | Solana JSON-RPC endpoint | https://api.mainnet-beta.solana.com |
| `SOLANA_WS_URL` / `SOLANA_WSS_URL` | Websocket RPC endpoint; inferred from RPC if unset | derived from RPC |
| `SOLANA_RPC_X_TOKEN` | Optional `x-token` header for RPC | none |
| `SOLANA_WS_X_TOKEN` | Optional `x-token` for WS; falls back to RPC token | none |
| `PORT` | HTTP listen port | 3000 |
| `RPC_TIMEOUT_MS` | RPC timeout | 10000 |
| `NODE_CACHE_TTL_MS` | Cache TTL for node list | 5000 |
| `SSE_HEARTBEAT_MS` | SSE heartbeat | 15000 |
| `WS_PING_MS` / `GRPC_PING_MS` | WS ping interval | 15000 |
| `LEADER_LOOKAHEAD` | Slots to prefetch | 5000 |
| `TRACK_LOOKAHEAD` | Slots to prefetch per tracked validator | 5000 |
| `STATIC_DIR` | Override static dir | `<repo>/leader-stream/public` |
| `NEXT_PUBLIC_LEADER_STREAM_URL` | Override SSE path injected into HTML | `/api/leader-stream` |

See `.env.example` and `k8s/secret.env.example` for templates.

## API docs
Static docs at `/docs.html` (source: `leader-stream/public/docs.html`). Key endpoints:
- `GET /api/next-leaders?limit=1000`
- `GET /api/current-slot`
- `GET /api/leader-stream?track=<validator>` (SSE)

## Deployment (Kubernetes)
`k8s/` uses Kustomize. Replace image `ghcr.io/trustless-engineering/leader-stream:${GIT_SHA}` and supply your own overlays/secrets:
```bash
cp k8s/secret.env.example k8s/secret.env
kubectl kustomize k8s | kubectl apply -f -
```
Ingress manifests include example domains (`areweslotyet.xyz`); adjust hosts/secret names as needed.

## Frontend assets
`leader-stream/public` built artifacts (`app.js`, `app_bg.wasm`, `*.d.ts`, `styles.css`) are generated and not tracked. CI/Docker rebuilds them; run `make wasm` locally if you need fresh assets.

## Contribution
- Rust: `rustfmt` defaults.
- CSS: 4-space indent, kebab-case classes.
- Tests: `cargo test` (from `leader-stream/`) for API smoke tests.
- Manual validation: load `/`, call `/api/next-leaders`, `/api/current-slot`, and stream `/api/leader-stream`.

## License
AGPL-3.0 except if your name is Mert or you are affiliated with Helius Blockchain Technologies, in which case you can kick rocks.
