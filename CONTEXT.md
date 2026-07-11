# Carrera Race Timing

Language for a Carrera slot-car race timing and race-control system.

## Language

**Messstelle**:
A physical start/finish light barrier that detects cars for exactly one lane.
_Avoid_: Sektor, Zwischenzeitmessung

**Messereignis**:
A light-barrier signal captured at the lane's **Messstelle** with lane number, timestamp, and signal edge.
_Avoid_: GPIO event, sensor event, Lichtschranken-Event

**Rundenkorrektur**:
A manual post-race adjustment to a lane's race result by whole or fractional laps.
_Avoid_: Abschnittsmessung, Sektorzeit

**Mindestrundenzeit**:
The configurable minimum time between two valid laps on the same lane, defaulting to 3 seconds.
_Avoid_: Debounce-Zeit, Sensor-Sperre

**Gültige Runde**:
A **Messereignis** accepted as a lap because it occurs after the lane's **Mindestrundenzeit**.
_Avoid_: Durchfahrt, Trigger

**Startsequenz**:
The configurable F1-style countdown before the official race start.
_Avoid_: Countdown, Ampelphase

**Fehlstart**:
A **Messereignis** during the **Startsequenz** before the official race start.
_Avoid_: Frühstart, Startfehler

**Fehlstartfolge**:
The configured consequence of a **Fehlstart**: race abort, time penalty, or lane power cut for a configured duration.
_Avoid_: Strafe

**Rennpause**:
A race state where race time is stopped and all lanes are without power, while rolling cars may still produce valid laps.
_Avoid_: Stopp, Unterbrechung

**Chaostaster**:
An emergency button that triggers a **Rennpause**.
_Avoid_: Notschalter, Pausentaster

**Chaosauslösung**:
A **Chaostaster** activation with a configurable consequence for the triggering lane or race control.
_Avoid_: Chaosstrafe

**Wiederanlaufsequenz**:
The restart countdown used to resume a race after a **Rennpause**.
_Avoid_: Restart, Fortsetzung

**Zielbedingung**:
The rule that decides when a race enters its finish phase, either by lap limit or time limit.
_Avoid_: Rennende

**Zielmodus**:
The rule that decides how remaining lanes may finish after the **Zielbedingung** is reached.
_Avoid_: Finish-Modus, Endregel

**Rennprotokoll**:
A chronological durable record of committed race-relevant events from which race state can be reconstructed after failure.
_Avoid_: Zwischenstand, Speicherstand

**RaceEngine**:
The UI-independent race core that processes commands and **Messereignisse** into committed race state.
_Avoid_: Backend, Server, Rennlogik in der UI

**Debug-Bedienung**:
A test-only control surface for simulation and hardware checks, available as screen UI and lightweight CLI.
_Avoid_: Rennleiter-UI, Produktions-UI

## Relationships

- Each lane has exactly one **Messstelle** at start/finish.
- A **Messereignis** belongs to exactly one lane.
- A **Messereignis** is the only automatic source for lap timing.
- A **Gültige Runde** is derived from one accepted **Messereignis**.
- A **Fehlstart** is derived from a **Messereignis** during the **Startsequenz**.
- A **Fehlstart** applies the configured **Fehlstartfolge**.
- A **Chaostaster** creates a **Chaosauslösung**.
- A **Chaosauslösung** creates a **Rennpause** and applies its configured consequence.
- The race-control **Chaostaster** never applies a penalty.
- During a **Rennpause**, race time is stopped but **Messereignisse** can still become **Gültige Runden**.
- A **Wiederanlaufsequenz** resumes a **Rennpause**.
- A race has one **Zielbedingung** and one **Zielmodus**.
- A race has one **Rennprotokoll**.
- A race-relevant event only counts after it is committed to the **Rennprotokoll**.
- The **RaceEngine** owns race rules and is independent of UI and transport.
- **Debug-Bedienung** drives the **RaceEngine** for simulation and hardware testing only.
- A **Rundenkorrektur** is entered manually and may be fractional.

## Example dialogue

> **Dev:** "Do we calculate the lap time from when the UI receives the signal?"
> **Domain expert:** "No — the **Messereignis** timestamp is authoritative. Processing and UI can happen later."
>
> **Dev:** "Are sector times measured by hardware?"
> **Domain expert:** "No — each lane only has one **Messstelle**. Fractional laps are handled as a manual **Rundenkorrektur**."
>
> **Dev:** "Does a false start always abort the race?"
> **Domain expert:** "No — the configured **Fehlstartfolge** decides whether the race aborts, a time penalty is applied, or the lane loses power for a duration."
>
> **Dev:** "Do laps count while the race is paused?"
> **Domain expert:** "Yes, if a car rolls over the **Messstelle** after power is cut, the **Messereignis** can still become a **Gültige Runde**. Only race time is stopped."
>
> **Dev:** "Does every chaos button apply a penalty?"
> **Domain expert:** "No — the race-control **Chaostaster** only pauses the race and never applies a penalty."
>
> **Dev:** "After a power loss, do we trust the last displayed standings?"
> **Domain expert:** "No — race state is rebuilt from the **Rennprotokoll**."
>
> **Dev:** "Can the SvelteKit UI decide whether a lap counts?"
> **Domain expert:** "No — only the **RaceEngine** owns race rules. UI and CLI are just control surfaces."

## Flagged ambiguities

- "möglichst genau" was resolved as millisecond-level timing: measured events should be timestamped with ≤ 1 ms resolution.
- "Rundenabschnitt" was resolved as manual **Rundenkorrektur**, not a measured sector.
- Very short repeated light-barrier triggers are not laps; a configurable **Mindestrundenzeit** decides whether a **Messereignis** becomes a **Gültige Runde**.
- "Pause" was resolved as stopped race time and no lane power, not as ignored timing.
- Race finish rules were split into **Zielbedingung** and **Zielmodus**.
