# Nest applications

The `apps/` folder is for **local product checkouts only**. Nest git **ignores everything here except this README** — product source is never committed to the framework repo.

Clone a product repo into `apps/<name>/` for side-by-side development with `core/` and `modules/`:

```text
nest/
├── core/crates/
├── modules/crates/
└── apps/
    ├── README.md           # tracked by nest
    └── airtable-sync/      # git clone — ignored by nest
```

## Setup

```bash
git clone https://github.com/pacificnm/airtable-sync.git apps/airtable-sync
```

Framework consumers who do not need Pacific NM products can skip this entirely — `apps/` stays empty (or absent).

## Pacific NM products

| Local path | Repository |
|------------|------------|
| `apps/airtable-sync/` | [github.com/pacificnm/airtable-sync](https://github.com/pacificnm/airtable-sync) |
| `apps/loon/` | [github.com/pacificnm/loon](https://github.com/pacificnm/loon) — [plan](loon/docs/v1.md) (clone into `apps/loon/`) |
| `apps/swift/` | Local checkout — [docs](swift/docs/README.md) (PM + knowledge + AI; reference Tauri desktop app) |

Planned: `kiwi`, `finch`, …

All products follow the [Nest app standard](../docs/app-standard.md): one Rust core, host adapters for CLI / TUI / desktop, and Tauri IPC only at the React webview boundary. **Desktop apps** use `ui/` + `src-tauri/` (Tauri + React + Tailwind). See also [nest-tauri v1 plan](../docs/plan/nest-tauri-v1.md).

### Loon

```bash
git clone https://github.com/pacificnm/loon.git apps/loon
```

See [apps/loon/docs/implementation-v1.md](loon/docs/implementation-v1.md) for build order.

### Swift

Personal project management + knowledge + Ollama assistant. Specs and plans: [apps/swift/docs/](swift/docs/README.md). Scaffold from [templates/desktop/](../templates/desktop/) into `apps/swift/`.

See [apps/swift/docs/README.md](swift/docs/README.md) and [swift-v1 plan](swift/docs/plan/swift-v1.md).

## Build (example)

Every product uses the same **`./build`** commands. See [docs/build.md](../docs/build.md).

```bash
cd apps/airtable-sync
cp config.example.toml config.toml
export AIRTABLE_TOKEN="pat..."
./build build
./build run -- tables
```

| Command | Use when |
|---------|----------|
| `./build dev` | Daily development (Tauri/Vite or cargo run) |
| `./build run` | Launch the app |
| `./build build` | Production artifacts (default) |
| `./build test` | Run tests |
| `./build clean` | Remove build output |

The product repo's `.cargo/config.toml` path-patches Nest crates from this layout (`../../core/…`, `../../modules/…`).

**Dependency rule:** products depend on Nest core and modules only. See [docs/architecture.md](../docs/architecture.md) and [docs/app-standard.md](../docs/app-standard.md).
# sparrow
