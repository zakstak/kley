# Bindery UI Port Status

This note explains what from the Bindery-style shell is **actually wired** in
Kley today, and what is still **UI-only**.

## Wired in Kley

These parts are connected to Kley's existing web backend and are not just static
markup:

- Session list rendering and session switching
- Transcript hydration from existing session history
- Live assistant message streaming
- Prompt submit and turn abort
- Tool activity cards and inspector event log
- Session provider/model selection
- Provider API-key login from the web UI

## Not implemented yet (UI-only)

These Bindery-style controls are visible in the shell, but they are not backed
by new Kley behavior yet:

### Fork action

Shown as `Fork (UI-only)`.

**Why it is not implemented:** Kley does not currently have a web-level session
fork workflow or corresponding backend command. Wiring this would require
defining what “fork” means in Kley's session/store model and adding protocol
support for it.

### Inspector drawer toggle

Shown as `Inspector drawer (UI-only)`.

**Why it is not implemented:** Kley's inspector is currently rendered as a
persistent panel, not a drawer with open/close state. The Bindery-style drawer
control was kept as a visual affordance, but the behavior was not added.

### Feed filter chips

Shown as `All`, `Messages`, `Tools`, and `UI` under
`filters: UI-only affordance`.

**Why it is not implemented:** Kley's transcript view currently renders a single
event/message stream. There is no client-side or backend filtering model yet for
separating the feed into Bindery-style filtered views.

### Timeline strip

Shown as the visual timeline bar strip above the main feed.

**Why it is not implemented:** This is currently decorative only. Kley does not
emit Bindery-style timeline segment/grouping metadata that would let the UI
render a meaningful interactive timeline.

### Context meter

Shown in the status strip as a visual meter with `ui-only` text.

**Why it is not implemented:** Kley's page does not currently receive a live
context-usage stream for the browser UI. The meter is only a shell visual right
now, not a real usage indicator.

## Why these features were left UI-only

The goal of this port was to **bring the Bindery UI into Kley while keeping
Kley's current tech stack and behavior model**.

That meant:

- keeping `Rust + Axum + Askama + inline JS`
- preserving Kley's existing websocket protocol and DOM contract
- not inventing backend capabilities that Kley does not already support

So the migration intentionally ported the **shell and visual structure first**,
and left unsupported Bindery interactions clearly labeled rather than pretending
they worked.

## Recent changes

The shell now supports two previously missing workflows directly in the browser:

- choosing the session provider and model via websocket-backed settings updates
- saving OpenAI or ZAI API-key credentials from the web UI

Those flows are now real Kley behavior, not placeholder controls.
