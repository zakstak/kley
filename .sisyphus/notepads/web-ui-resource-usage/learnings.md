- `StateSnapshotData` is the single websocket/bootstrap payload for the top
  shell strip, so adding new status metrics there automatically covers both
  initial hydrate and explicit `state.get` responses.
- The top status area in `templates/index.html` is rendered through dedicated
  normalize/render helpers (`renderContextUsage` plus DOM ids), so matching that
  pattern keeps the UI addition small and consistent.
- Hands-on QA at `http://127.0.0.1:3210` confirmed the live top strip renders
  `RAM`, `CPU`, and `DISK` near the top of the viewport (`y≈117.5`) with live
  values and no browser console errors.
