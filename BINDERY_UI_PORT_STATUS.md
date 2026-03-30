# Bindery UI Port Status

This note explains what from the Bindery-style shell is **actually wired** in
Kley today, and what is still **UI-only**.

## Wired in Kley

These parts are connected to Kley's existing web backend and are not just static
markup:

- Session list rendering and session switching
- Session metadata surfaces selected status/provider/model/created/updated
  timestamps
- Session list metadata surfaces each session `updated_at`
- Transcript hydration from existing session history
- Live assistant message streaming
- Prompt submit and turn abort
- Tool activity cards and inspector event log
- Tool cards render `tool.completed.edit_observation` details when present
- Transcript filter chips for backend-backed categories (`All`, `Messages`,
  `Tools`)
- Session provider/model selection
- ZAI API-key login from the web UI
- OpenAI browser login from the web UI
- Live context usage meter updates from websocket events

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

### UI feed filter chip

The `UI` chip is still a placeholder (`aria-disabled="true"`) and is not backed
by websocket feed data.

`All`, `Messages`, and `Tools` now filter the transcript client-side using
existing backend-backed transcript/tool event categories.

**Why UI remains unimplemented:** Kley still has no backend event-feed model for
UI-only feed entries, so only the backend-backed categories were activated.

### Timeline strip

Shown as the visual timeline bar strip above the main feed.

**Why it is not implemented:** This is currently decorative only. Kley does not
emit Bindery-style timeline segment/grouping metadata that would let the UI
render a meaningful interactive timeline.

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
- saving ZAI API-key credentials from the web UI
- completing OpenAI browser login from the web UI callback flow
- rendering context usage (percent/chars/tokens) from websocket snapshot + turn
  events

Those flows are now real Kley behavior, not placeholder controls.
