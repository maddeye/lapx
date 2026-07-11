# SQLite for the Rennprotokoll

Race-relevant events are stored as an append-only **Rennprotokoll** in local SQLite. An event only counts after the SQLite commit succeeds, so race state can be reconstructed from committed events after a power loss; using SQLite avoids a second event-store format and keeps later local persistence in the same durable database.

## Considered Options

- Separate event log file: rejected because it adds parsing, fsync, rotation, and corruption recovery code.
- SQLite append-only table: accepted because transactions and journaling are the simplest durable option on the Raspberry Pi.

## Consequences

- The RaceEngine must persist a race event before treating it as applied.
- SQLite should use robust durability settings for race events, favouring correctness over write throughput.
