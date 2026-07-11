# Rust server with static UI assets

The production system runs as a single Rust server that serves the API, SSE streams, and SvelteKit-built static assets. SvelteKit's Node development server is only used during development; avoiding a Node process in production reduces moving parts on the Raspberry Pi and simplifies recovery.

## Considered Options

- Run Rust backend and SvelteKit/Node server in production: rejected because it adds a second long-running process without a required capability.
- Serve static UI assets from Rust: accepted because the UI can be built ahead of time and the Rust server already owns the local API.

## Consequences

- Live read models use SSE from the Rust server.
- Commands use HTTP POST, not WebSocket messages.
- Mutating local control endpoints and public read-only endpoints can be bound separately.
