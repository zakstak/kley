use askama::Template;
use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse},
};

use crate::compact::CompactConfig;
use crate::web::protocol::PROTOCOL_VERSION;

#[derive(Template)]
#[template(path = "index.html")]
struct ShellTemplate {
    ws_path: &'static str,
    protocol_version: u32,
    #[allow(dead_code)] // Read by Askama template codegen
    context_max_chars: usize,
}

const BINDERY_ICON: &str = include_str!("../../assets/bindery-icon.svg");
const SELF_IMPROVE_PANEL: &str = r#"<script>
(() => {
  const appShell = document.querySelector('[data-testid="app-shell"]');
  const inspector = document.querySelector('[data-testid="inspector-panel"]');
  if (!appShell || !inspector) {
    return;
  }

  const panel = document.createElement('section');
  panel.setAttribute('data-testid', 'self-improve-panel');
  panel.className = 'rounded-lg border border-bdr2 bg-bg2 p-3 mb-3 space-y-3';
  panel.innerHTML = [
    '<div class="flex items-center justify-between gap-2">',
    '  <div class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt3">Self Improve</div>',
    '  <span id="self-improve-state" class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt2">idle</span>',
    '</div>',
    '<p id="self-improve-detail" class="hidden text-[11px] text-txt3"></p>',
    '<div class="grid grid-cols-2 gap-2">',
    '  <label class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt3" for="self-improve-cycles">Cycles</label>',
    '  <label class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt3" for="self-improve-turns">Turns</label>',
    '  <input id="self-improve-cycles" type="number" min="1" max="100" value="5" class="rounded border border-bdr2 bg-bg3 px-2 py-1 text-xs text-txt" />',
    '  <input id="self-improve-turns" type="number" min="1" max="200" value="30" class="rounded border border-bdr2 bg-bg3 px-2 py-1 text-xs text-txt" />',
    '</div>',
    '<div class="flex flex-wrap gap-2">',
    '  <button data-testid="self-improve-start" id="self-improve-start" type="button" class="send-btn h-9 rounded-lg px-3 text-[11px] font-semibold uppercase tracking-[0.08em]">Start</button>',
    '  <button data-testid="self-improve-stop" id="self-improve-stop" type="button" class="abort-btn h-9 rounded-lg border px-3 text-[11px] font-semibold uppercase tracking-[0.08em]">Stop</button>',
    '  <button data-testid="self-improve-restart" id="self-improve-restart" type="button" class="h-9 rounded-lg border border-bdr2 bg-bg3 px-3 text-[11px] font-semibold uppercase tracking-[0.08em] text-txt">Restart</button>',
    '</div>',
    '<div class="space-y-2">',
    '  <div class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt3">Live Log</div>',
    '  <pre id="self-improve-log" class="max-h-48 overflow-y-auto rounded border border-bdr2 bg-bg3 p-2 text-[11px] text-txt2 whitespace-pre-wrap">No run output yet.</pre>',
    '</div>',
    '<div>',
    '  <div class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt3 mb-1">Run History</div>',
    '  <div id="self-improve-history" class="space-y-1 text-xs text-txt2"></div>',
    '</div>',
    '<div>',
    '  <div class="text-[10px] font-mono uppercase tracking-[0.08em] text-txt3 mb-1">Retrospectives</div>',
    '  <div id="self-improve-retros" class="space-y-1 text-xs text-txt2"></div>',
    '</div>',
  ].join('');

  const inspectorEvents = document.getElementById('inspector-events');
  const eventsHeading = inspector.querySelector('.panel-title.mt-4.mb-2');
  if (eventsHeading) {
    inspector.insertBefore(panel, eventsHeading);
  } else if (inspectorEvents) {
    inspector.insertBefore(panel, inspectorEvents);
  } else {
    inspector.appendChild(panel);
  }

  const dom = {
    state: document.getElementById('self-improve-state'),
    detail: document.getElementById('self-improve-detail'),
    cycles: document.getElementById('self-improve-cycles'),
    turns: document.getElementById('self-improve-turns'),
    start: document.getElementById('self-improve-start'),
    stop: document.getElementById('self-improve-stop'),
    restart: document.getElementById('self-improve-restart'),
    log: document.getElementById('self-improve-log'),
    history: document.getElementById('self-improve-history'),
    retros: document.getElementById('self-improve-retros'),
  };

  if (!dom.state || !dom.detail || !dom.cycles || !dom.turns || !dom.start || !dom.stop || !dom.restart || !dom.log || !dom.history || !dom.retros) {
    return;
  }

  const state = {
    socket: null,
    connected: false,
    reconnectDelayMs: 400,
    reconnectTimer: null,
    logTail: [],
    selfImproveDetail: '',
    snapshot: null,
  };

  function renderSelfImproveStatus(label, detail) {
    dom.state.textContent = label || 'idle';
    if (typeof detail === 'string') {
      state.selfImproveDetail = detail.trim();
    }
    if (state.selfImproveDetail) {
      dom.detail.textContent = state.selfImproveDetail;
      dom.detail.classList.remove('hidden');
    } else {
      dom.detail.textContent = '';
      dom.detail.classList.add('hidden');
    }
  }

  function requestId(prefix) {
    if (window.crypto && typeof window.crypto.randomUUID === 'function') {
      return prefix + '-' + window.crypto.randomUUID();
    }
    return prefix + '-' + Date.now() + '-' + Math.random().toString(16).slice(2);
  }

  function wsUrl() {
    const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws';
    return protocol + '://' + window.location.host + '/ws/self-improve';
  }

  function setConnected(value) {
    state.connected = value;
    syncButtons();
  }

  function syncButtons() {
    const running = Boolean(state.snapshot && state.snapshot.active_run);
    dom.start.disabled = !state.connected || running;
    dom.stop.disabled = !state.connected || !running;
    dom.restart.disabled = !state.connected;
  }

  function renderLog(lines) {
    const finalLines = Array.isArray(lines) && lines.length ? lines : ['No run output yet.'];
    dom.log.textContent = finalLines.join('\n');
    dom.log.scrollTop = dom.log.scrollHeight;
  }

  function renderHistory(history) {
    dom.history.innerHTML = '';
    if (!Array.isArray(history) || !history.length) {
      dom.history.textContent = 'No runs yet.';
      return;
    }
    history.slice(0, 8).forEach((run) => {
      const line = document.createElement('p');
      const code = run.exit_code === null || run.exit_code === undefined ? '-' : String(run.exit_code);
      line.textContent = run.run_id + ' | ' + run.outcome + ' | exit=' + code;
      dom.history.appendChild(line);
    });
  }

  function renderRetros(retros) {
    dom.retros.innerHTML = '';
    if (!Array.isArray(retros) || !retros.length) {
      dom.retros.textContent = 'No retrospective records yet.';
      return;
    }
    retros.slice(0, 6).forEach((item, index) => {
      const entry = document.createElement('div');
      const line = document.createElement('p');
      const status = item && item.status ? item.status : 'unknown';
      const cycle = item && item.cycle ? item.cycle : '?';
      const statusDetail = item && item.status_detail ? String(item.status_detail).trim() : '';
      line.textContent = 'cycle ' + cycle + ': ' + status + ' (#' + (index + 1) + ')';
      entry.appendChild(line);
      if (statusDetail) {
        const detail = document.createElement('p');
        detail.className = 'text-[11px] text-txt3';
        detail.textContent = statusDetail;
        entry.appendChild(detail);
      }
      dom.retros.appendChild(entry);
    });
  }

  function applySnapshot(snapshot) {
    state.snapshot = snapshot || null;
    const running = snapshot && snapshot.active_run;
    if (!snapshot) {
      renderSelfImproveStatus('unknown', '');
      renderLog(state.logTail);
      renderHistory([]);
      renderRetros([]);
      syncButtons();
      return;
    }

    if (running) {
      renderSelfImproveStatus(
        running.latest_status || 'running',
        typeof running.latest_detail === 'string' ? running.latest_detail : state.selfImproveDetail
      );
      renderLog(running.log_tail || state.logTail);
    } else {
      renderSelfImproveStatus('idle', state.selfImproveDetail);
      if (state.logTail.length) {
        renderLog(state.logTail);
      } else {
        renderLog(snapshot.recent_logs || []);
      }
    }
    renderHistory(snapshot.history || []);
    renderRetros(snapshot.retrospectives || []);
    syncButtons();
  }

  function appendLog(line) {
    state.logTail.push(line);
    while (state.logTail.length > 300) {
      state.logTail.shift();
    }
    renderLog(state.logTail);
  }

  function send(command) {
    if (!state.socket || state.socket.readyState !== WebSocket.OPEN) {
      return;
    }
    state.socket.send(JSON.stringify(command));
  }

  function handleFrame(frame) {
    if (!frame || typeof frame !== 'object') {
      return;
    }
    if (frame.type === 'response.ok' && frame.data) {
      applySnapshot(frame.data);
      return;
    }
    if (frame.type === 'response.error') {
      renderSelfImproveStatus(
        frame.error && frame.error.code ? frame.error.code : 'error',
        frame.error && frame.error.message ? frame.error.message : ''
      );
      syncButtons();
      return;
    }
    if (frame.type === 'self_improve.snapshot') {
      applySnapshot(frame.data);
      return;
    }
    if (frame.type === 'self_improve.log') {
      appendLog(frame.line || '');
      return;
    }
    if (frame.type === 'self_improve.status') {
      renderSelfImproveStatus(frame.status || 'running', frame.detail || '');
      return;
    }
  }

  function scheduleReconnect() {
    if (state.reconnectTimer) {
      return;
    }
    const delay = state.reconnectDelayMs;
    state.reconnectTimer = window.setTimeout(() => {
      state.reconnectTimer = null;
      connect();
    }, delay);
    state.reconnectDelayMs = Math.min(state.reconnectDelayMs * 2, 4000);
  }

  function connect() {
    if (state.socket && (state.socket.readyState === WebSocket.OPEN || state.socket.readyState === WebSocket.CONNECTING)) {
      return;
    }
    const socket = new WebSocket(wsUrl());
    state.socket = socket;
    setConnected(false);

    socket.addEventListener('open', () => {
      state.reconnectDelayMs = 400;
      setConnected(true);
      send({ type: 'self_improve.get', request_id: requestId('self-get') });
    });

    socket.addEventListener('message', (event) => {
      let payload;
      try {
        payload = JSON.parse(event.data);
      } catch (_error) {
        return;
      }
      handleFrame(payload);
    });

    socket.addEventListener('close', () => {
      setConnected(false);
      scheduleReconnect();
    });

    socket.addEventListener('error', () => {
      setConnected(false);
    });
  }

  dom.start.addEventListener('click', () => {
    const maxCycles = Math.max(1, Math.min(100, Number(dom.cycles.value) || 5));
    const turnsPerCycle = Math.max(1, Math.min(200, Number(dom.turns.value) || 30));
    send({
      type: 'self_improve.start',
      request_id: requestId('self-start'),
      max_cycles: maxCycles,
      turns_per_cycle: turnsPerCycle,
    });
  });

  dom.stop.addEventListener('click', () => {
    send({ type: 'self_improve.stop', request_id: requestId('self-stop') });
  });

  dom.restart.addEventListener('click', () => {
    const maxCycles = Math.max(1, Math.min(100, Number(dom.cycles.value) || 5));
    const turnsPerCycle = Math.max(1, Math.min(200, Number(dom.turns.value) || 30));
    send({
      type: 'self_improve.restart',
      request_id: requestId('self-restart'),
      max_cycles: maxCycles,
      turns_per_cycle: turnsPerCycle,
    });
  });

  syncButtons();
  connect();
})();
</script>"#;

fn inject_self_improve_panel(mut html: String) -> String {
    if let Some(index) = html.rfind("</body>") {
        html.insert_str(index, SELF_IMPROVE_PANEL);
        return html;
    }
    html.push_str(SELF_IMPROVE_PANEL);
    html
}

pub async fn root() -> Result<Html<String>, (StatusCode, &'static str)> {
    ShellTemplate {
        ws_path: "/ws",
        protocol_version: PROTOCOL_VERSION,
        context_max_chars: CompactConfig::default().threshold_chars,
    }
    .render()
    .map(inject_self_improve_panel)
    .map(Html)
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to render web shell",
        )
    })
}

pub async fn bindery_icon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        BINDERY_ICON,
    )
}
