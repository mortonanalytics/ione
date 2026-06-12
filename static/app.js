/* ── Auth UI ── */
const DEMO_WORKSPACE_ID_CONST = '00000000-0000-0000-0000-000000000d30';
const DEMO_WORKSPACE_ID = DEMO_WORKSPACE_ID_CONST;

const userLabelEl = document.getElementById('user-label');
const loginBtn    = document.getElementById('login-btn');
const logoutBtn   = document.getElementById('logout-btn');

async function loadMe() {
  try {
    const data = await apiFetch('/api/v1/me');
    const user = data.user;
    if (user.email === 'default@localhost') {
      userLabelEl.textContent = 'Signed in as default (local)';
      loginBtn.hidden  = true;
      logoutBtn.hidden = true;
    } else {
      userLabelEl.textContent = 'Signed in as ' + (user.displayName || user.email);
      loginBtn.hidden  = true;
      logoutBtn.hidden = false;
    }
  } catch (_) {
    userLabelEl.textContent = '';
    loginBtn.hidden  = false;
    logoutBtn.hidden = true;
  }
}

if (loginBtn) {
  loginBtn.addEventListener('click', () => {
    // Redirect to login with the default Keycloak issuer. Operators can
    // customize IONE_KEYCLOAK_ISSUER or this URL as needed.
    const issuer = encodeURIComponent(
      window.IONE_KEYCLOAK_ISSUER || 'http://localhost:8080/realms/ione'
    );
    window.location.href = '/auth/login?issuer=' + issuer;
  });
}

if (logoutBtn) {
  logoutBtn.addEventListener('click', async () => {
    await fetch('/auth/logout', { method: 'POST' });
    await loadMe();
  });
}

/* ── DOM refs ── */
const convList             = document.getElementById('conv-list');
const newChatBtn           = document.getElementById('new-chat-btn');
const newWorkspaceBtn      = document.getElementById('new-workspace-btn');
const form                 = document.getElementById('chat-form');
const promptEl             = document.getElementById('prompt');
const transcript           = document.getElementById('transcript');
const statusEl             = document.getElementById('status');
const sendBtn              = document.getElementById('send-btn');
const workspaceTrigger     = document.getElementById('workspace-trigger');
const workspaceNameEl      = document.getElementById('workspace-name');
const workspaceDomainEl    = document.getElementById('workspace-domain');
const workspaceMenu        = document.getElementById('workspace-menu');
const workspaceClosedNotice = document.getElementById('workspace-closed-notice');
const newWorkspaceDialog   = document.getElementById('new-workspace-dialog');
const newWorkspaceForm     = document.getElementById('new-workspace-form');
const nwNameInput          = document.getElementById('nw-name');
const nwDomainInput        = document.getElementById('nw-domain');
const nwLifecycleSelect    = document.getElementById('nw-lifecycle');
const nwErrorEl            = document.getElementById('nw-error');
const nwCancelBtn          = document.getElementById('nw-cancel-btn');
const nwSubmitBtn          = document.getElementById('nw-submit-btn');

/* ── State ── */
let activeConvId   = null;   // UUID string or null
let conversations  = [];     // Conversation[] newest-first (all workspaces, filtered client-side)
let workspaces     = [];     // Workspace[]
let activeWorkspace = null;  // Workspace | null
let trackedDemoView = false;

const ACTIVE_WORKSPACE_KEY = 'ione.activeWorkspaceId';

/* ── API helpers ── */

async function apiFetch(path, options = {}) {
  const { onFieldError, skipErrorToast = false, ...fetchOptions } = options || {};
  let resp;
  try {
    resp = await fetch(path, fetchOptions);
  } catch (err) {
    if (!skipErrorToast) {
      showError('network_error', 'Network request failed.', 'Check your connection and try again.');
    }
    throw new ApiError(err.message || 'Network request failed.', 0, null);
  }
  if (!resp.ok) {
    let errorBody = null;
    try { errorBody = await resp.json(); } catch (_) {}
    if (errorBody?.error === 'demo_read_only') {
      showToast('The demo workspace is read-only. Switch to your workspace to make changes.');
    } else if (errorBody?.error === 'mfa_required') {
      window.location.href = '/mfa.html#challenge';
    } else if (errorBody?.error === 'mfa_enrollment_required') {
      window.location.href = '/mfa.html#enroll';
    } else if (errorBody?.field && typeof onFieldError === 'function') {
      onFieldError(errorBody.field, errorBody);
    } else if (!skipErrorToast) {
      showError(
        errorBody?.error || `http_${resp.status}`,
        errorBody?.message || `HTTP ${resp.status}`,
        errorBody?.hint
      );
    }
    throw new ApiError(errorBody?.message || `HTTP ${resp.status}`, resp.status, errorBody);
  }
  if (resp.status === 204) return null;
  const text = await resp.text();
  return text ? JSON.parse(text) : null;
}

class ApiError extends Error {
  constructor(message, status, body = null) {
    super(message);
    this.status = status;
    this.body = body;
  }
}

function track(eventKind, detail = null, workspaceId = null) {
  fetch('/api/v1/telemetry/events', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ eventKind, detail, workspaceId }),
  }).catch(() => {});
}

async function pollOllamaHealth() {
  try {
    return await apiFetch('/api/v1/health/ollama');
  } catch (_err) {
    return null;
  }
}

function renderHealthDot(health) {
  const btn = document.getElementById('health-dot');
  if (!btn) return;
  btn.hidden = false;
  const ok = !!(health && health.ok);
  btn.classList.toggle('health-dot--ok', ok);
  btn.classList.toggle('health-dot--error', !ok);
  btn.dataset.state = ok ? 'ok' : 'error';
}

function renderHealthPanel(health) {
  const body = document.getElementById('health-panel-body');
  if (!body) return;
  if (!health) {
    body.innerHTML = '<p>Could not reach the health endpoint.</p>';
    return;
  }
  const missing = ((health.models && health.models.missing) || []).filter(Boolean);
  if (health.ok) {
    body.innerHTML = `<p>Ollama is reachable at <code>${escapeHtml(health.baseUrl)}</code>. All required models are pulled.</p>`;
    return;
  }
  if (missing.length > 0) {
    const items = missing.map((m) => {
      const cmd = `ollama pull ${m}`;
      return `<li><code>${escapeHtml(m)}</code> <button type="button" class="health-copy" data-cmd="${escapeHtml(cmd)}">Copy pull command</button></li>`;
    }).join('');
    body.innerHTML = `
      <p>Ollama is reachable but some models are missing. Run these in a terminal:</p>
      <ul>${items}</ul>`;
  } else {
    body.innerHTML = `
      <p>Ollama is not reachable at <code>${escapeHtml(health.baseUrl)}</code>.</p>
      <p>Run <code>ollama serve</code>, or set <code>OLLAMA_BASE_URL</code> to the correct host.</p>
      ${health.error ? `<pre>${escapeHtml(health.error)}</pre>` : ''}`;
  }
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#x27;');
}

const PIPELINE_STAGE_LABELS = {
  publish_started: 'Publishing',
  first_event: 'First event',
  first_signal: 'First signal',
  first_survivor: 'First survivor',
  first_decision: 'First decision',
  stall: 'Waiting',
  error: 'Error',
};

function formatRelativeTime(isoString) {
  const then = new Date(isoString);
  const deltaMs = Date.now() - then.getTime();
  const s = Math.round(deltaMs / 1000);
  if (s < 5) return 'just now';
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  return `${d}d ago`;
}

function renderTimelineItem(event) {
  const li = document.createElement('li');
  li.className = `pe-item pe-item--${event.stage}`;
  const label = PIPELINE_STAGE_LABELS[event.stage] || event.stage;
  const time = event.occurredAt ? formatRelativeTime(event.occurredAt) : '';
  const detailTxt = event.detail ? JSON.stringify(event.detail).slice(0, 80) : '';
  li.innerHTML = `
    <span class="pe-stage">${escapeHtml(label)}</span>
    <span class="pe-time">${escapeHtml(time)}</span>
    ${detailTxt ? `<span class="pe-detail">${escapeHtml(detailTxt)}</span>` : ''}
  `;
  return li;
}

function renderChatRemediation(health, activeWorkspace) {
  const el = document.getElementById('chat-remediation');
  const promptEl = document.getElementById('prompt');
  if (!el) return;
  const isDemo = activeWorkspace && activeWorkspace.id === '00000000-0000-0000-0000-000000000d30';
  if (isDemo) {
    el.hidden = true;
    if (promptEl) {
      promptEl.disabled = false;
      promptEl.removeAttribute('aria-describedby');
    }
    return;
  }
  if (!health || health.ok) {
    el.hidden = true;
    if (promptEl) {
      promptEl.disabled = false;
      promptEl.removeAttribute('aria-describedby');
    }
    return;
  }
  const missing = ((health.models && health.models.missing) || []).filter(Boolean);
  const modelName = missing[0] || (health.models && health.models.required && health.models.required[0]) || 'llama3.2:latest';
  const msg = missing.length
    ? `Chat needs the '${escapeHtml(modelName)}' model. Run <code>ollama pull ${escapeHtml(modelName)}</code>, then click retry.`
    : `Chat needs Ollama running. Start it with <code>ollama serve</code>, then click retry.`;
  el.innerHTML = `<p>${msg}</p><button type="button" id="health-retry">Retry</button>`;
  el.hidden = false;
  if (promptEl) {
    promptEl.disabled = true;
    promptEl.setAttribute('aria-describedby', 'chat-remediation');
  }
}

let lastHealth = null;

async function refreshHealth() {
  lastHealth = await pollOllamaHealth();
  renderHealthDot(lastHealth);
  renderChatRemediation(lastHealth, window.activeWorkspace || null);
}

function listConversations() {
  return apiFetch('/api/v1/conversations');
}

function createConversation(title, workspaceId) {
  return apiFetch('/api/v1/conversations', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ title, workspaceId }),
  });
}

function getConversation(id) {
  return apiFetch('/api/v1/conversations/' + id);
}

function postMessage(convId, content) {
  return apiFetch('/api/v1/conversations/' + convId + '/messages', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ content }),
  });
}

function listWorkspaces() {
  return apiFetch('/api/v1/workspaces');
}

function createWorkspace(name, domain, lifecycle, parentId) {
  const body = { name, domain: domain || 'generic', lifecycle };
  if (parentId) body.parentId = parentId;
  return apiFetch('/api/v1/workspaces', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

async function loadWorkspaces() {
  const data = await listWorkspaces();
  workspaces = data.items || [];
  if (!workspaceMenu.hidden) renderWorkspaceMenu();
  return workspaces;
}

function closeWorkspace(id) {
  return apiFetch('/api/v1/workspaces/' + id + '/close', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
  });
}

function listSignals(workspaceId, source, severity) {
  let url = '/api/v1/workspaces/' + workspaceId + '/signals';
  const params = [];
  if (source) params.push('source=' + encodeURIComponent(source));
  if (severity) params.push('severity=' + encodeURIComponent(severity));
  if (params.length) url += '?' + params.join('&');
  return apiFetch(url);
}

function getRuleDiagnostics(workspaceId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/rule-diagnostics', { skipErrorToast: true });
}

/* ── Render helpers ── */

function formatDate(iso) {
  const d = new Date(iso);
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

// Render a message's prefix label + markdown body into `div`, replacing any
// existing content. Shared by appendMessage and rehydrateTranscriptChips so a
// chip re-hydration pass never flattens rendered markdown back to raw text.
function renderMessageInto(div, role, text) {
  div.textContent = '';
  const prefix = (role === 'user' ? 'You: ' : 'Model: ');
  div.dataset.rawText = prefix + text;
  div.dataset.body = text;

  const label = document.createElement('span');
  label.className = 'message-prefix';
  label.textContent = prefix;
  div.appendChild(label);

  const body = document.createElement('span');
  body.className = 'message-body';
  // Marked passes raw HTML through, so its output must be sanitized before it
  // reaches innerHTML — chat content includes model/connector output and is not
  // trusted. DOMPurify strips dangerous tags, attributes, and URL schemes.
  body.innerHTML = DOMPurify.sanitize(marked.parse(text, { breaks: true }));
  div.appendChild(body);
}

function appendMessage(role, text) {
  const div = document.createElement('div');
  div.className = 'message ' + role;
  renderMessageInto(div, role, text);
  transcript.appendChild(div);
  injectResourceChips(div);
  transcript.scrollTop = transcript.scrollHeight;
}

function appendOllamaFailureCard(errorBody, convId, lastPrompt) {
  const div = document.createElement('div');
  div.className = 'message assistant chat-failure-card';
  const pullCommand = errorBody?.pullCommand || (errorBody?.model ? `ollama pull ${errorBody.model}` : 'ollama serve');
  const message = errorBody?.message || 'Ollama is unavailable for this chat request.';
  div.innerHTML = `
    <p>${escapeHtml(message)}</p>
    <p>Run <code>${escapeHtml(pullCommand)}</code>, then retry.</p>
    <button type="button">Retry</button>`;
  const retryBtn = div.querySelector('button');
  retryBtn.addEventListener('click', async () => {
    retryBtn.disabled = true;
    sendBtn.disabled = true;
    setStatus('Loading...');
    try {
      const assistantMsg = await postMessage(convId, lastPrompt);
      div.remove();
      appendMessage(assistantMsg.role, assistantMsg.content);
      clearStatus();
      await refreshHealth();
    } catch (err) {
      const nextBody = err.body || null;
      if (nextBody?.error === 'ollama_unreachable' || nextBody?.error === 'ollama_model_missing') {
        div.remove();
        handleOllamaChatFailure(nextBody, convId, lastPrompt);
      } else {
        setStatus('Error: ' + err.message);
        retryBtn.disabled = false;
      }
    } finally {
      sendBtn.disabled = false;
    }
  });
  transcript.appendChild(div);
  transcript.scrollTop = transcript.scrollHeight;
}

function handleOllamaChatFailure(errorBody, convId, lastPrompt) {
  const model = errorBody.model || '';
  lastHealth = {
    ok: false,
    baseUrl: errorBody.baseUrl,
    models: { required: model ? [model] : [], missing: model ? [model] : [], available: [] },
    error: errorBody.message,
  };
  renderHealthDot(lastHealth);
  renderChatRemediation(lastHealth, window.activeWorkspace);
  appendOllamaFailureCard(errorBody, convId, lastPrompt);
}

function clearTranscript() {
  transcript.innerHTML = '';
}

function setStatus(msg) {
  statusEl.textContent = msg;
}

function clearStatus() {
  statusEl.textContent = '';
}

/* ── Workspace helpers ── */

function workspaceIsClosed(ws) {
  return ws && ws.closedAt != null;
}

function isDemoWorkspace(ws) {
  return ws && ws.id === DEMO_WORKSPACE_ID;
}

function showToast(message, { durationMs = 5000, onRetry = null } = {}) {
  const container = document.getElementById('toast-container');
  if (!container) return;
  const toast = document.createElement('div');
  toast.className = 'toast toast--error';
  const msg = document.createElement('span');
  msg.textContent = message;
  toast.appendChild(msg);
  if (typeof onRetry === 'function') {
    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'toast-retry';
    retry.textContent = 'Retry';
    retry.addEventListener('click', () => {
      toast.remove();
      onRetry();
    });
    toast.appendChild(retry);
  }
  container.appendChild(toast);
  setTimeout(() => toast.remove(), durationMs);
}

function showError(kind, message, hint, onRetry) {
  const full = hint ? `${message} ${hint}` : message;
  showToast(full, { durationMs: 8000, onRetry });
}

function renderChatChips(ws) {
  const el = document.getElementById('chat-chips');
  if (!el) return;
  el.hidden = !isDemoWorkspace(ws);
}

function renderWorkspaceLock(ws) {
  const el = document.getElementById('workspace-lock');
  if (!el) return;
  el.hidden = !isDemoWorkspace(ws);
}

async function fetchActivation(workspaceId, track) {
  try {
    return await apiFetch(`/api/v1/activation?workspaceId=${encodeURIComponent(workspaceId)}&track=${encodeURIComponent(track)}`);
  } catch (_err) {
    return null;
  }
}

async function markActivation(workspaceId, track, stepKey) {
  await apiFetch('/api/v1/activation/events', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspaceId, track, stepKey }),
  }).catch(() => {});
}

async function dismissActivation(workspaceId, track) {
  await apiFetch('/api/v1/activation/dismiss', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspaceId, track }),
  }).catch(() => {});
}

function trackForWorkspace(ws) {
  return ws && ws.id === DEMO_WORKSPACE_ID_CONST ? 'demo_walkthrough' : 'real_activation';
}

async function renderActivationTracker(ws) {
  const section = document.getElementById('activation-tracker');
  if (!section || !ws) return;
  const activationTrack = trackForWorkspace(ws);
  const state = await fetchActivation(ws.id, activationTrack);
  if (!activeWorkspace || activeWorkspace.id !== ws.id) return;
  if (!state || state.dismissed) {
    section.hidden = true;
    return;
  }
  section.hidden = false;
  section.dataset.track = activationTrack;
  document.getElementById('activation-title').textContent =
    activationTrack === 'demo_walkthrough' ? 'Demo walkthrough' : 'Get started';

  const list = document.getElementById('activation-steps');
  list.innerHTML = '';
  const items = Array.isArray(state.items) ? state.items : [];
  const allCompleted = items.length > 0 && items.every((it) => !!it.completedAt);
  items.forEach((it) => {
    const li = document.createElement('li');
    li.className = 'activation-step' + (it.completedAt ? ' activation-step--done' : '');
    li.innerHTML = `<span class="activation-check" aria-hidden="true"></span><span class="activation-label">${escapeHtml(it.label)}</span>`;
    list.appendChild(li);
  });

  const cta = document.getElementById('activation-cta');
  const wasCtaHidden = cta.hidden;
  if (allCompleted && activationTrack === 'demo_walkthrough') {
    cta.hidden = false;
    list.hidden = true;
    if (wasCtaHidden) {
      track('demo_cta_shown', null, ws.id);
    }
  } else {
    cta.hidden = true;
    list.hidden = false;
  }
}

function workspaceLabel(ws) {
  if (workspaceIsClosed(ws)) {
    const date = formatDate(ws.closedAt);
    return ws.name + ' (closed ' + date + ')';
  }
  return ws.name;
}

function setActiveWorkspace(ws) {
  if (workspaceEventSource) {
    workspaceEventSource.close();
    workspaceEventSource = null;
  }

  activeWorkspace = ws;
  window.activeWorkspace = ws;
  if (isDemoWorkspace(ws) && !trackedDemoView) {
    trackedDemoView = true;
    track('demo_viewed', null, ws.id);
  }
  localStorage.setItem(ACTIVE_WORKSPACE_KEY, ws.id);

  workspaceNameEl.textContent = workspaceLabel(ws);
  workspaceDomainEl.textContent = ws.domain || '';
  renderChatChips(ws);
  renderWorkspaceLock(ws);
  renderChatRemediation(lastHealth, ws);

  const isClosed = workspaceIsClosed(ws);
  newChatBtn.hidden = isClosed;
  workspaceClosedNotice.hidden = !isClosed;

  if (workspaceNameEl.classList) {
    workspaceNameEl.classList.toggle('workspace-name--closed', isClosed);
  }

  // Reset active conversation when switching workspaces.
  activeConvId = null;
  clearTranscript();
  clearStatus();

  renderSidebar();

  // Adaptive tab visibility from the workspace's data-presence summary. List
  // items carry no `panels`, so fetch the full workspace when it's absent.
  applyTabVisibility(ws.panels || null);
  if (!ws.panels) {
    apiFetch('/api/v1/workspaces/' + ws.id, { skipErrorToast: true })
      .then((full) => {
        if (activeWorkspace && activeWorkspace.id === ws.id) {
          applyTabVisibility(full.panels || null);
        }
      })
      .catch(() => {});
  }

  // If the connectors tab is active, reload connectors for the new workspace.
  if (activeTab === 'connectors') {
    loadConnectors(ws.id);
    loadPeerBrowser(ws.id);
  }

  if (activeTab === 'map') {
    updateMapLayers(ws.id);
  }

  resetChartPanel();
  if (activeTab === 'chart') {
    updateChartPanel(ws.id);
  }

  resetTablePanel();
  if (activeTab === 'table') {
    updateTablePanel(ws.id);
  }

  resetDocumentPanel();
  if (activeTab === 'document') {
    updateDocumentPanel(ws.id);
  }

  // If the signals tab is active, reload signals for the new workspace.
  if (activeTab === 'signals') {
    loadSignals();
  }

  // If the survivors tab is active, reload roles + survivors for the new workspace.
  if (activeTab === 'survivors') {
    loadRolesIntoFilter(ws.id).then(() => loadSurvivors());
  }

  // If the approvals tab is active, reload approvals for the new workspace.
  if (activeTab === 'approvals') {
    loadApprovals();
  }

  // Always update the pending badge count.
  updateApprovalsBadge(ws.id);
  renderActivationTracker(ws);
}

// TODO: Wire UI-owned activation marks when the UI has explicit survivor detail,
// approval detail, and audit trail open handlers. The current static UI renders
// survivor and approval lists only, and has no audit-trail view to hook.

/* ── Workspace menu ── */

function buildWorkspaceMenuItem(ws) {
  const li = document.createElement('li');
  li.setAttribute('role', 'menuitem');
  li.className = 'ws-menu-item' + (workspaceIsClosed(ws) ? ' ws-menu-item--closed' : '');
  li.dataset.id = ws.id;

  const labelSpan = document.createElement('span');
  labelSpan.className = 'ws-menu-label';
  labelSpan.textContent = workspaceLabel(ws);
  li.appendChild(labelSpan);

  if (!workspaceIsClosed(ws)) {
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'ws-close-btn';
    closeBtn.title = 'Close workspace';
    closeBtn.setAttribute('aria-label', 'Close workspace ' + ws.name);
    closeBtn.textContent = '⎘';

    closeBtn.addEventListener('click', async (e) => {
      e.stopPropagation();
      if (!confirm('Close workspace "' + ws.name + '"?')) return;
      try {
        const updated = await closeWorkspace(ws.id);
        // Update in-place so the same object reference is mutated for downstream callers.
        ws.closedAt = updated.closedAt;
        // Re-render just this item.
        const newLi = buildWorkspaceMenuItem(ws);
        workspaceMenu.replaceChild(newLi, li);
        // If this was the active workspace, refresh the header.
        if (activeWorkspace && activeWorkspace.id === ws.id) {
          setActiveWorkspace(ws);
        }
      } catch (err) {
        alert('Failed to close workspace: ' + err.message);
      }
    });

    li.appendChild(closeBtn);
  }

  li.addEventListener('click', (e) => {
    if (e.target.classList.contains('ws-close-btn')) return;
    setActiveWorkspace(ws);
    closeWorkspaceMenu();
  });

  li.setAttribute('tabindex', '0');
  li.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      if (e.target === li) {
        setActiveWorkspace(ws);
        closeWorkspaceMenu();
      }
    }
    if (e.key === 'Escape') {
      closeWorkspaceMenu();
      workspaceTrigger.focus();
    }
  });

  return li;
}

function renderWorkspaceMenu() {
  workspaceMenu.innerHTML = '';
  workspaces.forEach((ws) => {
    workspaceMenu.appendChild(buildWorkspaceMenuItem(ws));
  });
}

function openWorkspaceMenu() {
  renderWorkspaceMenu();
  workspaceMenu.hidden = false;
  workspaceTrigger.setAttribute('aria-expanded', 'true');
  // Focus first item.
  const first = workspaceMenu.querySelector('[role="menuitem"]');
  if (first) first.focus();
}

function closeWorkspaceMenu() {
  workspaceMenu.hidden = true;
  workspaceTrigger.setAttribute('aria-expanded', 'false');
}

workspaceTrigger.addEventListener('click', () => {
  if (workspaceMenu.hidden) {
    openWorkspaceMenu();
  } else {
    closeWorkspaceMenu();
  }
});

workspaceTrigger.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
    e.preventDefault();
    openWorkspaceMenu();
  }
});

// Close menu on outside click.
document.addEventListener('click', (e) => {
  if (!workspaceMenu.hidden && !document.getElementById('workspace-switcher').contains(e.target)) {
    closeWorkspaceMenu();
  }
});

// Close menu on Escape.
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !workspaceMenu.hidden) {
    closeWorkspaceMenu();
    workspaceTrigger.focus();
  }
});

/* ── Sidebar rendering ── */

function visibleConversations() {
  if (!activeWorkspace) return conversations;
  const wsId = activeWorkspace.id;
  /*
   * TODO: remove client-side filter once GET /api/v1/conversations supports
   * ?workspaceId= server-side filtering. The server currently returns all
   * conversations for the default user regardless of workspace.
   */
  return conversations.filter((c) => c.workspaceId === wsId);
}

function renderSidebar() {
  convList.innerHTML = '';
  visibleConversations().forEach((conv) => {
    const li = buildConvItem(conv);
    convList.appendChild(li);
  });
}

function buildConvItem(conv) {
  const li = document.createElement('li');
  li.className = 'conv-item' + (conv.id === activeConvId ? ' active' : '');
  li.setAttribute('role', 'listitem');
  li.setAttribute('tabindex', '0');
  li.dataset.id = conv.id;

  const titleDiv = document.createElement('div');
  titleDiv.className = 'conv-item-title';
  titleDiv.textContent = conv.title;

  const dateDiv = document.createElement('div');
  dateDiv.className = 'conv-item-date';
  dateDiv.textContent = formatDate(conv.createdAt);

  li.appendChild(titleDiv);
  li.appendChild(dateDiv);

  li.addEventListener('click', () => loadConversation(conv.id));
  li.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      loadConversation(conv.id);
    }
  });

  return li;
}

function setActiveSidebarItem(id) {
  convList.querySelectorAll('.conv-item').forEach((el) => {
    el.classList.toggle('active', el.dataset.id === id);
  });
}

function prependConversation(conv) {
  conversations.unshift(conv);
  // Only show in sidebar if it belongs to the active workspace.
  if (!activeWorkspace || conv.workspaceId === activeWorkspace.id) {
    const li = buildConvItem(conv);
    convList.insertBefore(li, convList.firstChild);
  }
}

function updateSidebarTitle(id, title) {
  const item = conversations.find((c) => c.id === id);
  if (item) item.title = title;
  const li = convList.querySelector('[data-id="' + id + '"]');
  if (li) {
    const titleDiv = li.querySelector('.conv-item-title');
    if (titleDiv) titleDiv.textContent = title;
  }
}

/* ── Load conversation ── */

async function loadConversation(id) {
  clearStatus();
  clearTranscript();
  activeConvId = id;
  setActiveSidebarItem(id);

  try {
    const data = await getConversation(id);
    // Response shape: { conversation, messages: Message[] }
    const msgs = data.messages || [];
    msgs.forEach((msg) => appendMessage(msg.role, msg.content));
  } catch (err) {
    setStatus('Error loading conversation: ' + err.message);
  }
}

/* ── New chat ── */

async function handleNewChat() {
  if (!activeWorkspace || workspaceIsClosed(activeWorkspace)) return;

  clearStatus();
  clearTranscript();
  activeConvId = null;
  setActiveSidebarItem(null);

  try {
    const conv = await createConversation('New chat', activeWorkspace.id);
    activeConvId = conv.id;
    prependConversation(conv);
    setActiveSidebarItem(conv.id);
  } catch (err) {
    setStatus('Error creating conversation: ' + err.message);
  }
}

newChatBtn.addEventListener('click', handleNewChat);

/* ── Send message ── */

form.addEventListener('submit', async (e) => {
  e.preventDefault();
  const content = promptEl.value.trim();
  if (!content) return;

  if (!activeWorkspace || workspaceIsClosed(activeWorkspace)) {
    setStatus('Cannot send — workspace is closed.');
    return;
  }

  clearStatus();

  // Ensure we have an active conversation.
  if (!activeConvId) {
    const title = content.slice(0, 40) || 'New chat';
    try {
      const conv = await createConversation(title, activeWorkspace.id);
      activeConvId = conv.id;
      prependConversation(conv);
      setActiveSidebarItem(conv.id);
    } catch (err) {
      setStatus('Error creating conversation: ' + err.message);
      return;
    }
  }

  const convId = activeConvId;

  // Optimistically append user message.
  appendMessage('user', content);
  promptEl.value = '';
  sendBtn.disabled = true;
  setStatus('Loading…');

  // After first message in a new conversation, update title if still default.
  const conv = conversations.find((c) => c.id === convId);
  const isDefaultTitle = conv && conv.title === 'New chat';
  const derivedTitle = content.slice(0, 40);

  try {
    const assistantMsg = await postMessage(convId, content);
    // assistantMsg is a Message (assistant reply)
    appendMessage(assistantMsg.role, assistantMsg.content);

    if (isDefaultTitle && derivedTitle) {
      updateSidebarTitle(convId, derivedTitle);
    }

    clearStatus();
  } catch (err) {
    const errorBody = err.body || null;
    if (errorBody?.error === 'ollama_unreachable' || errorBody?.error === 'ollama_model_missing') {
      handleOllamaChatFailure(errorBody, convId, content);
      clearStatus();
      return;
    }
    setStatus('Error: ' + err.message);
    // User message stays in transcript so they can retry.
  } finally {
    sendBtn.disabled = false;
  }
});

/* ── Keyboard: shift+enter inserts newline, enter submits ── */

promptEl.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault();
    form.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true }));
  }
});

document.querySelectorAll('#chat-chips .chip').forEach((btn) => {
  btn.addEventListener('click', () => {
    const promptEl = document.getElementById('prompt');
    if (!promptEl) return;
    const prompt = btn.dataset.prompt || btn.textContent.trim();
    track('demo_prompt_clicked', { prompt }, window.activeWorkspace?.id || null);
    promptEl.value = prompt;
    const form = document.getElementById('chat-form');
    form?.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true }));
  });
});

// Wire activation dismiss button.
document.getElementById('activation-dismiss')?.addEventListener('click', async () => {
  const ws = window.activeWorkspace;
  if (!ws) return;
  const track = trackForWorkspace(ws);
  await dismissActivation(ws.id, track);
  document.getElementById('activation-tracker').hidden = true;
});

// Wire activation CTA.
document.getElementById('activation-cta-primary')?.addEventListener('click', async () => {
  track('demo_cta_clicked', null, window.activeWorkspace?.id || null);
  try {
    const created = await createWorkspace('My Workspace', 'generic', 'continuous', null);
    await loadWorkspaces();
    const ws = workspaces.find((item) => item.id === created.id) || created;
    setActiveWorkspace(ws);
  } catch (_err) {
    showToast('Could not create your workspace. Try again.');
  }
});

document.getElementById('activation-cta-secondary')?.addEventListener('click', () => {
  document.getElementById('activation-cta').hidden = true;
  document.getElementById('activation-steps').hidden = false;
});

/* ── New workspace modal ── */

newWorkspaceBtn.addEventListener('click', () => {
  nwNameInput.value = '';
  nwDomainInput.value = '';
  nwLifecycleSelect.value = 'continuous';
  nwErrorEl.hidden = true;
  nwErrorEl.textContent = '';
  nwSubmitBtn.disabled = false;
  newWorkspaceDialog.showModal();
  nwNameInput.focus();
});

nwCancelBtn.addEventListener('click', () => {
  newWorkspaceDialog.close();
});

newWorkspaceDialog.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    newWorkspaceDialog.close();
  }
});

newWorkspaceForm.addEventListener('submit', async (e) => {
  e.preventDefault();
  const name = nwNameInput.value.trim();
  if (!name) {
    nwErrorEl.textContent = 'Name is required.';
    nwErrorEl.hidden = false;
    return;
  }
  const domain = nwDomainInput.value.trim() || 'generic';
  const lifecycle = nwLifecycleSelect.value;

  nwSubmitBtn.disabled = true;
  nwErrorEl.hidden = true;

  try {
    const ws = await createWorkspace(name, domain, lifecycle, null);
    workspaces.push(ws);
    newWorkspaceDialog.close();
    setActiveWorkspace(ws);
  } catch (err) {
    nwErrorEl.textContent = 'Error: ' + err.message;
    nwErrorEl.hidden = false;
    nwSubmitBtn.disabled = false;
  }
});

/* ── Tab state ── */

const tabChat          = document.getElementById('tab-chat');
const tabMap           = document.getElementById('tab-map');
const tabChart         = document.getElementById('tab-chart');
const tabTable         = document.getElementById('tab-table');
const tabDocument      = document.getElementById('tab-document');
const tabConnectors    = document.getElementById('tab-connectors');
const tabSignals       = document.getElementById('tab-signals');
const tabSurvivors     = document.getElementById('tab-survivors');
const tabApprovals     = document.getElementById('tab-approvals');
const tabAudit         = document.getElementById('tab-audit');
const panelChat        = document.getElementById('panel-chat');
const panelMap         = document.getElementById('panel-map');
const panelChart       = document.getElementById('panel-chart');
const panelTable       = document.getElementById('panel-table');
const panelDocument    = document.getElementById('panel-document');
const panelConnectors  = document.getElementById('panel-connectors');
const panelSignals     = document.getElementById('panel-signals');
const panelSurvivors   = document.getElementById('panel-survivors');
const panelApprovals   = document.getElementById('panel-approvals');
const panelAudit       = document.getElementById('panel-audit');

let activeTab = 'chat'; // 'chat' | 'map' | 'chart' | 'table' | 'document' | 'connectors' | 'signals' | 'survivors' | 'approvals' | 'audit'

function switchTab(name) {
  // Stop auto-refresh when leaving a polling tab.
  if (activeTab === 'signals' && name !== 'signals') {
    stopSignalsPolling();
  }
  if (activeTab === 'survivors' && name !== 'survivors') {
    stopSurvivorsPolling();
  }
  if (activeTab === 'approvals' && name !== 'approvals') {
    stopApprovalsPolling();
  }
  if (activeTab === 'audit' && name !== 'audit') {
    stopAuditPolling();
  }

  activeTab = name;

  tabChat.setAttribute('aria-selected', String(name === 'chat'));
  tabChat.classList.toggle('tab--active', name === 'chat');
  panelChat.hidden = name !== 'chat';

  tabMap.setAttribute('aria-selected', String(name === 'map'));
  tabMap.classList.toggle('tab--active', name === 'map');
  panelMap.hidden = name !== 'map';

  tabChart.setAttribute('aria-selected', String(name === 'chart'));
  tabChart.classList.toggle('tab--active', name === 'chart');
  panelChart.hidden = name !== 'chart';

  tabTable.setAttribute('aria-selected', String(name === 'table'));
  tabTable.classList.toggle('tab--active', name === 'table');
  panelTable.hidden = name !== 'table';

  tabDocument.setAttribute('aria-selected', String(name === 'document'));
  tabDocument.classList.toggle('tab--active', name === 'document');
  panelDocument.hidden = name !== 'document';

  tabConnectors.setAttribute('aria-selected', String(name === 'connectors'));
  tabConnectors.classList.toggle('tab--active', name === 'connectors');
  panelConnectors.hidden = name !== 'connectors';

  tabSignals.setAttribute('aria-selected', String(name === 'signals'));
  tabSignals.classList.toggle('tab--active', name === 'signals');
  panelSignals.hidden = name !== 'signals';

  tabSurvivors.setAttribute('aria-selected', String(name === 'survivors'));
  tabSurvivors.classList.toggle('tab--active', name === 'survivors');
  panelSurvivors.hidden = name !== 'survivors';

  tabApprovals.setAttribute('aria-selected', String(name === 'approvals'));
  tabApprovals.classList.toggle('tab--active', name === 'approvals');
  panelApprovals.hidden = name !== 'approvals';

  tabAudit.setAttribute('aria-selected', String(name === 'audit'));
  tabAudit.classList.toggle('tab--active', name === 'audit');
  panelAudit.hidden = name !== 'audit';

  if (name === 'connectors' && activeWorkspace) {
    loadConnectors(activeWorkspace.id);
    loadPeerBrowser(activeWorkspace.id);
  }

  if (name === 'map' && activeWorkspace && mapLoadedWorkspaceId !== activeWorkspace.id) {
    updateMapLayers(activeWorkspace.id);
  } else if (name === 'map' && window.mapInstance) {
    window.mapInstance.resize();
  }

  if (name === 'chart' && activeWorkspace && chartLoadedWorkspaceId !== activeWorkspace.id) {
    updateChartPanel(activeWorkspace.id);
  }

  if (name === 'table' && activeWorkspace && tableLoadedWorkspaceId !== activeWorkspace.id) {
    updateTablePanel(activeWorkspace.id);
  }

  if (name === 'document' && activeWorkspace && documentLoadedWorkspaceId !== activeWorkspace.id) {
    updateDocumentPanel(activeWorkspace.id);
  }

  if (name === 'signals' && activeWorkspace) {
    loadSignals();
    startSignalsPolling();
  }

  if (name === 'survivors' && activeWorkspace) {
    loadRolesIntoFilter(activeWorkspace.id).then(() => loadSurvivors());
    startSurvivorsPolling();
  }

  if (name === 'approvals' && activeWorkspace) {
    loadApprovals();
    startApprovalsPolling();
  }

  if (name === 'audit' && activeWorkspace) {
    loadAuditTrail();
    loadAuditStats();
    startAuditPolling();
  }
}

// Registry driving adaptive tab visibility + keyboard nav. Data-viz tabs
// surface only when the workspace has that data (from its `panels` block); ops
// tabs are always present (grouped to the right in the bar) except Approvals,
// which appears only when something is pending. switchTab() keeps the per-tab
// load side-effects above.
const tabBar = document.getElementById('tab-bar');
const TAB_REGISTRY = [
  { id: 'chat',       el: tabChat,       always: true },
  { id: 'map',        el: tabMap,        show: (p) => p.eventLayers > 0 || p.hasActivePeer },
  { id: 'chart',      el: tabChart,      show: (p) => p.charts > 0 || p.hasActivePeer },
  { id: 'table',      el: tabTable,      show: (p) => p.tables > 0 || p.hasActivePeer },
  { id: 'document',   el: tabDocument,   show: (p) => p.hasActivePeer },
  { id: 'connectors', el: tabConnectors, always: true },
  { id: 'signals',    el: tabSignals,    always: true },
  { id: 'survivors',  el: tabSurvivors,  always: true },
  { id: 'approvals',  el: tabApprovals,  show: (p) => p.approvalsPending > 0 },
  { id: 'audit',      el: tabAudit,      always: true },
];

TAB_REGISTRY.forEach((t) => {
  t.el.addEventListener('click', () => switchTab(t.id));
});

// Apply data-presence visibility from a workspace `panels` block. When panels
// are unknown (null), nothing is hidden — we never hide a tab we can't disprove.
// If the active tab gets hidden, fall back to Chat.
function applyTabVisibility(panels) {
  const known = panels != null;
  const p = panels || {};
  TAB_REGISTRY.forEach((t) => {
    const visible = t.always || !known || (t.show ? t.show(p) : true);
    t.el.hidden = !visible;
  });
  const active = TAB_REGISTRY.find((t) => t.id === activeTab);
  if (active && active.el.hidden) {
    switchTab('chat');
  }
}

// Arrow-key nav across the currently-visible tabs (delegated so it survives
// tabs being hidden/shown). Anchored on the focused tab, not the selected one,
// to match roving-tabindex semantics.
tabBar.addEventListener('keydown', (e) => {
  if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
  const visible = TAB_REGISTRY.filter((t) => !t.el.hidden);
  const idx = visible.findIndex((t) => t.el === e.target);
  if (idx < 0) return;
  e.preventDefault();
  const next = e.key === 'ArrowRight'
    ? visible[Math.min(idx + 1, visible.length - 1)]
    : visible[Math.max(idx - 1, 0)];
  next.el.focus();
  switchTab(next.id);
});

/* ── Map panel ── */

let mapInstance = null;
let mapLayerItems = [];
let mapEventLayers = [];
let mapLoadedWorkspaceId = null;
const mapLayerIds = new Set();
const mapSourceIds = new Set();
const mapEventLayerIds = new Set();
const mapEventSourceIds = new Set();
const RESOURCE_URI_RE = /\b([a-z][a-z0-9+\-.]*:\/\/[^\s"<>]+)/gi;

function initMapPanel() {
  const connectBtn = document.getElementById('map-connect-peer');
  const retryBtn = document.getElementById('map-error-retry');
  const toggleBtn = document.getElementById('map-layer-toggle');
  const list = document.getElementById('map-layer-list');

  connectBtn?.addEventListener('click', () => switchTab('connectors'));
  retryBtn?.addEventListener('click', () => {
    if (activeWorkspace) updateMapLayers(activeWorkspace.id);
  });
  document.getElementById('map-detail-close')?.addEventListener('click', () => {
    const panel = document.getElementById('map-detail-panel');
    if (panel) panel.hidden = true;
    document.getElementById('map-canvas')?.focus();
  });
  toggleBtn?.addEventListener('click', () => {
    const expanded = toggleBtn.getAttribute('aria-expanded') !== 'false';
    toggleBtn.setAttribute('aria-expanded', String(!expanded));
    list.hidden = expanded;
  });
  setupMapKeyboard();
}

async function updateMapLayers(workspaceId) {
  mapLoadedWorkspaceId = workspaceId;
  showMapState('loading');
  const [rastersR, eventsR] = await Promise.allSettled([
    apiFetch(`/api/v1/workspaces/${workspaceId}/map-layers`),
    apiFetch(`/api/v1/workspaces/${workspaceId}/event-layers`, { skipErrorToast: true }),
  ]);
  if (!activeWorkspace || activeWorkspace.id !== workspaceId) return;

  mapLayerItems = rastersR.status === 'fulfilled' ? (rastersR.value.items || []) : [];
  const peersFailed = rastersR.status === 'fulfilled' ? (rastersR.value.peersFailed || []) : [];
  const rasterFail = rastersR.status === 'rejected';
  mapEventLayers = eventsR.status === 'fulfilled' ? (eventsR.value.layers || []) : [];
  const streamsFailed = eventsR.status === 'fulfilled' ? (eventsR.value.streamsFailed || []) : [];
  const eventsTruncated = eventsR.status === 'fulfilled' ? !!eventsR.value.truncated : false;
  const eventsQueriedAt = eventsR.status === 'fulfilled' ? eventsR.value.queriedAt : null;
  const eventFail = eventsR.status === 'rejected';

  if (mapLayerItems.length === 0 && mapEventLayers.length === 0) {
    destroyMap();
    renderLayerControl([], { eventFail, streamsFailed, eventsTruncated });
    renderLegend([], null);
    renderFeedFreshness(null, []);
    renderEventList([]);
    if (rasterFail && eventFail) {
      showMapError('Could not load map. The data sources may be temporarily unavailable.');
    } else if (peersFailed.length > 0) {
      showMapError(`Could not reach any connected peer (${peersFailed.length} failed). The data source may be temporarily unavailable.`);
    } else {
      showMapState('empty');
    }
    rehydrateTranscriptChips();
    return;
  }

  showMapState('canvas');
  renderMapWithLayers({ rasters: mapLayerItems, eventLayers: mapEventLayers });
  renderLayerControl([...mapLayerItems, ...mapEventLayers], { eventFail, streamsFailed, eventsTruncated });
  renderLegend(mapEventLayers, eventsQueriedAt);
  renderFeedFreshness(eventsQueriedAt, mapEventLayers);
  renderEventList(mapEventLayers, { truncated: eventsTruncated });
  if (peersFailed.length > 0) markFailedPeers(peersFailed);
  rehydrateTranscriptChips();
}

function prefersReducedMotion() {
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function renderMapWithLayers({ rasters, eventLayers }) {
  if (typeof maplibregl === 'undefined') {
    showMapError('Map view requires MapLibre GL JS. Check your network connection.');
    return;
  }

  const reduceMotion = prefersReducedMotion();
  const attributions = [
    ...rasters.map((item) => item.meta.attribution),
    ...eventLayers.map((layer) => layer.attribution),
  ].filter(Boolean).map(escapeHtml);

  destroyMap();
  mapInstance = new maplibregl.Map({
    container: 'map-canvas',
    style: { version: 8, sources: {}, layers: [] },
    keyboard: true,
    fadeDuration: reduceMotion ? 0 : 300,
    attributionControl: false,
  });
  window.mapInstance = mapInstance;
  if (attributions.length > 0) {
    mapInstance.addControl(new maplibregl.AttributionControl({
      customAttribution: attributions,
      compact: true,
    }));
  }
  // Both raster and event layers are added inside one 'load' handler so we never
  // call addSource before the style is ready. Event layers are added AFTER
  // rasters so the circles draw on top.
  mapInstance.on('load', () => {
    addRasterLayersToMap(rasters);
    addEventLayersToMap(eventLayers);
    fitMapBounds(rasters, eventLayers, reduceMotion);
  });
}

function addRasterLayersToMap(items) {
  items.forEach((item) => {
    const sourceId = sourceIdForItem(item);
    const layerId = layerIdForItem(item);
    mapSourceIds.add(sourceId);
    mapLayerIds.add(layerId);

    mapInstance.addSource(sourceId, { type: 'raster', tiles: [item.meta.tileUrl], tileSize: 256 });
    mapInstance.addLayer({
      id: layerId,
      type: 'raster',
      source: sourceId,
      paint: { 'raster-opacity': item.meta.opacity ?? 1.0 },
    });
  });
}

function addEventLayersToMap(eventLayers) {
  eventLayers.forEach((layer) => {
    const sourceId = `evt-src-${layer.streamId}`;
    const layerId = `evt-lyr-${layer.streamId}`;
    mapEventSourceIds.add(sourceId);
    mapEventLayerIds.add(layerId);

    mapInstance.addSource(sourceId, { type: 'geojson', data: layer.collection });
    mapInstance.addLayer({
      id: layerId,
      type: 'circle',
      source: sourceId,
      paint: {
        'circle-radius': interpolateSize(layer.style),
        'circle-color': interpolateColor(layer.style),
        'circle-stroke-color': '#fff',
        'circle-stroke-width': 1,
      },
    });
    mapInstance.on('click', layerId, (e) => {
      const r = 22;
      const bbox = [[e.point.x - r, e.point.y - r], [e.point.x + r, e.point.y + r]];
      const features = mapInstance.queryRenderedFeatures(bbox, { layers: [layerId] });
      if (features[0]) openEventPopup(features[0], false);
    });
    mapInstance.on('mouseenter', layerId, () => { mapInstance.getCanvas().style.cursor = 'pointer'; });
    mapInstance.on('mouseleave', layerId, () => { mapInstance.getCanvas().style.cursor = ''; });
  });
}

function interpolateSize(style) {
  if (style && style.sizeField && Array.isArray(style.sizeDomain) && Array.isArray(style.sizeRange)
      && style.sizeDomain.length === 2 && style.sizeRange.length === 2) {
    return ['interpolate', ['linear'], ['get', style.sizeField],
      style.sizeDomain[0], style.sizeRange[0],
      style.sizeDomain[1], style.sizeRange[1]];
  }
  return 6;
}

function interpolateColor(style) {
  if (style && style.colorField && Array.isArray(style.colorDomain) && Array.isArray(style.colorRange)
      && style.colorDomain.length === style.colorRange.length && style.colorDomain.length >= 2
      && style.colorDomain.every((d) => typeof d === 'number')) {
    const expr = ['interpolate', ['linear'], ['get', style.colorField]];
    for (let i = 0; i < style.colorDomain.length; i += 1) {
      expr.push(style.colorDomain[i], style.colorRange[i]);
    }
    return expr;
  }
  return '#e63946';
}

function fitMapBounds(rasters, eventLayers, reduceMotion) {
  const bounds = new maplibregl.LngLatBounds();
  let hasBounds = false;

  rasters.forEach((item) => {
    if (Array.isArray(item.meta.bounds) && item.meta.bounds.length === 4) {
      const [w, s, e, n] = item.meta.bounds;
      bounds.extend([w, s]);
      bounds.extend([e, n]);
      hasBounds = true;
    }
  });

  eventLayers.forEach((layer) => {
    const features = (layer.collection && layer.collection.features) || [];
    features.forEach((f) => {
      const coords = f.geometry && f.geometry.coordinates;
      if (Array.isArray(coords) && coords.length === 2) {
        bounds.extend(coords);
        hasBounds = true;
      }
    });
  });

  if (hasBounds && !bounds.isEmpty()) {
    mapInstance.fitBounds(bounds, { padding: 40, maxZoom: 9, animate: !reduceMotion });
  }
}

function renderLayerControl(items, eventState = {}) {
  const list = document.getElementById('map-layer-list');
  const status = document.getElementById('event-layer-status');
  list.innerHTML = '';
  if (status) {
    status.textContent = eventStatusText(eventState);
  }
  items.forEach((item) => {
    if (item.streamId !== undefined) {
      renderEventLayerRow(list, item);
      return;
    }
    const layerId = layerIdForItem(item);
    const label = item.meta.layerName || item.name || item.uri;
    const li = document.createElement('li');
    li.className = 'layer-row';
    li.dataset.uri = item.uri;
    li.dataset.peerId = item.peerId;
    li.innerHTML = `
      <label>
        <input type="checkbox" checked data-layer-id="${escapeHtml(layerId)}" />
        <span>${escapeHtml(label)}</span>
      </label>
      ${item.meta.opacity != null ? `<input type="range" min="0" max="1" step="0.05"
        value="${escapeHtml(item.meta.opacity)}" data-layer-id="${escapeHtml(layerId)}"
        aria-label="Opacity for ${escapeHtml(label)}" />` : ''}
      <span class="layer-error-icon" hidden aria-label="Tiles unavailable"></span>
    `;
    li.querySelector('input[type=checkbox]').addEventListener('change', (e) => {
      if (mapInstance && mapInstance.getLayer(layerId)) {
        mapInstance.setLayoutProperty(layerId, 'visibility', e.target.checked ? 'visible' : 'none');
      }
    });
    const slider = li.querySelector('input[type=range]');
    if (slider) {
      slider.addEventListener('input', (e) => {
        if (mapInstance && mapInstance.getLayer(layerId)) {
          mapInstance.setPaintProperty(layerId, 'raster-opacity', Number(e.target.value));
        }
      });
    }
    list.appendChild(li);
  });
  if (eventState.eventFail) renderEventLayerError(list);
  (eventState.streamsFailed || []).forEach((stream) => renderStreamErrorRow(list, stream));
}

function renderEventLayerRow(list, layer) {
  const layerId = `evt-lyr-${layer.streamId}`;
  const label = layer.streamName || 'Events';
  const li = document.createElement('li');
  li.className = 'layer-row layer-row--event';
  li.dataset.streamId = layer.streamId;
  li.innerHTML = `
    <label>
      <input type="checkbox" checked data-layer-id="${escapeHtml(layerId)}" />
      <span>${escapeHtml(label)}</span>
    </label>
    <span class="layer-type-badge">Events</span>
  `;
  li.querySelector('input[type=checkbox]').addEventListener('change', (e) => {
    if (mapInstance && mapInstance.getLayer(layerId)) {
      mapInstance.setLayoutProperty(layerId, 'visibility', e.target.checked ? 'visible' : 'none');
    }
    const visible = visibleEventLayers();
    renderLegend(visible, null);
    renderEventList(visible);
  });
  list.appendChild(li);
}

function eventStatusText(eventState) {
  if (eventState.eventFail) return 'Event layers could not be loaded.';
  if ((eventState.streamsFailed || []).length > 0) return `${eventState.streamsFailed.length} event stream could not be rendered.`;
  const featureCount = countEventFeatures(mapEventLayers);
  if (mapEventLayers.length > 0 && featureCount === 0) return 'No events in last 24 h.';
  if (eventState.eventsTruncated) return `Showing ${featureCount} of more than ${featureCount} events - narrow your window.`;
  return '';
}

function renderEventLayerError(list) {
  const li = document.createElement('li');
  li.className = 'layer-row layer-row--error';
  li.innerHTML = `
    <span class="layer-error-icon" aria-hidden="true"></span>
    <span>Event layers unavailable</span>
    <button type="button" class="layer-row-retry">Retry</button>
  `;
  li.querySelector('button').addEventListener('click', () => {
    if (activeWorkspace) updateMapLayers(activeWorkspace.id);
  });
  list.appendChild(li);
}

function renderStreamErrorRow(list, stream) {
  const li = document.createElement('li');
  li.className = 'layer-row layer-row--error';
  li.dataset.streamId = stream.streamId;
  li.innerHTML = `
    <span class="layer-error-icon" aria-hidden="true"></span>
    <span>${escapeHtml(stream.streamName || 'Event stream')} config error</span>
  `;
  li.title = stream.error || 'Event stream config error';
  list.appendChild(li);
}

function visibleEventLayers() {
  return mapEventLayers.filter((layer) => {
    const checkbox = document.querySelector(`#map-layer-list .layer-row--event[data-stream-id="${CSS.escape(layer.streamId)}"] input[type=checkbox]`);
    return !checkbox || checkbox.checked;
  });
}

function countEventFeatures(layers) {
  return layers.reduce((sum, layer) => sum + (((layer.collection || {}).features || []).length), 0);
}

// "Feed last updated {ts}" trust signal. queriedAt is the event-layers query
// time (contract #9). Flags "may be delayed" when the feed is older than the
// staleness window (default 15 min — ~3× a 5-min USGS poll, since the poll
// interval is not surfaced per-layer on the event-layers response).
const FEED_STALE_MS = 15 * 60 * 1000;

function renderFeedFreshness(queriedAt, layers) {
  const el = document.getElementById('event-feed-freshness');
  if (!el) return;
  const hasEvents = (layers || []).some((l) => (((l.collection || {}).features) || []).length > 0);
  if (!queriedAt || !hasEvents) {
    el.hidden = true;
    el.textContent = '';
    el.classList.remove('feed-freshness--stale');
    return;
  }
  el.hidden = false;
  const ts = new Date(queriedAt);
  const stale = Date.now() - ts.getTime() > FEED_STALE_MS;
  el.classList.toggle('feed-freshness--stale', stale);
  el.textContent = stale
    ? `Feed may be delayed · last updated ${formatRelativeTime(queriedAt)}`
    : `Feed last updated ${formatRelativeTime(queriedAt)}`;
}

function renderLegend(layers, queriedAt) {
  const legend = document.getElementById('event-layer-legend');
  if (!legend) return;
  const visible = layers.filter((layer) => ((layer.collection || {}).features || []).length > 0);
  if (visible.length === 0) {
    legend.hidden = true;
    legend.innerHTML = '';
    return;
  }
  legend.hidden = false;
  const footer = queriedAt ? `Last 24 h · Updated ${escapeHtml(formatRelativeTime(queriedAt))}` : 'Last 24 h';
  legend.innerHTML = visible.map((layer) => {
    const style = layer.style || {};
    warnLowContrastColor(style.colorRange);
    const colorStops = Array.isArray(style.colorRange) && style.colorRange.length >= 2 ? style.colorRange : ['#f5d76e', '#d9534f'];
    const gradient = `linear-gradient(90deg, ${colorStops.map(escapeHtml).join(', ')})`;
    const minSize = Array.isArray(style.sizeRange) ? Number(style.sizeRange[0]) : 6;
    const maxSize = Array.isArray(style.sizeRange) ? Number(style.sizeRange[1]) : 12;
    return `
      <div class="event-legend-layer">
        <div class="event-legend-title">${escapeHtml(layer.streamName || 'Events')}</div>
        <div class="event-legend-ramp" aria-hidden="true">
          <span style="width:${Math.max(6, minSize)}px;height:${Math.max(6, minSize)}px"></span>
          <span style="width:${Math.max(8, maxSize)}px;height:${Math.max(8, maxSize)}px"></span>
        </div>
        <div class="event-legend-gradient" style="background:${gradient}" aria-hidden="true"></div>
        ${layer.attribution ? `<div class="event-legend-attribution">${escapeHtml(layer.attribution)}</div>` : ''}
      </div>
    `;
  }).join('') + `<div class="event-legend-footer">${footer}</div>`;
}

function renderEventList(layers, opts = {}) {
  const details = document.getElementById('event-list-disclosure');
  if (!details) return;
  const tbody = details.querySelector('tbody');
  const summary = details.querySelector('summary');
  const rows = [];
  layers.forEach((layer) => {
    ((layer.collection || {}).features || []).forEach((feature) => rows.push({ layer, feature }));
  });
  if (rows.length === 0) {
    details.hidden = true;
    tbody.innerHTML = '';
    return;
  }
  const capped = rows.slice(0, 100);
  details.hidden = false;
  summary.textContent = opts.truncated || rows.length > 100
    ? `Events (${capped.length}+ shown)`
    : `Events (${capped.length})`;
  tbody.innerHTML = '';
  capped.forEach(({ layer, feature }, index) => {
    const props = feature.properties || {};
    const label = eventFeatureLabel(feature, layer);
    const observed = props._observed_at || '';
    const tr = document.createElement('tr');
    tr.innerHTML = `
      <td>${escapeHtml(label)}</td>
      <td>${escapeHtml(observed ? formatDateTime(observed) : '')}</td>
      <td>${escapeHtml(layer.streamName || 'Events')}</td>
      <td><button type="button" data-event-index="${index}">Show on map</button></td>
    `;
    tr.querySelector('button').addEventListener('click', () => showEventOnMap(feature, layer));
    tbody.appendChild(tr);
  });
}

function eventFeatureLabel(feature, layer) {
  const props = feature.properties || {};
  const labelField = layer.style && layer.style.labelField;
  if (labelField && props[labelField] != null) return String(props[labelField]);
  const firstKey = Object.keys(props).find((k) => !k.startsWith('_'));
  if (firstKey) return `${firstKey}: ${props[firstKey]}`;
  return props._event_id || layer.streamName || 'Event';
}

function showEventOnMap(feature, layer) {
  const coords = feature.geometry && feature.geometry.coordinates;
  if (!mapInstance || !Array.isArray(coords) || coords.length !== 2) return;
  mapInstance.flyTo({ center: coords, zoom: Math.max(mapInstance.getZoom(), 8), essential: !prefersReducedMotion() });
  openEventPopup({ ...feature, layer: { id: `evt-lyr-${layer.streamId}` } }, true);
}

// First property whose key matches any of `names` (case-insensitive). Field
// names are the allowlisted view_config.property_fields[].name values.
function firstProp(props, names) {
  const lower = {};
  Object.keys(props).forEach((k) => { lower[k.toLowerCase()] = props[k]; });
  for (const n of names) {
    const v = lower[n.toLowerCase()];
    if (v != null && v !== '') return v;
  }
  return null;
}

// PAGER alert level → human label. USGS green<yellow<orange<red. We pair the
// color chip with a TEXT label so meaning is never conveyed by color alone
// (AC-2.6 / WCAG 1.4.1).
const PAGER_LABELS = {
  green: 'Green — no impact expected',
  yellow: 'Yellow — local impact',
  orange: 'Orange — regional impact',
  red: 'Red — major impact',
};

function openEventPopup(feature, triggeredByKeyboard) {
  const panel = document.getElementById('map-detail-panel');
  if (!panel) return;
  const props = feature.properties || {};

  const mag = firstProp(props, ['magnitude', 'mag']);
  const depth = firstProp(props, ['depth_km', 'depth']);
  const place = firstProp(props, ['place', 'location', 'region']);
  const alert = firstProp(props, ['alert', 'pager']);
  const url = firstProp(props, ['url', 'usgs_url', 'detail']);

  const titleEl = document.getElementById('map-detail-title');
  titleEl.textContent = place != null ? String(place) : (eventFeatureLabel(feature, feature.layer || {}) || 'Event');

  const rows = [];
  if (mag != null) rows.push(`<dt>Magnitude</dt><dd>${escapeHtml(mag)}</dd>`);
  if (depth != null) rows.push(`<dt>Depth (km)</dt><dd>${escapeHtml(depth)}</dd>`);
  if (place != null) rows.push(`<dt>Place</dt><dd>${escapeHtml(place)}</dd>`);

  // PAGER impact assessment: chip carries a text label, never color-alone.
  const alertKey = alert != null ? String(alert).toLowerCase() : null;
  if (alertKey && PAGER_LABELS[alertKey]) {
    rows.push(`<dt>PAGER alert</dt><dd><span class="pager-chip pager-chip--${escapeHtml(alertKey)}">${escapeHtml(PAGER_LABELS[alertKey])}</span></dd>`);
  } else {
    rows.push('<dt>PAGER alert</dt><dd>No impact assessment</dd>');
  }

  if (url != null) {
    rows.push(`<dt>Source</dt><dd><a href="${escapeHtml(url)}" target="_blank" rel="noopener noreferrer">View on USGS</a></dd>`);
  }

  document.getElementById('map-detail-fields').innerHTML = rows.join('');
  panel.hidden = false;
  if (triggeredByKeyboard) {
    setTimeout(() => document.getElementById('map-detail-close')?.focus(), 0);
  }
}

function warnLowContrastColor(colorRange) {
  if (!Array.isArray(colorRange) || colorRange.length === 0) return;
  const contrast = contrastAgainstWhite(colorRange[0]);
  if (contrast != null && contrast < 3) {
    console.warn('event layer legend color has low contrast against white', colorRange[0]);
  }
}

function contrastAgainstWhite(color) {
  const rgb = parseHexColor(color);
  if (!rgb) return null;
  const lum = relativeLuminance(rgb);
  return (1.05) / (lum + 0.05);
}

function parseHexColor(color) {
  const m = String(color).trim().match(/^#([0-9a-f]{3}|[0-9a-f]{6})$/i);
  if (!m) return null;
  const hex = m[1].length === 3 ? m[1].split('').map((c) => c + c).join('') : m[1];
  return [0, 2, 4].map((idx) => parseInt(hex.slice(idx, idx + 2), 16));
}

function relativeLuminance([r, g, b]) {
  const vals = [r, g, b].map((v) => {
    const n = v / 255;
    return n <= 0.03928 ? n / 12.92 : ((n + 0.055) / 1.055) ** 2.4;
  });
  return (0.2126 * vals[0]) + (0.7152 * vals[1]) + (0.0722 * vals[2]);
}

function setupMapKeyboard() {
  const canvas = document.getElementById('map-canvas');
  canvas.addEventListener('keydown', (e) => {
    if (!mapInstance) return;
    const pan = 100;
    if (e.key === 'ArrowRight') mapInstance.panBy([pan, 0]);
    else if (e.key === 'ArrowLeft') mapInstance.panBy([-pan, 0]);
    else if (e.key === 'ArrowUp') mapInstance.panBy([0, -pan]);
    else if (e.key === 'ArrowDown') mapInstance.panBy([0, pan]);
    else if (e.key === '+') mapInstance.zoomIn();
    else if (e.key === '-') mapInstance.zoomOut();
    else if (e.key === 'Escape') document.getElementById('map-layer-toggle').focus();
    else return;
    e.preventDefault();
  });
}

function showMapState(state) {
  document.getElementById('map-loading').hidden = state !== 'loading';
  document.getElementById('map-empty').hidden = state !== 'empty';
  document.getElementById('map-error').hidden = state !== 'error';
  document.getElementById('map-canvas-container').hidden = state !== 'canvas';
}

function showMapError(msg) {
  document.getElementById('map-error-msg').textContent = msg;
  showMapState('error');
}

function clearMapLayers() {
  if (!mapInstance) return;
  [...Array.from(mapEventLayerIds), ...Array.from(mapLayerIds)].reverse().forEach((id) => {
    if (mapInstance.getLayer(id)) mapInstance.removeLayer(id);
    mapLayerIds.delete(id);
    mapEventLayerIds.delete(id);
  });
  [...Array.from(mapEventSourceIds), ...Array.from(mapSourceIds)].reverse().forEach((id) => {
    if (mapInstance.getSource(id)) mapInstance.removeSource(id);
    mapSourceIds.delete(id);
    mapEventSourceIds.delete(id);
  });
}

function destroyMap() {
  if (!mapInstance) return;
  clearMapLayers();
  mapInstance.remove();
  mapInstance = null;
  window.mapInstance = null;
}

function markFailedPeers(failed) {
  const list = document.getElementById('map-layer-list');
  failed.forEach((peer) => {
    const li = document.createElement('li');
    li.className = 'layer-row layer-row--error';
    li.dataset.peerId = peer.peerId;
    li.innerHTML = `<span class="layer-error-icon" aria-hidden="true"></span><span>${escapeHtml(peer.peerName || 'Peer')} unavailable</span>`;
    li.title = peer.error || 'Peer unavailable';
    list.appendChild(li);
  });
}

function sourceIdForItem(item) {
  return 'src-' + layerTokenForItem(item);
}

function layerIdForItem(item) {
  return 'lyr-' + layerTokenForItem(item);
}

function layerTokenForItem(item) {
  let hash = 0;
  const raw = `${item.peerId}:${item.uri}`;
  for (let i = 0; i < raw.length; i += 1) {
    hash = ((hash << 5) - hash + raw.charCodeAt(i)) | 0;
  }
  return `${item.peerId}-${Math.abs(hash)}`;
}

function rehydrateTranscriptChips() {
  transcript.querySelectorAll('.message[data-raw-text]').forEach((el) => {
    const role = el.classList.contains('user') ? 'user' : 'assistant';
    const text = el.dataset.body != null
      ? el.dataset.body
      : el.dataset.rawText.replace(/^(You: |Model: )/, '');
    renderMessageInto(el, role, text);
    injectResourceChips(el);
  });
}

function injectResourceChips(messageEl) {
  const walker = document.createTreeWalker(messageEl, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);

  nodes.forEach((textNode) => {
    const text = textNode.textContent;
    if (!RESOURCE_URI_RE.test(text)) return;
    RESOURCE_URI_RE.lastIndex = 0;

    const frag = document.createDocumentFragment();
    let last = 0;
    let match;
    while ((match = RESOURCE_URI_RE.exec(text)) !== null) {
      const uri = match[1];
      const item = mapLayerItems.find((candidate) => candidate.uri === uri);
      if (!item) continue;

      frag.appendChild(document.createTextNode(text.slice(last, match.index)));
      const chip = document.createElement('button');
      chip.className = 'resource-chip';
      chip.type = 'button';
      chip.dataset.uri = uri;
      chip.textContent = item.meta.layerName || item.name || uri;

      const viewBtn = document.createElement('button');
      viewBtn.className = 'chip-view-map';
      viewBtn.type = 'button';
      viewBtn.dataset.uri = uri;
      viewBtn.setAttribute('aria-label', 'View in Map');
      viewBtn.textContent = 'Map';
      viewBtn.addEventListener('click', () => highlightMapLayer(uri));

      frag.appendChild(chip);
      frag.appendChild(viewBtn);
      last = match.index + match[0].length;
    }
    frag.appendChild(document.createTextNode(text.slice(last)));
    textNode.parentNode.replaceChild(frag, textNode);
  });
}

function highlightMapLayer(uri) {
  switchTab('map');
  const row = document.querySelector(`#map-layer-list .layer-row[data-uri="${CSS.escape(uri)}"]`);
  if (!row) return;
  row.scrollIntoView({ block: 'nearest' });
  row.classList.add('layer-row--highlight');
  setTimeout(() => row.classList.remove('layer-row--highlight'), 1500);
}

initMapPanel();

/* ── Chart panel ── */

let chartLoadedWorkspaceId = null;
let chartItems = [];
let chartInstance = null;

function initChartPanel() {
  document.getElementById('chart-connect-peer')?.addEventListener('click', () => switchTab('connectors'));
  document.getElementById('chart-error-retry')?.addEventListener('click', () => {
    if (activeWorkspace) updateChartPanel(activeWorkspace.id);
  });
  document.getElementById('chart-refresh-btn')?.addEventListener('click', () => {
    if (activeWorkspace) updateChartPanel(activeWorkspace.id);
  });
}

function resetChartPanel() {
  chartLoadedWorkspaceId = null;
  chartItems = [];
  destroyChart();
  const list = document.getElementById('chart-list');
  if (list) list.innerHTML = '';
  const status = document.getElementById('chart-status');
  if (status) status.textContent = '';
  const title = document.getElementById('chart-title');
  if (title) title.textContent = 'Select a chart';
  const source = document.getElementById('chart-source-label');
  if (source) source.textContent = '';
  renderChartTable([]);
  hideChartRenderError();
}

async function updateChartPanel(workspaceId) {
  chartLoadedWorkspaceId = workspaceId;
  showChartState('loading');
  hideChartRenderError();
  let data;
  try {
    data = await apiFetch(`/api/v1/workspaces/${workspaceId}/chart-panels`, { skipErrorToast: true });
  } catch (_err) {
    if (!activeWorkspace || activeWorkspace.id !== workspaceId || chartLoadedWorkspaceId !== workspaceId) return;
    showChartError('Could not load charts. The data sources may be temporarily unavailable.');
    return;
  }
  if (!activeWorkspace || activeWorkspace.id !== workspaceId || chartLoadedWorkspaceId !== workspaceId) return;

  const ioneCharts = data.ioneCharts || data.ione_charts || [];
  const peerCharts = data.peerCharts || data.peer_charts || [];
  const peerErrors = data.peerErrors || data.peer_errors || [];
  chartItems = [...ioneCharts, ...peerCharts];
  renderChartList(chartItems, peerErrors);

  if (chartItems.length === 0) {
    showChartState(peerErrors.length > 0 ? 'error' : 'empty');
    if (peerErrors.length > 0) {
      document.getElementById('chart-error-msg').textContent = `Could not reach any connected peer (${peerErrors.length} failed).`;
    }
    return;
  }

  showChartState('workspace');
}

function renderChartList(items, peerErrors) {
  const list = document.getElementById('chart-list');
  const status = document.getElementById('chart-status');
  list.innerHTML = '';
  status.textContent = peerErrors.length > 0
    ? `${peerErrors.length} peer could not be reached.`
    : '';

  items.forEach((item) => {
    const li = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'chart-row';
    button.dataset.chartId = item.id;
    button.innerHTML = `
      <span class="chart-row-title">${escapeHtml(item.name || 'Chart')}</span>
      <span class="chart-row-meta">${escapeHtml(chartMeta(item))}</span>
    `;
    button.addEventListener('click', () => selectChart(item));
    li.appendChild(button);
    list.appendChild(li);
  });

  peerErrors.forEach((peer) => {
    const li = document.createElement('li');
    li.className = 'chart-row--error';
    li.title = peer.error || 'Peer unavailable';
    li.innerHTML = `<span class="layer-error-icon" aria-hidden="true"></span><span>${escapeHtml(peer.peerName || 'Peer')} unavailable</span>`;
    list.appendChild(li);
  });
}

function chartMeta(item) {
  const source = item.source === 'peer' ? (item.peerName || 'Peer') : 'IONe';
  const type = (item.spec && (item.spec.chartType || item.spec.chart_type)) || 'line';
  return `${source} · ${type}`;
}

async function selectChart(item) {
  document.querySelectorAll('.chart-row--active').forEach((row) => row.classList.remove('chart-row--active'));
  document.querySelector(`.chart-row[data-chart-id="${CSS.escape(item.id)}"]`)?.classList.add('chart-row--active');
  document.getElementById('chart-title').textContent = item.name || 'Chart';
  document.getElementById('chart-source-label').textContent = chartMeta(item);
  hideChartRenderError();
  destroyChart();

  const workspaceId = activeWorkspace?.id;
  if (!workspaceId) return;
  let payload;
  try {
    if (item.source === 'peer') {
      payload = await fetchPeerChart(workspaceId, item);
    } else {
      payload = await fetchIoneChart(workspaceId, item);
    }
  } catch (err) {
    showChartRenderError(err.message || 'Could not load chart data.');
    return;
  }
  if (!activeWorkspace || activeWorkspace.id !== workspaceId) return;
  renderChartPayload(payload.spec, payload.rows || []);
}

async function fetchIoneChart(workspaceId, item) {
  const descriptor = item.descriptor || {};
  const until = new Date();
  const since = new Date(until.getTime() - 30 * 24 * 60 * 60 * 1000);
  const params = new URLSearchParams({
    stream_id: descriptor.streamId || descriptor.stream_id,
    op: descriptor.op || 'count',
    bucket: descriptor.bucket || 'day',
    since: since.toISOString(),
    until: until.toISOString()
  });
  const valuePointer = descriptor.valuePointer || descriptor.value_pointer;
  const groupByPointer = descriptor.groupByPointer || descriptor.group_by_pointer;
  if (valuePointer) params.set('value_pointer', valuePointer);
  if (groupByPointer) params.set('group_by_pointer', groupByPointer);
  if (descriptor.percentile != null) params.set('percentile', String(descriptor.percentile));
  const data = await apiFetch(`/api/v1/workspaces/${workspaceId}/event-aggregates?${params.toString()}`, { skipErrorToast: true });
  return { spec: item.spec, rows: data.rows || [] };
}

async function fetchPeerChart(workspaceId, item) {
  const peerId = item.peerId || item.peer_id;
  const params = new URLSearchParams({ peer_id: peerId, uri: item.uri });
  return apiFetch(`/api/v1/workspaces/${workspaceId}/chart-data?${params.toString()}`, { skipErrorToast: true });
}

function renderChartPayload(spec, rows) {
  const target = document.getElementById('chart-myio-target');
  target.innerHTML = '';
  if (!window.IoneChartAdapter || typeof window.IoneChartAdapter.ioneToMyio !== 'function') {
    showChartRenderError('Chart adapter did not load.');
    return;
  }
  if (typeof window.myIOchart !== 'function') {
    showChartRenderError('Chart engine did not load.');
    return;
  }

  try {
    const config = window.IoneChartAdapter.ioneToMyio(spec, rows);
    const rect = target.getBoundingClientRect();
    chartInstance = new window.myIOchart({
      element: target,
      config,
      width: rect.width || 720,
      height: Math.max(320, rect.height || 360)
    });
    chartInstance.on?.('error', (event) => {
      showChartRenderError(event?.message || 'Chart render failed.');
    });
    renderChartTable(rows);
  } catch (err) {
    showChartRenderError(err.message || 'Chart render failed.');
  }
}

function renderChartTable(rows) {
  const details = document.getElementById('chart-data-disclosure');
  const thead = details?.querySelector('thead');
  const tbody = details?.querySelector('tbody');
  if (!details || !thead || !tbody) return;
  const safeRows = Array.isArray(rows) ? rows : [];
  if (safeRows.length === 0) {
    details.hidden = true;
    thead.innerHTML = '';
    tbody.innerHTML = '';
    return;
  }
  const keys = Array.from(new Set(safeRows.flatMap((row) => Object.keys(row || {}))));
  details.hidden = false;
  details.querySelector('summary').textContent = `Data (${safeRows.length})`;
  thead.innerHTML = `<tr>${keys.map((key) => `<th scope="col">${escapeHtml(labelizeKey(key))}</th>`).join('')}</tr>`;
  tbody.innerHTML = '';
  safeRows.forEach((row) => {
    const tr = document.createElement('tr');
    tr.innerHTML = keys.map((key) => `<td>${escapeHtml(formatChartCell(row[key]))}</td>`).join('');
    tbody.appendChild(tr);
  });
}

function labelizeKey(key) {
  return String(key).replace(/([a-z])([A-Z])/g, '$1 $2').replace(/_/g, ' ');
}

function formatChartCell(value) {
  if (value == null) return '';
  if (typeof value === 'number') return Number.isInteger(value) ? String(value) : value.toFixed(3);
  return String(value);
}

function destroyChart() {
  if (chartInstance) {
    try { chartInstance.destroy(); } catch (_) {}
    chartInstance = null;
  }
  const target = document.getElementById('chart-myio-target');
  if (target) target.innerHTML = '';
}

function showChartState(state) {
  document.getElementById('chart-loading').hidden = state !== 'loading';
  document.getElementById('chart-empty').hidden = state !== 'empty';
  document.getElementById('chart-error').hidden = state !== 'error';
  document.getElementById('chart-workspace').hidden = state !== 'workspace';
}

function showChartError(message) {
  document.getElementById('chart-error-msg').textContent = message;
  showChartState('error');
}

function showChartRenderError(message) {
  const el = document.getElementById('chart-render-error');
  el.textContent = message;
  el.hidden = false;
}

function hideChartRenderError() {
  const el = document.getElementById('chart-render-error');
  if (!el) return;
  el.textContent = '';
  el.hidden = true;
}

initChartPanel();

/* ── Table panel ── */

let tableLoadedWorkspaceId = null;
let tableItems = [];
let tableActiveItem = null;
let tableRequestController = null;
let tablePeerCache = null;
const tableState = {
  page: 1,
  perPage: 25,
  sortBy: '_observed_at',
  sortDir: 'desc',
  filters: {},
  totalCount: 0,
  truncated: false,
};

function initTablePanel() {
  document.getElementById('table-connect-peer')?.addEventListener('click', () => switchTab('connectors'));
  document.getElementById('table-error-retry')?.addEventListener('click', () => {
    if (activeWorkspace) updateTablePanel(activeWorkspace.id);
  });
  document.getElementById('table-refresh-btn')?.addEventListener('click', () => {
    if (activeWorkspace) updateTablePanel(activeWorkspace.id);
  });
  document.getElementById('table-prev-page')?.addEventListener('click', () => {
    if (tableState.page <= 1) return;
    tableState.page -= 1;
    refreshActiveTable();
  });
  document.getElementById('table-next-page')?.addEventListener('click', () => {
    tableState.page += 1;
    refreshActiveTable();
  });
  document.getElementById('table-per-page')?.addEventListener('change', (event) => {
    tableState.perPage = Number(event.target.value) || 25;
    tableState.page = 1;
    refreshActiveTable();
  });
  document.getElementById('table-clear-filters')?.addEventListener('click', () => {
    tableState.filters = {};
    tableState.page = 1;
    refreshActiveTable();
  });
}

function resetTablePanel() {
  tableLoadedWorkspaceId = null;
  tableItems = [];
  tableActiveItem = null;
  tablePeerCache = null;
  tableState.page = 1;
  tableState.perPage = 25;
  tableState.sortBy = '_observed_at';
  tableState.sortDir = 'desc';
  tableState.filters = {};
  tableState.totalCount = 0;
  tableState.truncated = false;
  if (tableRequestController) {
    tableRequestController.abort();
    tableRequestController = null;
  }
  const list = document.getElementById('table-list');
  if (list) list.innerHTML = '';
  const status = document.getElementById('table-status');
  if (status) status.textContent = '';
  const title = document.getElementById('table-title');
  if (title) title.textContent = 'Select a table';
  const source = document.getElementById('table-source-label');
  if (source) source.textContent = '';
  const region = document.getElementById('table-render-region');
  if (region) region.innerHTML = '';
  updateTablePager();
  hideTableRenderError();
}

async function updateTablePanel(workspaceId) {
  tableLoadedWorkspaceId = workspaceId;
  tableActiveItem = null;
  tablePeerCache = null;
  showTableState('loading');
  hideTableRenderError();
  const controller = replaceTableController();
  let data;
  try {
    data = await apiFetch(`/api/v1/workspaces/${workspaceId}/table-panels`, {
      skipErrorToast: true,
      signal: controller.signal
    });
  } catch (_err) {
    if (controller.signal.aborted || !activeWorkspace || activeWorkspace.id !== workspaceId || tableLoadedWorkspaceId !== workspaceId) return;
    showTableError('Could not load tables. The data sources may be temporarily unavailable.');
    return;
  }
  if (!activeWorkspace || activeWorkspace.id !== workspaceId || tableLoadedWorkspaceId !== workspaceId) return;

  const ioneTables = data.ioneTables || data.ione_tables || [];
  const peerTables = data.peerTables || data.peer_tables || [];
  const peerErrors = data.peerErrors || data.peer_errors || [];
  tableItems = [...ioneTables, ...peerTables];
  renderTableList(tableItems, peerErrors);

  if (tableItems.length === 0) {
    showTableState(peerErrors.length > 0 ? 'error' : 'empty');
    if (peerErrors.length > 0) {
      document.getElementById('table-error-msg').textContent = `Could not reach any connected peer (${peerErrors.length} failed).`;
    }
    return;
  }

  showTableState('workspace');
}

function renderTableList(items, peerErrors) {
  const list = document.getElementById('table-list');
  const status = document.getElementById('table-status');
  list.innerHTML = '';
  status.textContent = peerErrors.length > 0
    ? `${peerErrors.length} peer could not be reached.`
    : '';

  items.forEach((item) => {
    const li = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'table-row';
    button.dataset.tableId = item.id;
    button.innerHTML = `
      <span class="table-row-title">${escapeHtml(item.name || 'Table')}</span>
      <span class="table-row-meta">${escapeHtml(tableMeta(item))}</span>
    `;
    button.addEventListener('click', () => selectTable(item));
    li.appendChild(button);
    list.appendChild(li);
  });

  peerErrors.forEach((peer) => {
    const li = document.createElement('li');
    li.className = 'table-row--error';
    li.title = peer.error || 'Peer unavailable';
    li.innerHTML = `<span class="layer-error-icon" aria-hidden="true"></span><span>${escapeHtml(peer.peerName || 'Peer')} unavailable</span>`;
    list.appendChild(li);
  });
}

function tableMeta(item) {
  return item.source === 'peer' ? (item.peerName || item.peer_name || 'Peer') : 'IONe';
}

async function selectTable(item) {
  tableActiveItem = item;
  tablePeerCache = null;
  tableState.page = 1;
  tableState.perPage = Number(document.getElementById('table-per-page')?.value) || 25;
  tableState.sortBy = item.source === 'peer' ? firstPeerSortColumn(item) : '_observed_at';
  tableState.sortDir = item.source === 'peer' ? 'asc' : 'desc';
  tableState.filters = {};
  document.querySelectorAll('.table-row--active').forEach((row) => row.classList.remove('table-row--active'));
  document.querySelector(`.table-row[data-table-id="${CSS.escape(item.id)}"]`)?.classList.add('table-row--active');
  document.getElementById('table-title').textContent = item.name || 'Table';
  document.getElementById('table-source-label').textContent = tableMeta(item);
  hideTableRenderError();
  await refreshActiveTable();
}

function firstPeerSortColumn(_item) {
  return '';
}

async function refreshActiveTable() {
  if (!tableActiveItem || !activeWorkspace) return;
  if (tableActiveItem.source === 'peer') {
    await refreshPeerTable(activeWorkspace.id, tableActiveItem);
  } else {
    await refreshIoneTable(activeWorkspace.id, tableActiveItem);
  }
}

async function refreshIoneTable(workspaceId, item) {
  const controller = replaceTableController();
  showTableRenderLoading();
  const streamId = item.streamId || item.stream_id;
  const params = new URLSearchParams({
    stream_id: streamId,
    page: String(tableState.page),
    per_page: String(tableState.perPage),
    sort_by: tableState.sortBy || '_observed_at',
    sort_dir: tableState.sortDir || 'desc',
  });
  const activeFilter = firstActiveFilter();
  if (activeFilter) {
    params.set('filter_col', activeFilter.name);
    params.set('filter_val', activeFilter.value);
  }
  try {
    const data = await apiFetch(`/api/v1/workspaces/${workspaceId}/event-table?${params.toString()}`, {
      skipErrorToast: true,
      signal: controller.signal
    });
    if (controller.signal.aborted || !activeWorkspace || activeWorkspace.id !== workspaceId || tableActiveItem?.id !== item.id) return;
    const columns = normalizeColumns(data.columns || []);
    const rows = Array.isArray(data.rows) ? data.rows : [];
    tableState.totalCount = Number(data.totalCount || data.total_count || rows.length);
    tableState.truncated = !!data.truncated;
    renderTable(columns, rows, { totalCount: tableState.totalCount });
    hideTableRenderError();
  } catch (err) {
    if (controller.signal.aborted) return;
    showTableRenderError(err.message || 'Could not load table data.');
  }
}

async function refreshPeerTable(workspaceId, item) {
  try {
    if (!tablePeerCache) {
      const controller = replaceTableController();
      showTableRenderLoading();
      const peerId = item.peerId || item.peer_id;
      const params = new URLSearchParams({ peer_id: peerId, uri: item.uri });
      const data = await apiFetch(`/api/v1/workspaces/${workspaceId}/table-data?${params.toString()}`, {
        skipErrorToast: true,
        signal: controller.signal
      });
      if (controller.signal.aborted || !activeWorkspace || activeWorkspace.id !== workspaceId || tableActiveItem?.id !== item.id) return;
      tablePeerCache = {
        columns: normalizeColumns(data.schema || []),
        rows: Array.isArray(data.rows) ? data.rows : []
      };
      if (!tableState.sortBy && tablePeerCache.columns[0]) {
        tableState.sortBy = tablePeerCache.columns[0].name;
      }
    }
    const sliced = slicePeerRows(tablePeerCache.columns, tablePeerCache.rows);
    renderTable(tablePeerCache.columns, sliced.rows, { totalCount: sliced.totalCount });
    hideTableRenderError();
  } catch (err) {
    if (err.name === 'AbortError') return;
    showTableRenderError(err.message || 'Could not load table data.');
  }
}

function replaceTableController() {
  if (tableRequestController) tableRequestController.abort();
  tableRequestController = new AbortController();
  return tableRequestController;
}

function showTableRenderLoading() {
  const live = document.getElementById('table-render-live');
  if (live) live.textContent = 'Loading table rows';
}

function normalizeColumns(columns) {
  return (Array.isArray(columns) ? columns : [])
    .map((col) => {
      const name = col.name || col.key || col.id;
      if (!name) return null;
      return {
        name: String(name),
        label: col.label || labelizeKey(name),
        type: col.type || col.columnType || col.column_type || 'string',
        pointer: col.pointer ?? null
      };
    })
    .filter(Boolean);
}

function renderTable(columns, rows, { totalCount }) {
  const region = document.getElementById('table-render-region');
  const live = document.getElementById('table-render-live');
  if (!region) return;
  const safeRows = Array.isArray(rows) ? rows : [];
  const safeColumns = Array.isArray(columns) ? columns : [];
  if (safeColumns.length === 0) {
    region.innerHTML = '<p class="table-empty-copy">No columns available.</p>';
    updateTablePager();
    return;
  }

  const table = document.createElement('table');
  table.className = 'data-table';
  const caption = document.createElement('caption');
  caption.textContent = `${Number(totalCount || safeRows.length).toLocaleString()} rows`;
  table.appendChild(caption);

  const thead = document.createElement('thead');
  const headerRow = document.createElement('tr');
  safeColumns.forEach((column) => {
    const th = document.createElement('th');
    th.scope = 'col';
    th.setAttribute('aria-sort', tableAriaSort(column.name));
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'table-sort-btn';
    button.textContent = column.label || column.name;
    button.addEventListener('click', () => sortTableBy(column.name));
    th.appendChild(button);
    headerRow.appendChild(th);
  });
  thead.appendChild(headerRow);

  const filterRow = document.createElement('tr');
  filterRow.className = 'table-filter-row';
  safeColumns.forEach((column) => {
    const td = document.createElement('td');
    const input = document.createElement('input');
    input.type = 'search';
    input.value = tableState.filters[column.name] || '';
    input.placeholder = 'Filter';
    input.setAttribute('aria-label', `Filter ${column.label || column.name}`);
    input.addEventListener('input', () => updateTableFilter(column.name, input.value));
    td.appendChild(input);
    filterRow.appendChild(td);
  });
  thead.appendChild(filterRow);
  table.appendChild(thead);

  const tbody = document.createElement('tbody');
  safeRows.forEach((row) => {
    const tr = document.createElement('tr');
    safeColumns.forEach((column) => {
      const td = document.createElement('td');
      const value = row ? row[column.name] : null;
      td.textContent = formatTableCell(value, column.type);
      if (column.type === 'datetime' && value) {
        td.title = String(value);
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });
  table.appendChild(tbody);
  region.innerHTML = '';
  region.appendChild(table);
  if (live) live.textContent = `${safeRows.length} rows visible`;
  updateTablePager();
}

function tableAriaSort(columnName) {
  if (tableState.sortBy !== columnName) return 'none';
  return tableState.sortDir === 'desc' ? 'descending' : 'ascending';
}

function sortTableBy(columnName) {
  if (tableState.sortBy === columnName) {
    tableState.sortDir = tableState.sortDir === 'desc' ? 'asc' : 'desc';
  } else {
    tableState.sortBy = columnName;
    tableState.sortDir = 'asc';
  }
  tableState.page = 1;
  refreshActiveTable();
}

let tableFilterTimer = null;
function updateTableFilter(columnName, value) {
  if (value) {
    if (tableActiveItem?.source !== 'peer') {
      tableState.filters = {};
    }
    tableState.filters[columnName] = value;
  } else {
    delete tableState.filters[columnName];
  }
  tableState.page = 1;
  clearTimeout(tableFilterTimer);
  tableFilterTimer = setTimeout(() => refreshActiveTable(), 200);
}

function firstActiveFilter() {
  const entry = Object.entries(tableState.filters).find(([, value]) => String(value).length > 0);
  return entry ? { name: entry[0], value: String(entry[1]) } : null;
}

function slicePeerRows(columns, rows) {
  const filtered = rows.filter((row) => {
    return Object.entries(tableState.filters).every(([key, filter]) => {
      if (!filter) return true;
      return String(row?.[key] ?? '').toLocaleLowerCase().includes(String(filter).toLocaleLowerCase());
    });
  });
  const sortColumn = columns.find((column) => column.name === tableState.sortBy) || columns[0];
  if (sortColumn) {
    filtered.sort((a, b) => compareTableValues(a?.[sortColumn.name], b?.[sortColumn.name], sortColumn.type));
    if (tableState.sortDir === 'desc') filtered.reverse();
  }
  const start = (tableState.page - 1) * tableState.perPage;
  const end = start + tableState.perPage;
  const pageRows = filtered.slice(start, end);
  tableState.totalCount = filtered.length;
  tableState.truncated = end < filtered.length;
  return { rows: pageRows, totalCount: filtered.length };
}

function compareTableValues(a, b, type) {
  if (a == null && b == null) return 0;
  if (a == null) return 1;
  if (b == null) return -1;
  if (type === 'number') {
    const an = Number(a);
    const bn = Number(b);
    if (!Number.isNaN(an) && !Number.isNaN(bn)) return an - bn;
  }
  if (type === 'datetime') {
    const at = Date.parse(a);
    const bt = Date.parse(b);
    if (!Number.isNaN(at) && !Number.isNaN(bt)) return at - bt;
  }
  return String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: 'base' });
}

function formatTableCell(value, type) {
  if (value == null) return '';
  if (type === 'datetime') {
    const d = new Date(value);
    if (!Number.isNaN(d.getTime())) return d.toLocaleString();
  }
  if (type === 'number' && typeof value === 'number') {
    return Number.isInteger(value) ? String(value) : value.toFixed(3);
  }
  return String(value);
}

function updateTablePager() {
  const pageStatus = document.getElementById('table-page-status');
  const prev = document.getElementById('table-prev-page');
  const next = document.getElementById('table-next-page');
  const perPage = document.getElementById('table-per-page');
  if (pageStatus) {
    const total = Number(tableState.totalCount || 0).toLocaleString();
    pageStatus.textContent = `Page ${tableState.page} · ${total} rows`;
  }
  if (prev) prev.disabled = tableState.page <= 1;
  if (next) next.disabled = !tableState.truncated;
  if (perPage) perPage.value = String(tableState.perPage);
}

function showTableState(state) {
  document.getElementById('table-loading').hidden = state !== 'loading';
  document.getElementById('table-empty').hidden = state !== 'empty';
  document.getElementById('table-error').hidden = state !== 'error';
  document.getElementById('table-workspace').hidden = state !== 'workspace';
}

function showTableError(message) {
  document.getElementById('table-error-msg').textContent = message;
  showTableState('error');
}

function showTableRenderError(message) {
  const el = document.getElementById('table-render-error');
  el.textContent = message;
  el.hidden = false;
}

function hideTableRenderError() {
  const el = document.getElementById('table-render-error');
  if (!el) return;
  el.textContent = '';
  el.hidden = true;
}

initTablePanel();

/* ── Document panel ── */

const DOCUMENT_IFRAME_SANDBOX = 'allow-downloads allow-same-origin';
const DOCUMENT_EMBED_TIMEOUT_MS = 3000;

let documentLoadedWorkspaceId = null;
let documentItems = [];
let documentActiveItem = null;
let documentRequestController = null;
let documentEmbedTimer = null;

function initDocumentPanel() {
  document.getElementById('document-connect-peer')?.addEventListener('click', () => switchTab('connectors'));
  document.getElementById('document-error-retry')?.addEventListener('click', () => {
    if (activeWorkspace) updateDocumentPanel(activeWorkspace.id);
  });
  document.getElementById('document-refresh-btn')?.addEventListener('click', () => {
    if (activeWorkspace) updateDocumentPanel(activeWorkspace.id);
  });
}

function resetDocumentPanel() {
  documentLoadedWorkspaceId = null;
  documentItems = [];
  documentActiveItem = null;
  if (documentRequestController) {
    documentRequestController.abort();
    documentRequestController = null;
  }
  clearDocumentEmbed();
  const list = document.getElementById('document-list');
  if (list) list.innerHTML = '';
  const status = document.getElementById('document-status');
  if (status) status.textContent = '';
  const title = document.getElementById('document-title');
  if (title) title.textContent = 'Select a document';
  const source = document.getElementById('document-source-label');
  if (source) source.textContent = '';
  const live = document.getElementById('document-render-live');
  if (live) live.textContent = '';
}

async function updateDocumentPanel(workspaceId) {
  documentLoadedWorkspaceId = workspaceId;
  documentActiveItem = null;
  showDocumentState('loading');
  resetDocumentRender();
  const controller = replaceDocumentController();
  let data;
  try {
    data = await apiFetch(`/api/v1/workspaces/${workspaceId}/document-panels`, {
      skipErrorToast: true,
      signal: controller.signal
    });
  } catch (_err) {
    if (controller.signal.aborted || !activeWorkspace || activeWorkspace.id !== workspaceId || documentLoadedWorkspaceId !== workspaceId) return;
    showDocumentError('Could not load documents. The data sources may be temporarily unavailable.');
    return;
  }
  if (!activeWorkspace || activeWorkspace.id !== workspaceId || documentLoadedWorkspaceId !== workspaceId) return;

  documentItems = (data.peerDocuments || data.peer_documents || []).map(normalizeDocumentItem);
  const peerErrors = data.peerErrors || data.peer_errors || [];
  renderDocumentList(documentItems, peerErrors);

  if (documentItems.length === 0) {
    showDocumentState(peerErrors.length > 0 ? 'error' : 'empty');
    if (peerErrors.length > 0) {
      document.getElementById('document-error-msg').textContent = `Could not reach any connected peer (${peerErrors.length} failed).`;
    }
    return;
  }

  showDocumentState('workspace');
}

function normalizeDocumentItem(item) {
  return {
    id: item.id,
    name: item.name || 'Document',
    source: item.source || 'peer',
    peerId: item.peerId || item.peer_id,
    peerName: item.peerName || item.peer_name || 'Peer',
    uri: item.uri,
    downloadUrl: item.downloadUrl || item.download_url,
    mimeType: item.mimeType || item.mime_type || 'application/octet-stream',
    fileSizeBytes: item.fileSizeBytes ?? item.file_size_bytes ?? null,
    lastModified: item.lastModified || item.last_modified || null
  };
}

function renderDocumentList(items, peerErrors) {
  const list = document.getElementById('document-list');
  const status = document.getElementById('document-status');
  list.innerHTML = '';
  status.textContent = peerErrors.length > 0
    ? `${peerErrors.length} peer could not be reached.`
    : '';

  items.forEach((item) => {
    const li = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'document-row';
    button.dataset.documentId = item.id;

    const title = document.createElement('span');
    title.className = 'document-row-title';
    title.textContent = item.name;
    button.appendChild(title);

    const meta = document.createElement('span');
    meta.className = 'document-row-meta';
    meta.textContent = documentMeta(item);
    button.appendChild(meta);

    const badge = document.createElement('span');
    badge.className = 'document-mime-badge';
    badge.textContent = documentMimeLabel(item.mimeType);
    button.appendChild(badge);

    button.addEventListener('click', () => selectDocument(item));
    li.appendChild(button);
    list.appendChild(li);
  });

  peerErrors.forEach((peer) => {
    const li = document.createElement('li');
    li.className = 'document-row--error';
    li.title = peer.error || 'Peer unavailable';
    const icon = document.createElement('span');
    icon.className = 'layer-error-icon';
    icon.setAttribute('aria-hidden', 'true');
    const label = document.createElement('span');
    label.textContent = `${peer.peerName || peer.peer_name || 'Peer'} unavailable`;
    li.appendChild(icon);
    li.appendChild(label);
    list.appendChild(li);
  });
}

function documentMeta(item) {
  const details = [item.peerName || 'Peer'];
  if (item.fileSizeBytes != null) details.push(formatBytes(item.fileSizeBytes));
  if (item.lastModified) details.push(formatDate(item.lastModified));
  return details.filter(Boolean).join(' · ');
}

function documentMimeLabel(mimeType) {
  const value = String(mimeType || 'file').toLowerCase();
  if (value === 'application/pdf') return 'PDF';
  const parts = value.split('/');
  return (parts[1] || parts[0] || 'file').slice(0, 16);
}

function formatBytes(value) {
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes < 0) return '';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function selectDocument(item) {
  documentActiveItem = item;
  document.querySelectorAll('.document-row--active').forEach((row) => row.classList.remove('document-row--active'));
  document.querySelector(`.document-row[data-document-id="${CSS.escape(item.id)}"]`)?.classList.add('document-row--active');
  document.getElementById('document-title').textContent = item.name || 'Document';
  document.getElementById('document-source-label').textContent = documentMeta(item);
  resetDocumentRender();

  if (isPdfDocument(item)) {
    renderPdfDocument(item);
  } else {
    renderDocumentLinkCard(item);
  }
}

function isPdfDocument(item) {
  return String(item.mimeType || '').toLowerCase().split(';')[0].trim() === 'application/pdf';
}

function resetDocumentRender() {
  clearDocumentEmbed();
  const toolbar = document.getElementById('document-toolbar');
  if (toolbar) {
    toolbar.innerHTML = '';
    toolbar.hidden = true;
  }
  const notice = document.getElementById('document-notice');
  if (notice) {
    notice.textContent = '';
    notice.hidden = true;
  }
  const linkCard = document.getElementById('document-link-card');
  if (linkCard) {
    linkCard.innerHTML = '';
    linkCard.hidden = true;
  }
}

function clearDocumentEmbed() {
  if (documentEmbedTimer) {
    clearTimeout(documentEmbedTimer);
    documentEmbedTimer = null;
  }
  const frameContainer = document.getElementById('document-frame-container');
  if (frameContainer) frameContainer.innerHTML = '';
}

function renderPdfDocument(item) {
  const frameContainer = document.getElementById('document-frame-container');
  const toolbar = document.getElementById('document-toolbar');
  const live = document.getElementById('document-render-live');
  if (!frameContainer || !toolbar) return;

  renderDocumentToolbar(item);
  const iframe = document.createElement('iframe');
  iframe.className = 'document-frame';
  iframe.src = item.downloadUrl;
  iframe.title = `${item.name || 'Document'} - PDF document`;
  iframe.setAttribute('sandbox', DOCUMENT_IFRAME_SANDBOX);
  iframe.setAttribute('referrerpolicy', 'no-referrer');

  const fallback = buildDocumentLink(item, 'Open in new tab', 'document-fallback-link sr-only');
  fallback.textContent = `Open ${item.name || 'document'} in new tab`;
  iframe.appendChild(fallback);

  iframe.addEventListener('load', () => {
    if (documentActiveItem?.id !== item.id) return;
    if (documentEmbedTimer) {
      clearTimeout(documentEmbedTimer);
      documentEmbedTimer = null;
    }
    let hasDocument = false;
    try {
      hasDocument = !!iframe.contentDocument;
    } catch (_) {
      hasDocument = false;
    }
    if (!hasDocument) {
      showDocumentEmbedFallback(item);
      return;
    }
    if (live) live.textContent = `${item.name || 'Document'} loaded`;
  });
  iframe.addEventListener('error', () => showDocumentEmbedFallback(item));

  frameContainer.appendChild(iframe);
  if (live) live.textContent = `Loading ${item.name || 'document'}`;
  documentEmbedTimer = setTimeout(() => {
    documentEmbedTimer = null;
    let hasDocument = false;
    try {
      hasDocument = !!iframe.contentDocument;
    } catch (_) {
      hasDocument = false;
    }
    if (!hasDocument && documentActiveItem?.id === item.id) {
      showDocumentEmbedFallback(item);
    }
  }, DOCUMENT_EMBED_TIMEOUT_MS);
}

function renderDocumentToolbar(item) {
  const toolbar = document.getElementById('document-toolbar');
  if (!toolbar) return;
  toolbar.innerHTML = '';
  toolbar.hidden = false;
  toolbar.appendChild(buildDocumentLink(item, 'Open in new tab', 'document-action-link'));
  const download = buildDocumentLink(item, 'Download', 'document-action-link');
  download.setAttribute('download', '');
  toolbar.appendChild(download);
}

function showDocumentEmbedFallback(item) {
  clearDocumentEmbed();
  const notice = document.getElementById('document-notice');
  if (notice) {
    notice.textContent = 'This document could not be displayed inline.';
    notice.hidden = false;
  }
  renderDocumentLinkCard(item);
}

function renderDocumentLinkCard(item) {
  clearDocumentEmbed();
  const toolbar = document.getElementById('document-toolbar');
  if (toolbar) {
    toolbar.innerHTML = '';
    toolbar.hidden = true;
  }
  const card = document.getElementById('document-link-card');
  if (!card) return;
  card.innerHTML = '';
  card.hidden = false;

  const title = document.createElement('h3');
  title.textContent = item.name || 'Document';
  card.appendChild(title);

  const meta = document.createElement('p');
  meta.className = 'document-link-meta';
  meta.textContent = `${documentMimeLabel(item.mimeType)} · ${documentMeta(item)}`;
  card.appendChild(meta);

  card.appendChild(buildDocumentLink(item, 'Open in new tab', 'document-primary-link'));
}

function buildDocumentLink(item, label, className) {
  const link = document.createElement('a');
  link.href = item.downloadUrl;
  link.target = '_blank';
  link.rel = 'noopener noreferrer';
  link.className = className;
  link.textContent = label;
  link.setAttribute('aria-label', `${label} ${item.name || 'document'} (opens in new tab)`);
  return link;
}

function replaceDocumentController() {
  if (documentRequestController) documentRequestController.abort();
  documentRequestController = new AbortController();
  return documentRequestController;
}

function showDocumentState(state) {
  document.getElementById('document-loading').hidden = state !== 'loading';
  document.getElementById('document-empty').hidden = state !== 'empty';
  document.getElementById('document-error').hidden = state !== 'error';
  document.getElementById('document-workspace').hidden = state !== 'workspace';
}

function showDocumentError(message) {
  document.getElementById('document-error-msg').textContent = message;
  showDocumentState('error');
}

initDocumentPanel();

/* ── Connector + Stream API helpers ── */

function listConnectors(workspaceId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/connectors');
}

function createConnector(workspaceId, kind, name, config) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/connectors', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ kind, name, config }),
  });
}

function listStreams(connectorId) {
  return apiFetch('/api/v1/connectors/' + connectorId + '/streams');
}

function pollStream(streamId) {
  return apiFetch('/api/v1/streams/' + streamId + '/poll', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
  });
}

/* ── Connector panel state ── */

const connectorsStatusEl  = document.getElementById('connectors-status');
const connectorListEl     = document.getElementById('connector-list');
const addConnectorBtn     = document.getElementById('add-connector-btn');
const addConnectorDialog  = document.getElementById('add-connector-dialog');
const acStepProvider      = document.getElementById('ac-step-provider');
const acStepConfigure     = document.getElementById('ac-step-configure');
const acConfigureTitle    = document.getElementById('ac-configure-title');
const acFormFields        = document.getElementById('ac-form-fields');
const acTestResultEl      = document.getElementById('ac-test-result');
const acErrorEl           = document.getElementById('ac-error');
const acBackBtn           = document.getElementById('ac-back-btn');
const acTestBtn           = document.getElementById('ac-test-btn');
const acSubmitBtn         = document.getElementById('ac-submit-btn');
const acProviderCancelBtn = document.getElementById('ac-step-provider-cancel');
const acStepProgress      = document.getElementById('ac-step-progress');
const acProgressHint      = document.getElementById('ac-progress-hint');
const acProgressDoneBtn   = document.getElementById('ac-progress-done');

// connectorStreams: Map<connectorId, Stream[]>
const connectorStreams = new Map();
let workspaceEventSource = null;
let acProgressConnectorId = null;

function setConnectorsStatus(msg, isError) {
  connectorsStatusEl.textContent = msg;
  connectorsStatusEl.style.color = isError ? 'var(--color-error)' : 'var(--color-sidebar-muted)';
}

function clearConnectorsStatus() {
  connectorsStatusEl.textContent = '';
}

async function loadConnectorTimeline(workspaceId, connectorId, listEl) {
  try {
    const data = await apiFetch(`/api/v1/workspaces/${workspaceId}/events?connector_id=${connectorId}&connectorId=${connectorId}&limit=10`);
    listEl.innerHTML = '';
    (data.items || []).forEach((ev) => listEl.appendChild(renderTimelineItem(ev)));
  } catch (_err) {
    // Timeline is supplemental; connector cards still render without it.
  }
}

function subscribeWorkspaceEvents(workspaceId) {
  if (workspaceEventSource) {
    workspaceEventSource.close();
    workspaceEventSource = null;
  }
  try {
    const url = `/api/v1/workspaces/${workspaceId}/events/stream`;
    const es = new EventSource(url, { withCredentials: true });
    es.addEventListener('pipeline_event', (msg) => {
      try {
        const ev = JSON.parse(msg.data);
        handlePipelineEvent(ev);
      } catch (_) {}
    });
    es.onerror = () => {
      const indicator = document.getElementById('sse-reconnect');
      if (indicator) indicator.hidden = false;
    };
    es.onopen = () => {
      const indicator = document.getElementById('sse-reconnect');
      if (indicator) indicator.hidden = true;
    };
    workspaceEventSource = es;
  } catch (_err) {
    // EventSource unsupported or blocked; the static timeline fetch is enough to degrade gracefully.
  }
}

function handlePipelineEvent(ev) {
  if (ev.connectorId) {
    const list = document.querySelector(`.conn-timeline[data-connector-id="${ev.connectorId}"]`);
    if (list) {
      list.insertBefore(renderTimelineItem(ev), list.firstChild);
      while (list.children.length > 10) list.removeChild(list.lastChild);
    }
  }

  if (acStepProgress && !acStepProgress.hidden && acProgressConnectorId === ev.connectorId) {
    const li = acStepProgress.querySelector(`li[data-stage="${ev.stage}"]`);
    if (li) li.classList.add('done');
    if (ev.stage === 'error' && acProgressHint) {
      acProgressHint.textContent = `Error: ${ev.detail && ev.detail.error ? ev.detail.error : 'see server logs'}`;
    }
    if (ev.stage === 'stall' && acProgressHint) {
      acProgressHint.textContent = 'Still waiting — this can take up to a minute on a cold Ollama.';
    }
  }

  if (ev.stage === 'rule_diagnostic' && activeTab === 'signals') {
    refreshRuleDiagnostics();
  }
}

/* ── Build stream sub-list ── */

function buildStreamItem(stream) {
  const li = document.createElement('li');
  li.className = 'stream-item';
  li.dataset.id = stream.id;

  const nameSpan = document.createElement('span');
  nameSpan.className = 'stream-name';
  nameSpan.textContent = stream.name;

  const pollBtn = document.createElement('button');
  pollBtn.type = 'button';
  pollBtn.className = 'poll-btn';
  pollBtn.textContent = 'Poll';
  pollBtn.setAttribute('aria-label', 'Poll stream ' + stream.name);

  const resultSpan = document.createElement('span');
  resultSpan.className = 'poll-result';
  resultSpan.setAttribute('aria-live', 'polite');

  pollBtn.addEventListener('click', async () => {
    pollBtn.disabled = true;
    resultSpan.textContent = 'Polling…';
    resultSpan.className = 'poll-result';
    try {
      const data = await pollStream(stream.id);
      resultSpan.textContent = 'Ingested ' + data.ingested + ' events';
      resultSpan.className = 'poll-result';
      // Refresh streams for parent connector.
      await refreshStreamsForConnector(stream.connectorId);
    } catch (err) {
      resultSpan.textContent = 'Error: ' + err.message;
      resultSpan.className = 'poll-result poll-result--error';
    } finally {
      pollBtn.disabled = false;
    }
  });

  li.appendChild(nameSpan);
  li.appendChild(pollBtn);
  li.appendChild(resultSpan);
  return li;
}

function buildStreamSubList(connectorId, streams) {
  if (streams.length === 0) {
    const p = document.createElement('p');
    p.className = 'streams-empty';
    p.textContent = 'No streams';
    return p;
  }

  const ul = document.createElement('ul');
  ul.className = 'stream-list';
  ul.setAttribute('role', 'list');
  streams.forEach((s) => ul.appendChild(buildStreamItem(s)));
  return ul;
}

async function refreshStreamsForConnector(connectorId) {
  const card = connectorListEl.querySelector('[data-connector-id="' + connectorId + '"]');
  if (!card) return;

  const streamsContainer = card.querySelector('.connector-streams');
  if (!streamsContainer) return;

  try {
    const data = await listStreams(connectorId);
    const streams = data.items || [];
    connectorStreams.set(connectorId, streams);
    streamsContainer.innerHTML = '';
    streamsContainer.appendChild(buildStreamSubList(connectorId, streams));
  } catch (err) {
    streamsContainer.innerHTML = '';
    const p = document.createElement('p');
    p.className = 'streams-error';
    p.textContent = 'Error loading streams: ' + err.message;
    streamsContainer.appendChild(p);
  }
}

/* ── Build connector card ── */

function buildConnectorCard(connector) {
  const li = document.createElement('li');
  li.className = 'connector-card';
  li.dataset.connectorId = connector.id;

  // Header row.
  const header = document.createElement('div');
  header.className = 'connector-card-header';

  const nameSpan = document.createElement('span');
  nameSpan.className = 'connector-name';
  nameSpan.textContent = isDemoWorkspace(activeWorkspace) ? `Sample — ${connector.name}` : connector.name;

  const kindBadge = document.createElement('span');
  kindBadge.className = 'badge-kind';
  kindBadge.textContent = connector.kind;

  const statusChip = document.createElement('span');
  statusChip.className = 'status-chip status-chip--' + connector.status;
  statusChip.textContent = connector.status;
  if (connector.status === 'error' && connector.lastError) {
    statusChip.title = connector.lastError;
  }

  header.appendChild(nameSpan);
  header.appendChild(kindBadge);
  header.appendChild(statusChip);
  li.appendChild(header);

  const timeline = document.createElement('ul');
  timeline.className = 'conn-timeline';
  timeline.dataset.connectorId = connector.id;
  timeline.setAttribute('aria-label', 'Recent pipeline events');
  li.appendChild(timeline);

  // Streams container (async-filled).
  const streamsContainer = document.createElement('div');
  streamsContainer.className = 'connector-streams';

  const loadingP = document.createElement('p');
  loadingP.className = 'stream-loading';
  loadingP.textContent = 'Loading streams…';
  streamsContainer.appendChild(loadingP);

  li.appendChild(streamsContainer);

  if (activeWorkspace) {
    loadConnectorTimeline(activeWorkspace.id, connector.id, timeline);
  }

  // Load streams.
  listStreams(connector.id).then((data) => {
    const streams = data.items || [];
    connectorStreams.set(connector.id, streams);
    streamsContainer.innerHTML = '';
    streamsContainer.appendChild(buildStreamSubList(connector.id, streams));
  }).catch((err) => {
    streamsContainer.innerHTML = '';
    const p = document.createElement('p');
    p.className = 'streams-error';
    p.textContent = 'Error: ' + err.message;
    streamsContainer.appendChild(p);
  });

  return li;
}

/* ── Load connectors for active workspace ── */

async function loadConnectors(workspaceId) {
  clearConnectorsStatus();
  subscribeWorkspaceEvents(workspaceId);
  connectorListEl.innerHTML = '';
  connectorStreams.clear();

  const loadingP = document.createElement('p');
  loadingP.className = 'stream-loading';
  loadingP.textContent = 'Loading connectors…';
  connectorListEl.appendChild(loadingP);

  try {
    const data = await listConnectors(workspaceId);
    const items = data.items || [];
    connectorListEl.innerHTML = '';

    if (items.length === 0) {
      const p = document.createElement('p');
      p.className = 'streams-empty';
      p.textContent = 'No connectors. Add one to start ingesting data.';
      connectorListEl.appendChild(p);
      return;
    }

    items.forEach((connector) => {
      connectorListEl.appendChild(buildConnectorCard(connector));
    });
  } catch (err) {
    connectorListEl.innerHTML = '';
    setConnectorsStatus('Error loading connectors: ' + err.message, true);
  }
}

document.getElementById('peer-browser-refresh')?.addEventListener('click', () => {
  if (activeWorkspace) loadPeerBrowser(activeWorkspace.id);
});

async function loadPeerBrowser(workspaceId) {
  const list = document.getElementById('peer-browser-list');
  const status = document.getElementById('peer-browser-status');
  if (!list || !status) return;
  list.innerHTML = '';
  status.textContent = 'Loading peers...';
  try {
    const bindings = await apiFetch(`/api/v1/workspaces/${workspaceId}/bindings`, { skipErrorToast: true });
    const active = (bindings.items || []).filter((binding) => (binding.status || 'pending') === 'active');
    if (!active.length) {
      status.textContent = 'No active peer bindings for this workspace.';
      return;
    }
    status.textContent = `${active.length} active peer${active.length === 1 ? '' : 's'}.`;
    for (const binding of active) {
      list.appendChild(await buildPeerBrowserCard(workspaceId, binding));
    }
  } catch (err) {
    status.textContent = 'Could not load peers: ' + (err.message || String(err));
  }
}

async function buildPeerBrowserCard(workspaceId, binding) {
  const li = document.createElement('li');
  li.className = 'peer-browser-card';
  const peerId = binding.peerId;
  li.innerHTML = `
    <header class="peer-browser-card-header">
      <div>
        <h3>${escapeHtml(binding.peerName || binding.foreignTenantName || 'Peer')}</h3>
        <p>${escapeHtml(binding.foreignTenantId || '')}</p>
      </div>
      <span class="peer-session-badge">checking...</span>
    </header>
    <div class="peer-browser-columns">
      <section>
        <h4>Tools</h4>
        <ul class="peer-browser-tools"></ul>
      </section>
      <section>
        <h4>Resources</h4>
        <ul class="peer-browser-resources"></ul>
      </section>
    </div>
  `;
  const toolsList = li.querySelector('.peer-browser-tools');
  const resourcesList = li.querySelector('.peer-browser-resources');
  const badge = li.querySelector('.peer-session-badge');
  try {
    const [tools, resources, session] = await Promise.all([
      apiFetch(`/api/v1/workspaces/${workspaceId}/peers/${peerId}/tools`, { skipErrorToast: true }),
      apiFetch(`/api/v1/workspaces/${workspaceId}/peers/${peerId}/resources`, { skipErrorToast: true }),
      apiFetch(`/api/v1/peers/${peerId}/session`, { skipErrorToast: true }).catch(() => null),
    ]);
    renderPeerBrowserItems(toolsList, tools.items || [], 'tool');
    renderPeerBrowserItems(resourcesList, resources.items || [], 'resource');
    if (badge) {
      const state = session?.sessionStatus || session?.runtimeState || 'disconnected';
      badge.textContent = typeof state === 'string' ? state : 'live';
      badge.className = `peer-session-badge peer-session-badge--${String(badge.textContent).toLowerCase()}`;
      if (session?.lastSessionError) badge.title = session.lastSessionError;
    }
  } catch (err) {
    li.classList.add('peer-browser-card--error');
    if (toolsList) toolsList.innerHTML = `<li>${escapeHtml(err.message || String(err))}</li>`;
    if (resourcesList) resourcesList.innerHTML = '<li>Unavailable</li>';
    if (badge) {
      badge.textContent = 'error';
      badge.className = 'peer-session-badge peer-session-badge--error';
    }
  }
  return li;
}

function renderPeerBrowserItems(list, items, kind) {
  if (!list) return;
  list.innerHTML = '';
  if (!items.length) {
    const empty = document.createElement('li');
    empty.className = 'peer-browser-empty';
    empty.textContent = `No ${kind}s.`;
    list.appendChild(empty);
    return;
  }
  items.slice(0, 12).forEach((item) => {
    const li = document.createElement('li');
    const name = item.name || item.uri || '(unnamed)';
    const desc = item.description || item.mimeType || item.ioneView || '';
    li.innerHTML = `<strong>${escapeHtml(name)}</strong>${desc ? `<span>${escapeHtml(desc)}</span>` : ''}`;
    list.appendChild(li);
  });
}

/* ── Add connector dialog ── */

const ACProvider = {
  nws: {
    title: 'NWS — weather alerts',
    fields: [
      { key: 'lat', label: 'Latitude', type: 'number', step: 'any', placeholder: '46.87', required: true, hint: 'Decimal degrees, -90 to 90.' },
      { key: 'lon', label: 'Longitude', type: 'number', step: 'any', placeholder: '-113.99', required: true, hint: 'Decimal degrees, -180 to 180.' },
      { key: 'pollIntervalSecs', label: 'Poll interval (seconds)', type: 'number', placeholder: '300', required: false },
    ],
    build: (vals) => ({ lat: Number(vals.lat), lon: Number(vals.lon), pollIntervalSecs: vals.pollIntervalSecs ? Number(vals.pollIntervalSecs) : undefined }),
  },
  firms: {
    title: 'FIRMS — fire detections',
    fields: [
      { key: 'mapKey', label: 'MAP_KEY', type: 'password', required: true, hint: 'Request a key at firms.modaps.eosdis.nasa.gov/api.' },
      { key: 'country', label: 'Country code', type: 'text', placeholder: 'USA', required: false },
    ],
    build: (vals) => ({ mapKey: vals.mapKey, country: vals.country || undefined }),
  },
  s3: {
    title: 'S3 — bucket',
    fields: [
      { key: 'endpoint', label: 'Endpoint URL', type: 'url', placeholder: 'https://s3.amazonaws.com', required: true },
      { key: 'bucket', label: 'Bucket', type: 'text', required: true },
      { key: 'prefix', label: 'Prefix', type: 'text', placeholder: 'data/', required: false },
      { key: 'accessKey', label: 'Access key ID', type: 'text', required: false },
      { key: 'secretKey', label: 'Secret access key', type: 'password', required: false },
      { key: 'region', label: 'Region', type: 'text', placeholder: 'us-east-1', required: false },
    ],
    build: (vals) => ({ endpoint: vals.endpoint, bucket: vals.bucket, prefix: vals.prefix || undefined, accessKey: vals.accessKey || undefined, secretKey: vals.secretKey || undefined, region: vals.region || undefined }),
  },
  slack: {
    title: 'Slack — webhook delivery',
    fields: [
      { key: 'webhookUrl', label: 'Webhook URL', type: 'url', required: true, placeholder: 'https://hooks.slack.com/services/...' },
    ],
    build: (vals) => ({ webhookUrl: vals.webhookUrl }),
  },
  irwin: {
    title: 'IRWIN — incident endpoint',
    fields: [
      { key: 'endpoint', label: 'Endpoint URL', type: 'url', required: true },
    ],
    build: (vals) => ({ endpoint: vals.endpoint }),
  },
  custom: {
    title: 'Custom JSON',
    fields: [
      { key: '_kind', label: 'Kind', type: 'text', required: true, placeholder: 'rust_native' },
      { key: '_name', label: 'Name', type: 'text', required: true },
      { key: '_config', label: 'Config (JSON)', type: 'textarea', placeholder: '{}' },
    ],
    build: (vals) => {
      let cfg = {};
      try { cfg = vals._config ? JSON.parse(vals._config) : {}; } catch (_e) { throw new Error('Config is not valid JSON.'); }
      return cfg;
    },
    kindOverride: (vals) => vals._kind,
    nameOverride: (vals) => vals._name,
  },
};

let acState = { kind: null, name: null, provider: null };

function openAddConnectorWizard() {
  acStepProvider.hidden = false;
  acStepConfigure.hidden = true;
  if (acStepProgress) acStepProgress.hidden = true;
  acProgressConnectorId = null;
  acState = { kind: null, name: null, provider: null };
  acErrorEl.hidden = true;
  acErrorEl.textContent = '';
  acTestResultEl.textContent = '';
  acTestResultEl.className = 'ac-test-result';
  acSubmitBtn.disabled = true;
  addConnectorDialog?.showModal?.();
}

function showAcProgress(connector) {
  acProgressConnectorId = connector.id;
  acStepConfigure.hidden = true;
  if (!acStepProgress) return;
  acStepProgress.hidden = false;
  acStepProgress.querySelectorAll('li').forEach((li) => li.classList.remove('done'));
  if (acProgressHint) acProgressHint.textContent = '';
}

function renderACFields(fields) {
  acFormFields.innerHTML = '';
  fields.forEach((f) => {
    const wrap = document.createElement('div');
    wrap.className = 'ac-field';
    const label = document.createElement('label');
    label.setAttribute('for', `ac-f-${f.key}`);
    label.textContent = f.label + (f.required ? ' *' : '');
    wrap.appendChild(label);

    let input;
    if (f.type === 'textarea') {
      input = document.createElement('textarea');
      input.rows = 4;
    } else {
      input = document.createElement('input');
      input.type = f.type || 'text';
      if (f.step) input.step = f.step;
    }
    input.id = `ac-f-${f.key}`;
    input.name = f.key;
    if (f.placeholder) input.placeholder = f.placeholder;
    if (f.required) input.required = true;
    input.addEventListener('input', () => {
      if (acState.name !== 'custom') {
        acSubmitBtn.disabled = true;
        acTestResultEl.textContent = '';
        acTestResultEl.className = 'ac-test-result';
      }
      acErrorEl.hidden = true;
    });
    wrap.appendChild(input);

    if (f.hint) {
      const hint = document.createElement('small');
      hint.className = 'ac-hint';
      hint.textContent = f.hint;
      wrap.appendChild(hint);
    }

    acFormFields.appendChild(wrap);
  });
}

function readACFields() {
  const vals = {};
  document.querySelectorAll('#ac-form-fields [name]').forEach((el) => {
    vals[el.name] = el.value;
  });
  return vals;
}

function buildACPayload() {
  const { kind, name, provider } = acState;
  if (!provider) throw new Error('Choose a provider first.');
  const vals = readACFields();
  const effectiveKind = provider.kindOverride ? provider.kindOverride(vals) : kind;
  const effectiveName = provider.nameOverride ? provider.nameOverride(vals) : name;
  const config = provider.build(vals);
  return { kind: effectiveKind, name: effectiveName, config };
}

document.querySelectorAll('#ac-step-provider .provider-tile').forEach((tile) => {
  tile.addEventListener('click', () => {
    const kind = tile.dataset.kind;
    const name = tile.dataset.name;
    const provider = ACProvider[name];
    if (!provider) return;
    acState = { kind, name, provider };
    acConfigureTitle.textContent = provider.title;
    renderACFields(provider.fields);
    acStepProvider.hidden = true;
    acStepConfigure.hidden = false;
    if (acStepProgress) acStepProgress.hidden = true;
    acSubmitBtn.disabled = name !== 'custom';
    acErrorEl.hidden = true;
    acErrorEl.textContent = '';
    acTestResultEl.textContent = '';
    acTestResultEl.className = 'ac-test-result';
    const firstField = acFormFields.querySelector('[name]');
    if (firstField) firstField.focus();
  });
});

addConnectorBtn.addEventListener('click', openAddConnectorWizard);

acBackBtn?.addEventListener('click', () => {
  acStepProvider.hidden = false;
  acStepConfigure.hidden = true;
  if (acStepProgress) acStepProgress.hidden = true;
});

acProviderCancelBtn?.addEventListener('click', () => {
  addConnectorDialog?.close?.();
});

acProgressDoneBtn?.addEventListener('click', () => {
  addConnectorDialog?.close?.();
  acProgressConnectorId = null;
});

addConnectorDialog.addEventListener('close', () => {
  addConnectorBtn.focus();
});

addConnectorDialog.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    addConnectorDialog.close();
  }
});

acTestBtn?.addEventListener('click', async () => {
  if (!acStepConfigure.reportValidity()) return;
  acTestResultEl.textContent = 'Testing…';
  acTestResultEl.className = 'ac-test-result';
  acErrorEl.hidden = true;
  try {
    const payload = buildACPayload();
    const body = await apiFetch('/api/v1/connectors/validate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
      onFieldError: (_field, body) => {
        const hint = body.hint ? ` - ${body.hint}` : '';
        acTestResultEl.textContent = `✗ ${body.message || body.error || 'Validation failed'}${hint}`;
        acTestResultEl.className = 'ac-test-result ac-test-result--error';
      },
    });
    if (body.ok) {
      acTestResultEl.textContent = `✓ ${JSON.stringify(body.sample || {})}`;
      acTestResultEl.className = 'ac-test-result ac-test-result--ok';
      acSubmitBtn.disabled = false;
    } else {
      const hint = body.hint ? ` — ${body.hint}` : '';
      acTestResultEl.textContent = `✗ ${body.message || body.error || 'Validation failed'}${hint}`;
      acTestResultEl.className = 'ac-test-result ac-test-result--error';
      acSubmitBtn.disabled = true;
    }
  } catch (err) {
    acTestResultEl.textContent = `✗ ${err.message || String(err)}`;
    acTestResultEl.className = 'ac-test-result ac-test-result--error';
    acSubmitBtn.disabled = true;
  }
});

async function submitAddConnectorWizard(e) {
  e.preventDefault();
  if (!acStepConfigure.reportValidity()) return;
  acSubmitBtn.disabled = true;
  acErrorEl.hidden = true;

  try {
    const ws = window.activeWorkspace;
    if (!ws) throw new Error('No active workspace.');
    const payload = buildACPayload();
    const body = await apiFetch(`/api/v1/workspaces/${ws.id}/connectors`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
      onFieldError: (_field, body) => {
        const hint = body.hint ? ` - ${body.hint}` : '';
        acErrorEl.textContent = `${body.message || body.error || 'Connector creation failed'}${hint}`;
        acErrorEl.hidden = false;
      },
    });
    const connector = body.connector || body;
    showAcProgress(connector);
    if (typeof loadConnectors === 'function') loadConnectors(ws.id);
    (body.events || body.pipelineEvents || []).forEach((ev) => handlePipelineEvent(ev));
  } catch (err) {
    acErrorEl.textContent = err.message || String(err);
    acErrorEl.hidden = false;
    acSubmitBtn.disabled = false;
  }
}

acStepConfigure?.addEventListener('submit', submitAddConnectorWizard);

/* ── Signals panel ── */

const signalsFilterSource   = document.getElementById('signals-filter-source');
const signalsFilterSeverity = document.getElementById('signals-filter-severity');
const signalsRefreshBtn     = document.getElementById('signals-refresh-btn');
const signalsStatusEl       = document.getElementById('signals-status');
const ruleDiagnosticsEl     = document.getElementById('rule-diagnostics');
const signalListEl          = document.getElementById('signal-list');

let signalsPollTimer = null;
const SIGNALS_POLL_MS = 15000;

function setSignalsStatus(msg, isError) {
  signalsStatusEl.textContent = msg;
  signalsStatusEl.className = isError ? 'signals-status--error' : '';
}

function clearSignalsStatus() {
  signalsStatusEl.textContent = '';
  signalsStatusEl.className = '';
}

function formatDateTime(iso) {
  const d = new Date(iso);
  return d.toLocaleString(undefined, {
    month: 'short', day: 'numeric',
    hour: 'numeric', minute: '2-digit',
  });
}

function sourceBadgeClass(source) {
  if (source === 'rule') return 'signal-source--rule';
  if (source === 'connector_event') return 'signal-source--connector-event';
  if (source === 'generator') return 'signal-source--generator';
  return '';
}

function sourceLabel(source) {
  if (source === 'connector_event') return 'connector event';
  return source;
}

function buildSignalCard(signal) {
  const li = document.createElement('li');
  li.className = 'signal-card';

  // Header row: severity chip + source badge + timestamp
  const header = document.createElement('div');
  header.className = 'signal-card-header';

  const severityChip = document.createElement('span');
  severityChip.className = 'severity-chip severity-chip--' + signal.severity;
  severityChip.textContent = signal.severity;

  const sourceBadge = document.createElement('span');
  sourceBadge.className = 'signal-source-badge ' + sourceBadgeClass(signal.source);
  sourceBadge.textContent = sourceLabel(signal.source);

  const timeSpan = document.createElement('span');
  timeSpan.className = 'signal-time';
  timeSpan.textContent = formatDateTime(signal.createdAt);

  header.appendChild(severityChip);
  header.appendChild(sourceBadge);
  header.appendChild(timeSpan);
  li.appendChild(header);

  // Title
  const titleEl = document.createElement('div');
  titleEl.className = 'signal-title';
  titleEl.textContent = signal.title;
  li.appendChild(titleEl);

  // Body
  const bodyEl = document.createElement('div');
  bodyEl.className = 'signal-body';
  bodyEl.textContent = signal.body;
  li.appendChild(bodyEl);

  // generatorModel secondary line (generator source only)
  if (signal.source === 'generator' && signal.generatorModel) {
    const modelEl = document.createElement('div');
    modelEl.className = 'signal-model';
    modelEl.textContent = signal.generatorModel;
    li.appendChild(modelEl);
  }

  // Evidence disclosure (hide if empty / null)
  const evidence = signal.evidence;
  const hasEvidence = evidence && (Array.isArray(evidence) ? evidence.length > 0 : Object.keys(evidence).length > 0);
  if (hasEvidence) {
    const details = document.createElement('details');
    details.className = 'signal-evidence';

    const summary = document.createElement('summary');
    summary.textContent = 'Evidence';
    details.appendChild(summary);

    const pre = document.createElement('pre');
    pre.className = 'signal-evidence-body';
    pre.textContent = JSON.stringify(evidence, null, 2);
    details.appendChild(pre);

    li.appendChild(details);
  }

  return li;
}

function diagnosticIsBroken(item) {
  return item.status && item.status !== 'ok' && item.status !== 'no_events';
}

function diagnosticStatusLabel(status) {
  const labels = {
    ok: 'Active',
    no_events: 'No events',
    stream_not_found: 'Stream missing',
    parse_error: 'Rule parse error',
    type_mismatch: 'Type mismatch',
    rules_unparseable: 'Rules invalid',
  };
  return labels[status] || status || 'Unknown';
}

function renderRuleDiagnostics(data) {
  if (!ruleDiagnosticsEl) return;
  const items = data?.items || [];
  ruleDiagnosticsEl.innerHTML = '';
  ruleDiagnosticsEl.hidden = items.length === 0;
  if (items.length === 0) return;

  const header = document.createElement('div');
  header.className = 'rule-diagnostics-header';
  const title = document.createElement('strong');
  title.textContent = 'Rule diagnostics';
  header.appendChild(title);
  if (data.evaluatedAt) {
    const when = document.createElement('span');
    when.textContent = formatDateTime(data.evaluatedAt);
    header.appendChild(when);
  }
  ruleDiagnosticsEl.appendChild(header);

  const list = document.createElement('ul');
  list.className = 'rule-diagnostics-list';
  items.forEach((item) => {
    const li = document.createElement('li');
    li.className = 'rule-diagnostic-item' + (diagnosticIsBroken(item) ? ' rule-diagnostic-item--warning' : '');

    const top = document.createElement('div');
    top.className = 'rule-diagnostic-top';
    const status = document.createElement('span');
    status.className = 'rule-diagnostic-status';
    status.textContent = (diagnosticIsBroken(item) ? 'Warning: ' : '') + diagnosticStatusLabel(item.status);
    top.appendChild(status);

    const name = document.createElement('span');
    name.className = 'rule-diagnostic-name';
    name.textContent = item.ruleTitle || item.stream || 'rules';
    top.appendChild(name);
    li.appendChild(top);

    const meta = document.createElement('div');
    meta.className = 'rule-diagnostic-meta';
    meta.textContent = `${item.eventsEvaluated || 0} events, ${item.matchCount || 0} matches`;
    li.appendChild(meta);

    if (Array.isArray(item.skipReasons) && item.skipReasons.length > 0) {
      const reasons = document.createElement('ul');
      reasons.className = 'rule-diagnostic-reasons';
      item.skipReasons.forEach((reason) => {
        const reasonEl = document.createElement('li');
        reasonEl.textContent = `${reason.code}: ${reason.detail} (${reason.count})`;
        reasons.appendChild(reasonEl);
      });
      li.appendChild(reasons);
    }

    if (item.status === 'no_events') {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'rule-diagnostic-action';
      btn.textContent = 'Open Connectors';
      btn.addEventListener('click', () => activateTab('connectors'));
      li.appendChild(btn);
    }

    list.appendChild(li);
  });
  ruleDiagnosticsEl.appendChild(list);
}

async function refreshRuleDiagnostics() {
  if (!activeWorkspace || !ruleDiagnosticsEl) return;
  const workspaceId = activeWorkspace.id;
  try {
    const diagnostics = await getRuleDiagnostics(workspaceId);
    if (!activeWorkspace || activeWorkspace.id !== workspaceId) return;
    renderRuleDiagnostics(diagnostics);
  } catch (_err) {
    if (!activeWorkspace || activeWorkspace.id !== workspaceId) return;
    ruleDiagnosticsEl.hidden = true;
  }
}

async function loadSignals() {
  if (!activeWorkspace) return;

  clearSignalsStatus();
  signalListEl.innerHTML = '';

  const loadingEl = document.createElement('p');
  loadingEl.className = 'signals-loading';
  loadingEl.textContent = 'Loading signals…';
  signalListEl.appendChild(loadingEl);

  const source = signalsFilterSource.value;
  const severity = signalsFilterSeverity.value;

  try {
    refreshRuleDiagnostics();
    const data = await listSignals(activeWorkspace.id, source, severity);
    const items = data.items || [];
    signalListEl.innerHTML = '';

    if (items.length === 0) {
      const empty = document.createElement('p');
      empty.className = 'signals-empty';
      empty.textContent = 'No signals match the current filters.';
      signalListEl.appendChild(empty);
      return;
    }

    items.forEach((signal) => {
      signalListEl.appendChild(buildSignalCard(signal));
    });
  } catch (err) {
    signalListEl.innerHTML = '';
    setSignalsStatus('Error loading signals: ' + err.message, true);
  }
}

function startSignalsPolling() {
  stopSignalsPolling();
  signalsPollTimer = setInterval(() => {
    if (activeTab === 'signals') {
      loadSignals();
    }
  }, SIGNALS_POLL_MS);
}

function stopSignalsPolling() {
  if (signalsPollTimer !== null) {
    clearInterval(signalsPollTimer);
    signalsPollTimer = null;
  }
}

signalsRefreshBtn.addEventListener('click', loadSignals);
signalsFilterSource.addEventListener('change', loadSignals);
signalsFilterSeverity.addEventListener('change', loadSignals);

/* ── Survivors panel ── */

const survivorsFilterRole    = document.getElementById('survivors-filter-role');
const survivorsFilterVerdict = document.getElementById('survivors-filter-verdict');
const survivorsRefreshBtn    = document.getElementById('survivors-refresh-btn');
const survivorsStatusEl      = document.getElementById('survivors-status');
const survivorListEl         = document.getElementById('survivor-list');

let survivorsPollTimer = null;
const SURVIVORS_POLL_MS = 15000;

const ACTIVE_ROLE_KEY_PREFIX = 'ione.activeRoleId.';

function activeRoleStorageKey(workspaceId) {
  return ACTIVE_ROLE_KEY_PREFIX + workspaceId;
}

function listSurvivors(workspaceId, verdict) {
  let url = '/api/v1/workspaces/' + workspaceId + '/survivors';
  if (verdict) url += '?verdict=' + encodeURIComponent(verdict);
  return apiFetch(url);
}

function listFeed(workspaceId, roleId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/feed?roleId=' + encodeURIComponent(roleId));
}

function listRoles(workspaceId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/roles');
}

async function loadRolesIntoFilter(workspaceId) {
  // Reset to "All survivors" then populate from API.
  survivorsFilterRole.innerHTML = '<option value="">All survivors</option>';

  try {
    const data = await listRoles(workspaceId);
    const roles = data.items || [];
    roles.forEach((role) => {
      const opt = document.createElement('option');
      opt.value = role.id;
      opt.textContent = role.name + ' (CoC ' + role.cocLevel + ')';
      survivorsFilterRole.appendChild(opt);
    });

    // Restore persisted role selection for this workspace.
    const savedRoleId = localStorage.getItem(activeRoleStorageKey(workspaceId));
    if (savedRoleId && roles.find((r) => r.id === savedRoleId)) {
      survivorsFilterRole.value = savedRoleId;
    }
  } catch (_err) {
    // Non-fatal: role filter just stays at "All survivors".
  }
}

function setSurvivorsStatus(msg, isError) {
  survivorsStatusEl.textContent = msg;
  survivorsStatusEl.className = isError ? 'survivors-status--error' : '';
}

function clearSurvivorsStatus() {
  survivorsStatusEl.textContent = '';
  survivorsStatusEl.className = '';
}

function verdictChipClass(verdict) {
  if (verdict === 'survive') return 'verdict-chip--survive';
  if (verdict === 'reject') return 'verdict-chip--reject';
  if (verdict === 'defer') return 'verdict-chip--defer';
  return '';
}

function buildConfidenceBar(confidence) {
  const wrapper = document.createElement('div');
  wrapper.className = 'confidence-bar-wrapper';
  wrapper.title = 'Confidence: ' + (confidence * 100).toFixed(0) + '%';

  const track = document.createElement('div');
  track.className = 'confidence-bar-track';

  const fill = document.createElement('div');
  fill.className = 'confidence-bar-fill';
  fill.style.width = (confidence * 100).toFixed(1) + '%';

  track.appendChild(fill);
  wrapper.appendChild(track);

  const label = document.createElement('span');
  label.className = 'confidence-bar-label';
  label.textContent = (confidence * 100).toFixed(0) + '%';
  wrapper.appendChild(label);

  return wrapper;
}

function routingChipClass(kind) {
  if (kind === 'feed') return 'routing-chip--feed';
  if (kind === 'notification') return 'routing-chip--notification';
  if (kind === 'draft') return 'routing-chip--draft';
  if (kind === 'peer') return 'routing-chip--peer';
  return '';
}

function buildRoutingChips(routingDecisions) {
  if (!Array.isArray(routingDecisions) || routingDecisions.length === 0) return null;
  const row = document.createElement('div');
  row.className = 'routing-chips';
  routingDecisions.forEach((rd) => {
    const chip = document.createElement('span');
    chip.className = 'routing-chip ' + routingChipClass(rd.targetKind);
    chip.textContent = rd.targetKind;
    chip.title = rd.rationale || '';
    row.appendChild(chip);
  });
  return row;
}

function buildSurvivorCard(survivor) {
  const li = document.createElement('li');
  li.className = 'survivor-card';

  // Header: verdict chip + severity chip + timestamp
  const header = document.createElement('div');
  header.className = 'survivor-card-header';

  const verdictChip = document.createElement('span');
  verdictChip.className = 'verdict-chip ' + verdictChipClass(survivor.verdict);
  verdictChip.textContent = survivor.verdict;

  const severityChip = document.createElement('span');
  severityChip.className = 'severity-chip severity-chip--' + (survivor.signalSeverity || '');
  severityChip.textContent = survivor.signalSeverity || '';

  const timeSpan = document.createElement('span');
  timeSpan.className = 'signal-time';
  timeSpan.textContent = formatDateTime(survivor.createdAt);

  header.appendChild(verdictChip);
  if (survivor.signalSeverity) header.appendChild(severityChip);
  header.appendChild(timeSpan);
  li.appendChild(header);

  // Confidence bar
  li.appendChild(buildConfidenceBar(survivor.confidence));

  // Signal title (from joined projection)
  const titleEl = document.createElement('div');
  titleEl.className = 'signal-title';
  titleEl.textContent = survivor.signalTitle || '(no title)';
  li.appendChild(titleEl);

  // Signal body
  const bodyEl = document.createElement('div');
  bodyEl.className = 'signal-body';
  bodyEl.textContent = survivor.signalBody || '';
  li.appendChild(bodyEl);

  // Critic model secondary line
  const modelEl = document.createElement('div');
  modelEl.className = 'signal-model';
  modelEl.textContent = survivor.criticModel;
  li.appendChild(modelEl);

  // Rationale
  const rationaleEl = document.createElement('div');
  rationaleEl.className = 'survivor-rationale';
  rationaleEl.textContent = survivor.rationale;
  li.appendChild(rationaleEl);

  // Routing decision chips (inline, from server-side join)
  const chipsEl = buildRoutingChips(survivor.routingDecisions);
  if (chipsEl) li.appendChild(chipsEl);

  // Expandable chain-of-reasoning
  const steps = survivor.chainOfReasoning;
  if (Array.isArray(steps) && steps.length > 0) {
    const details = document.createElement('details');
    details.className = 'survivor-reasoning';

    const summary = document.createElement('summary');
    summary.textContent = 'Reasoning (' + steps.length + ' step' + (steps.length !== 1 ? 's' : '') + ')';
    details.appendChild(summary);

    const ol = document.createElement('ol');
    ol.className = 'survivor-reasoning-steps';
    steps.forEach((step) => {
      const item = document.createElement('li');
      item.textContent = typeof step === 'string' ? step : JSON.stringify(step);
      ol.appendChild(item);
    });
    details.appendChild(ol);
    li.appendChild(details);
  }

  return li;
}

async function loadSurvivors() {
  if (!activeWorkspace) return;

  clearSurvivorsStatus();
  survivorListEl.innerHTML = '';

  const loadingEl = document.createElement('p');
  loadingEl.className = 'survivors-loading';
  loadingEl.textContent = 'Loading survivors…';
  survivorListEl.appendChild(loadingEl);

  const roleId = survivorsFilterRole.value;
  const verdict = survivorsFilterVerdict.value;

  // Persist role selection for this workspace.
  if (roleId) {
    localStorage.setItem(activeRoleStorageKey(activeWorkspace.id), roleId);
  } else {
    localStorage.removeItem(activeRoleStorageKey(activeWorkspace.id));
  }

  try {
    let data;
    if (roleId) {
      // Use feed endpoint for role-scoped view.
      data = await listFeed(activeWorkspace.id, roleId);
    } else {
      data = await listSurvivors(activeWorkspace.id, verdict);
    }
    const items = data.items || [];
    survivorListEl.innerHTML = '';

    if (items.length === 0) {
      const empty = document.createElement('p');
      empty.className = 'survivors-empty';
      empty.textContent = 'No survivors match the current filter.';
      survivorListEl.appendChild(empty);
      return;
    }

    items.forEach((survivor) => {
      survivorListEl.appendChild(buildSurvivorCard(survivor));
    });
  } catch (err) {
    survivorListEl.innerHTML = '';
    setSurvivorsStatus('Error loading survivors: ' + err.message, true);
  }
}

function startSurvivorsPolling() {
  stopSurvivorsPolling();
  survivorsPollTimer = setInterval(() => {
    if (activeTab === 'survivors') {
      loadSurvivors();
    }
  }, SURVIVORS_POLL_MS);
}

function stopSurvivorsPolling() {
  if (survivorsPollTimer !== null) {
    clearInterval(survivorsPollTimer);
    survivorsPollTimer = null;
  }
}

survivorsRefreshBtn.addEventListener('click', loadSurvivors);
survivorsFilterRole.addEventListener('change', loadSurvivors);
survivorsFilterVerdict.addEventListener('change', loadSurvivors);

/* ── Approvals panel ── */

const approvalsFilterStatus = document.getElementById('approvals-filter-status');
const approvalsRefreshBtn   = document.getElementById('approvals-refresh-btn');
const approvalsStatusEl     = document.getElementById('approvals-status');
const approvalListEl        = document.getElementById('approval-list');
const approvalsBadgeEl      = document.getElementById('approvals-badge');

let approvalsPollTimer = null;
const APPROVALS_POLL_MS = 10000;

function listApprovals(workspaceId, status) {
  let url = '/api/v1/workspaces/' + workspaceId + '/approvals';
  if (status) url += '?status=' + encodeURIComponent(status);
  return apiFetch(url);
}

function decideApproval(approvalId, decision, comment) {
  return apiFetch('/api/v1/approvals/' + approvalId, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ decision, comment: comment || undefined }),
  });
}

function setApprovalsStatus(msg, isError) {
  approvalsStatusEl.textContent = msg;
  approvalsStatusEl.className = isError ? 'approvals-status--error' : '';
}

function clearApprovalsStatus() {
  approvalsStatusEl.textContent = '';
  approvalsStatusEl.className = '';
}

async function updateApprovalsBadge(workspaceId) {
  try {
    const data = await listApprovals(workspaceId, 'pending');
    const items = data.items || [];
    const count = items.length;
    if (count > 0) {
      approvalsBadgeEl.textContent = count;
      approvalsBadgeEl.hidden = false;
    } else {
      approvalsBadgeEl.hidden = true;
    }
    renderAlertBanner(items);
  } catch (_) {
    approvalsBadgeEl.hidden = true;
    renderAlertBanner([]);
  }
}

// Severity rank for picking the most-urgent pending approval to surface.
function approvalSeverityRank(item) {
  const sev = ((item.artifactContent || {}).severity || '').toLowerCase();
  if (sev === 'command') return 3;
  if (sev === 'flagged') return 2;
  return 1;
}

// Ambient unacknowledged-alert banner over the existing toast container. Persists
// (no auto-dismiss) whenever any approval is pending, surfacing the highest-
// severity / most-recent item plus a "+N more" tail. Clicking jumps to Approvals.
function renderAlertBanner(pendingItems) {
  const container = document.getElementById('toast-container');
  if (!container) return;
  let banner = document.getElementById('alert-banner');
  const items = pendingItems || [];
  if (items.length === 0) {
    if (banner) banner.remove();
    return;
  }
  // Most urgent: highest severity, then most recent.
  const sorted = [...items].sort((a, b) => {
    const r = approvalSeverityRank(b) - approvalSeverityRank(a);
    if (r !== 0) return r;
    return String(b.createdAt || '').localeCompare(String(a.createdAt || ''));
  });
  const top = sorted[0];
  const content = top.artifactContent || {};
  const title = content.title || top.kind || 'Unacknowledged command';
  const more = items.length - 1;
  const label = `${items.length} pending approval${items.length === 1 ? '' : 's'}: ${title}${more > 0 ? ` · +${more} more` : ''}`;

  if (!banner) {
    banner = document.createElement('div');
    banner.id = 'alert-banner';
    banner.className = 'alert-banner';
    banner.setAttribute('role', 'alert');
    banner.tabIndex = 0;
    banner.addEventListener('click', () => {
      switchTab('approvals');
      document.getElementById('panel-approvals')?.focus();
    });
    banner.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        switchTab('approvals');
        document.getElementById('panel-approvals')?.focus();
      }
    });
    container.appendChild(banner);
  }
  banner.textContent = label;
}

async function loadApprovals() {
  if (!activeWorkspace) return;
  clearApprovalsStatus();
  const status = approvalsFilterStatus.value;
  try {
    const data = await listApprovals(activeWorkspace.id, status);
    const items = data.items || [];
    renderApprovals(items);
    updateApprovalsBadge(activeWorkspace.id);
  } catch (err) {
    setApprovalsStatus('Error loading approvals: ' + err.message, true);
  }
}

function severityBadgeClass(severity) {
  if (severity === 'command') return 'severity-badge--command';
  if (severity === 'flagged') return 'severity-badge--flagged';
  return 'severity-badge--routine';
}

function renderApprovals(items) {
  approvalListEl.innerHTML = '';

  if (items.length === 0) {
    const empty = document.createElement('li');
    empty.className = 'approval-item approval-item--empty';
    empty.textContent = 'No approvals to show.';
    approvalListEl.appendChild(empty);
    return;
  }

  items.forEach((approval) => {
    const li = document.createElement('li');
    li.className = 'approval-item';
    li.dataset.approvalId = approval.id;

    const content = approval.artifactContent || {};
    const title = content.title || '(untitled)';
    const body = content.body || '';
    const severity = content.severity || '';
    const rationale = content.rationale || '';

    const header = document.createElement('div');
    header.className = 'approval-header';

    const kindSpan = document.createElement('span');
    kindSpan.className = 'approval-kind';
    kindSpan.textContent = approval.kind || 'notification_draft';
    header.appendChild(kindSpan);

    if (severity) {
      const sevBadge = document.createElement('span');
      sevBadge.className = 'severity-badge ' + severityBadgeClass(severity);
      sevBadge.textContent = severity;
      header.appendChild(sevBadge);
    }

    const statusSpan = document.createElement('span');
    statusSpan.className = 'approval-status approval-status--' + approval.status;
    statusSpan.textContent = approval.status;
    header.appendChild(statusSpan);

    li.appendChild(header);

    const titleEl = document.createElement('div');
    titleEl.className = 'approval-title';
    titleEl.textContent = title;
    li.appendChild(titleEl);

    if (body) {
      const bodyEl = document.createElement('div');
      bodyEl.className = 'approval-body';
      bodyEl.textContent = body;
      li.appendChild(bodyEl);
    }

    if (rationale) {
      const rationaleEl = document.createElement('div');
      rationaleEl.className = 'approval-rationale';
      rationaleEl.textContent = 'Rationale: ' + rationale;
      li.appendChild(rationaleEl);
    }

    if (approval.status === 'pending') {
      const commentArea = document.createElement('textarea');
      commentArea.className = 'approval-comment';
      commentArea.placeholder = 'Comment (optional)';
      commentArea.rows = 2;
      li.appendChild(commentArea);

      const actions = document.createElement('div');
      actions.className = 'approval-actions';

      const approveBtn = document.createElement('button');
      approveBtn.className = 'approval-btn approval-btn--approve';
      approveBtn.type = 'button';
      approveBtn.textContent = 'Approve';
      approveBtn.addEventListener('click', async () => {
        approveBtn.disabled = true;
        rejectBtn.disabled = true;
        try {
          await decideApproval(approval.id, 'approved', commentArea.value);
          setApprovalsStatus('Sent.', false);
          await loadApprovals();
        } catch (err) {
          setApprovalsStatus('Delivery failed: ' + err.message, true);
          approveBtn.disabled = false;
          rejectBtn.disabled = false;
        }
      });

      const rejectBtn = document.createElement('button');
      rejectBtn.className = 'approval-btn approval-btn--reject';
      rejectBtn.type = 'button';
      rejectBtn.textContent = 'Reject';
      rejectBtn.addEventListener('click', async () => {
        approveBtn.disabled = true;
        rejectBtn.disabled = true;
        try {
          await decideApproval(approval.id, 'rejected', commentArea.value);
          setApprovalsStatus('Rejected.', false);
          await loadApprovals();
        } catch (err) {
          setApprovalsStatus('Error: ' + err.message, true);
          approveBtn.disabled = false;
          rejectBtn.disabled = false;
        }
      });

      actions.appendChild(approveBtn);
      actions.appendChild(rejectBtn);
      li.appendChild(actions);
    } else if (approval.decidedAt) {
      const decidedEl = document.createElement('div');
      decidedEl.className = 'approval-decided-at';
      decidedEl.textContent = 'Decided: ' + new Date(approval.decidedAt).toLocaleString();
      li.appendChild(decidedEl);
      if (approval.comment) {
        const commentEl = document.createElement('div');
        commentEl.className = 'approval-comment-display';
        commentEl.textContent = 'Comment: ' + approval.comment;
        li.appendChild(commentEl);
      }
    }

    approvalListEl.appendChild(li);
  });
}

function startApprovalsPolling() {
  stopApprovalsPolling();
  approvalsPollTimer = setInterval(() => {
    if (activeTab === 'approvals') {
      loadApprovals();
    }
  }, APPROVALS_POLL_MS);
}

function stopApprovalsPolling() {
  if (approvalsPollTimer !== null) {
    clearInterval(approvalsPollTimer);
    approvalsPollTimer = null;
  }
}

// Ambient poll that keeps the unacknowledged-alert banner + badge live on every
// tab, not just Approvals. updateApprovalsBadge drives both.
let ambientAlertTimer = null;
function startAmbientAlertPolling() {
  if (ambientAlertTimer !== null) return;
  ambientAlertTimer = setInterval(() => {
    if (activeTab !== 'approvals' && activeWorkspace) {
      updateApprovalsBadge(activeWorkspace.id);
    }
  }, APPROVALS_POLL_MS);
}

approvalsRefreshBtn.addEventListener('click', loadApprovals);
approvalsFilterStatus.addEventListener('change', loadApprovals);

/* ── Audit-trail panel (read-only ingest→signal→routing→decision chronology) ── */

const auditRefreshBtn = document.getElementById('audit-refresh-btn');
const auditStatusEl   = document.getElementById('audit-status');
const auditRowsEl      = document.getElementById('audit-rows');
const auditFilterActorKind = document.getElementById('audit-filter-actor-kind');
const auditFilterVerb      = document.getElementById('audit-filter-verb');
const auditFilterWindow    = document.getElementById('audit-filter-window');
const auditLoadMoreBtn     = document.getElementById('audit-load-more');

let auditPollTimer = null;
const AUDIT_POLL_MS = 10000;

const AUDIT_WINDOW_MS = { '1h': 3600e3, '24h': 86400e3, '7d': 7 * 86400e3, '30d': 30 * 86400e3 };

let auditNextCursor = null;
let auditItems = [];
let auditPaged = false; // true once "Load more" has appended a page

function auditFilterParts() {
  const parts = [];
  if (auditFilterActorKind.value) parts.push('actor_kind=' + encodeURIComponent(auditFilterActorKind.value));
  const verb = auditFilterVerb.value.trim();
  if (verb) parts.push('verb=' + encodeURIComponent(verb));
  const winMs = AUDIT_WINDOW_MS[auditFilterWindow.value];
  if (winMs) parts.push('since=' + encodeURIComponent(new Date(Date.now() - winMs).toISOString()));
  return parts;
}

async function loadAuditTrail(opts) {
  if (!activeWorkspace) return;
  const append = !!(opts && opts.append);
  try {
    const parts = auditFilterParts();
    if (append && auditNextCursor) parts.push('cursor=' + encodeURIComponent(auditNextCursor));
    const qs = parts.length ? '?' + parts.join('&') : '';
    const data = await apiFetch(`/api/v1/workspaces/${activeWorkspace.id}/audit_events${qs}`);
    auditNextCursor = data.next_cursor || null;
    auditItems = append ? auditItems.concat(data.items || []) : (data.items || []);
    auditPaged = append;
    renderAuditTrail(auditItems);
    auditLoadMoreBtn.hidden = !auditNextCursor;
    auditStatusEl.textContent = '';
  } catch (err) {
    auditStatusEl.textContent = 'Error loading audit trail: ' + err.message;
  }
}

function renderAuditTrail(items) {
  auditRowsEl.innerHTML = '';
  if (items.length === 0) {
    const tr = document.createElement('tr');
    const td = document.createElement('td');
    td.colSpan = 4;
    td.textContent = 'No audit events yet.';
    tr.appendChild(td);
    auditRowsEl.appendChild(tr);
    return;
  }
  items.forEach((ev) => {
    const tr = document.createElement('tr');
    const artifact = ev.objectId
      ? `${ev.objectKind || 'object'} ${String(ev.objectId).slice(0, 8)}`
      : (ev.objectKind || '');
    [ev.verb || '', artifact, ev.actorRef || (ev.actorKind || ''), ev.createdAt ? formatDateTime(ev.createdAt) : '']
      .forEach((text) => {
        const td = document.createElement('td');
        td.textContent = text;
        tr.appendChild(td);
      });
    auditRowsEl.appendChild(tr);
  });
}

/* Admin-only stat strip (probe-and-hide, Codex finding 5): /api/v1/me carries
   no role flag, so the first audit-aggregates fetch doubles as the admin
   probe. 403 → hide admin-only UI for the session; the server-side
   require_admin gate remains the actual protection. */
const auditStatsEl     = document.getElementById('audit-stats');
const auditStatTotalEl = document.getElementById('audit-stat-total');
const auditStatRecEl   = document.getElementById('audit-stat-recovery');
const auditChartEl     = document.getElementById('audit-chart');
const auditExportBtn   = document.getElementById('audit-export-btn');

let auditAdminDenied = false;

async function loadAuditStats() {
  if (!activeWorkspace || auditAdminDenied) return;
  const until = new Date();
  const since = new Date(until.getTime() - 24 * 3600e3);
  const windowQ = `since=${encodeURIComponent(since.toISOString())}&until=${encodeURIComponent(until.toISOString())}`;
  try {
    const agg = await apiFetch(
      `/api/v1/workspaces/${activeWorkspace.id}/audit-aggregates?op=count_by_bucket&bucket=hour&group_by=actor_kind&${windowQ}`,
      { skipErrorToast: true }
    );
    const pipe = await apiFetch(
      `/api/v1/workspaces/${activeWorkspace.id}/pipeline-aggregates?op=recovery_gap&${windowQ}`,
      { skipErrorToast: true }
    );
    renderAuditStats(agg, pipe);
    auditExportBtn.hidden = false; // probe succeeded → admin
  } catch (err) {
    if (err && err.status === 403) {
      auditAdminDenied = true;
    }
    auditStatsEl.hidden = true;
    auditChartEl.hidden = true;
    auditExportBtn.hidden = true;
  }
}

/* Export the current filter view as NDJSON, following X-Next-Cursor pages.
   since/until are computed once so every page sees the same frozen window. */
async function exportAuditNdjson() {
  if (!activeWorkspace) return;
  auditExportBtn.disabled = true;
  auditStatusEl.textContent = 'Exporting…';
  try {
    const baseParts = auditFilterParts().filter((p) => !p.startsWith('since='));
    const winMs = AUDIT_WINDOW_MS[auditFilterWindow.value] || 30 * 86400e3;
    const until = new Date();
    const since = new Date(until.getTime() - winMs);
    baseParts.push('since=' + encodeURIComponent(since.toISOString()));
    baseParts.push('until=' + encodeURIComponent(until.toISOString()));

    const chunks = [];
    let cursor = null;
    do {
      const parts = baseParts.slice();
      if (cursor) parts.push('cursor=' + encodeURIComponent(cursor));
      const resp = await fetch(
        `/api/v1/workspaces/${activeWorkspace.id}/audit-export?` + parts.join('&')
      );
      if (!resp.ok) throw new Error('HTTP ' + resp.status);
      chunks.push(await resp.text());
      cursor = resp.headers.get('X-Next-Cursor');
    } while (cursor);

    const blob = new Blob(chunks, { type: 'application/x-ndjson' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `audit-events-${activeWorkspace.id}.ndjson`;
    a.click();
    URL.revokeObjectURL(url);
    auditStatusEl.textContent = '';
  } catch (err) {
    auditStatusEl.textContent = 'Export failed: ' + err.message;
  } finally {
    auditExportBtn.disabled = false;
  }
}

auditExportBtn.addEventListener('click', exportAuditNdjson);

function renderAuditStats(agg, pipe) {
  const groups = (agg && agg.groups) || [];
  const total = groups.reduce((sum, g) => sum + (g.count || 0), 0);
  auditStatTotalEl.textContent = `Interactions (24 h): ${total}`;

  const summary = (pipe && pipe.summary) || {};
  auditStatRecEl.textContent = summary.count
    ? `Recovery gap p50/p90: ${Math.round(summary.p50)}s / ${Math.round(summary.p90)}s (${summary.count} faults)`
    : 'Recovery gap: no faults in window';
  auditStatsEl.hidden = false;

  // Counts-by-hour bar list (plain HTML/CSS — no new chart type).
  const byHour = new Map();
  groups.forEach((g) => {
    byHour.set(g.bucket_start, (byHour.get(g.bucket_start) || 0) + (g.count || 0));
  });
  auditChartEl.innerHTML = '';
  const maxCount = Math.max(...byHour.values(), 1);
  [...byHour.keys()].sort().forEach((bucketStart) => {
    const count = byHour.get(bucketStart);
    const li = document.createElement('li');
    const label = document.createElement('span');
    label.className = 'audit-bar-label';
    label.textContent = formatDateTime(bucketStart);
    const bar = document.createElement('span');
    bar.className = 'audit-bar';
    bar.style.width = `${Math.max((count / maxCount) * 60, 1)}%`;
    bar.title = `${count}`;
    const value = document.createElement('span');
    value.textContent = String(count);
    li.append(label, bar, value);
    auditChartEl.appendChild(li);
  });
  auditChartEl.hidden = byHour.size === 0;
}

function startAuditPolling() {
  stopAuditPolling();
  auditPollTimer = setInterval(() => {
    // Poll only the unfiltered, unpaged first page — a poll while the user is
    // filtering or walking pages would clobber what they're looking at.
    if (activeTab === 'audit' && auditFilterParts().length === 0 && !auditPaged) {
      loadAuditTrail();
    }
  }, AUDIT_POLL_MS);
}

function stopAuditPolling() {
  if (auditPollTimer !== null) {
    clearInterval(auditPollTimer);
    auditPollTimer = null;
  }
}

auditRefreshBtn.addEventListener('click', () => loadAuditTrail());
auditFilterActorKind.addEventListener('change', () => loadAuditTrail());
auditFilterVerb.addEventListener('change', () => loadAuditTrail());
auditFilterWindow.addEventListener('change', () => loadAuditTrail());
auditLoadMoreBtn.addEventListener('click', () => loadAuditTrail({ append: true }));

/* ── Init: load workspaces then conversations ── */

(async function init() {
  // Fetch current user identity and update top bar.
  await loadMe();

  try {
    await loadWorkspaces();

    // Restore last-active workspace from localStorage, fall back to first.
    const savedId = localStorage.getItem(ACTIVE_WORKSPACE_KEY);
    const restored = savedId ? workspaces.find((w) => w.id === savedId) : null;
    const initial = restored || workspaces[0] || null;

    if (initial) {
      // Call directly (not setActiveWorkspace) to avoid clearing transcript before convs load.
      activeWorkspace = initial;
      window.activeWorkspace = initial;
      if (isDemoWorkspace(initial) && !trackedDemoView) {
        trackedDemoView = true;
        track('demo_viewed', null, initial.id);
      }
      localStorage.setItem(ACTIVE_WORKSPACE_KEY, initial.id);
      workspaceNameEl.textContent = workspaceLabel(initial);
      workspaceDomainEl.textContent = initial.domain || '';
      renderChatChips(initial);
      renderWorkspaceLock(initial);
      const isClosed = workspaceIsClosed(initial);
      newChatBtn.hidden = isClosed;
      workspaceClosedNotice.hidden = !isClosed;
      // Seed the pending approvals badge + ambient alert banner.
      updateApprovalsBadge(initial.id);
      startAmbientAlertPolling();
      renderActivationTracker(initial);
    } else {
      workspaceNameEl.textContent = 'No workspaces';
    }
  } catch (err) {
    setStatus('Error loading workspaces: ' + err.message);
    workspaceNameEl.textContent = 'Error';
  }

  try {
    const data = await listConversations();
    // Response shape: { items: Conversation[] }
    conversations = data.items || [];
    renderSidebar();
  } catch (err) {
    setStatus('Error loading conversations: ' + err.message);
  }

  await refreshHealth();
  setInterval(refreshHealth, 15000);
})();

const healthDot = document.getElementById('health-dot');
const healthPanel = document.getElementById('health-panel');
const healthPanelClose = document.getElementById('health-panel-close');
const chatRemediation = document.getElementById('chat-remediation');

if (healthDot && healthPanel) {
  healthDot.addEventListener('click', () => {
    const willShow = healthPanel.hidden;
    if (willShow) renderHealthPanel(lastHealth);
    healthPanel.hidden = !willShow;
  });
}

if (healthPanelClose && healthPanel) {
  healthPanelClose.addEventListener('click', () => {
    healthPanel.hidden = true;
  });
}

if (healthPanel) {
  healthPanel.addEventListener('click', async (e) => {
    const copyBtn = e.target.closest('.health-copy');
    if (!copyBtn) return;
    await navigator.clipboard.writeText(copyBtn.dataset.cmd || '');
  });
}

if (chatRemediation) {
  chatRemediation.addEventListener('click', (e) => {
    if (e.target && e.target.id === 'health-retry') {
      refreshHealth();
    }
  });
}

/* ── MCP endpoint copy button (Phase 11) ── */
const mcpCopyBtn = document.getElementById('mcp-copy-btn');
const mcpUrlEl   = document.getElementById('mcp-url');

function currentMcpUrl() {
  return `${window.location.protocol}//${window.location.host}/mcp`;
}

function buildCursorDeepLink(mcpUrl) {
  const config = { name: 'ione', transport: 'http', url: mcpUrl };
  const encoded = btoa(JSON.stringify(config));
  return `cursor://anysphere.cursor-deeplink/mcp/install?name=ione&config=${encoded}`;
}

function buildVsCodeDeepLink(mcpUrl) {
  const config = { name: 'ione', url: mcpUrl, type: 'http' };
  const encoded = encodeURIComponent(JSON.stringify(config));
  return `vscode:mcp/install?${encoded}`;
}

function buildRawJsonConfig(mcpUrl) {
  return JSON.stringify({
    mcpServers: { ione: { url: mcpUrl, type: 'http' } },
  }, null, 2);
}

async function loadMcpClients() {
  try {
    const data = await apiFetch('/api/v1/mcp/clients');
    const body = document.getElementById('mcp-clients-body');
    if (!body) return;
    if (!data.items || data.items.length === 0) {
      body.innerHTML = '<tr><td colspan="4" class="mcp-clients-empty">No clients connected yet.</td></tr>';
      return;
    }
    body.innerHTML = '';
    data.items.forEach((c) => {
      const tr = document.createElement('tr');
      tr.innerHTML = `
        <td>${escapeHtml(c.displayName)}</td>
        <td>${c.createdAt ? new Date(c.createdAt).toLocaleString() : '—'}</td>
        <td>${c.lastSeenAt ? new Date(c.lastSeenAt).toLocaleString() : '—'}</td>
        <td><button type="button" class="mcp-client-revoke" data-client-row-id="${escapeHtml(c.id)}">Revoke</button></td>
      `;
      body.appendChild(tr);
    });
    body.querySelectorAll('.mcp-client-revoke').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const id = btn.dataset.clientRowId;
        if (!id) return;
        await apiFetch(`/api/v1/mcp/clients/${id}`, { method: 'DELETE' }).catch(() => {});
        loadMcpClients();
      });
    });
  } catch (_err) { /* ignore */ }
}

let mcpClientsInterval = null;
let currentPeerId = null;
let currentBindingId = null;
let currentBindings = [];
let peerAuthorizeUrl = null;
let bindingDialogReturnFocus = null;

document.getElementById('peer-federate-open-btn')?.addEventListener('click', () => {
  const dialog = document.getElementById('peer-federate-dialog');
  showPeerStep('url');
  const input = document.getElementById('peer-federate-url');
  if (input) input.value = '';
  const errEl = document.getElementById('peer-federate-allowlist-error');
  if (errEl) {
    errEl.hidden = true;
    errEl.textContent = '';
  }
  peerAuthorizeUrl = null;
  const fallback = document.getElementById('peer-federate-popup-fallback');
  if (fallback) fallback.hidden = true;
  dialog?.showModal?.();
});

document.getElementById('peer-federate-close')?.addEventListener('click', closePeerDialog);
document.getElementById('peer-federate-close-done')?.addEventListener('click', closePeerDialog);

function closePeerDialog() {
  const dialog = document.getElementById('peer-federate-dialog');
  dialog?.close?.();
  currentPeerId = null;
  currentBindings = [];
  peerAuthorizeUrl = null;
  resetWebhookProvision();
}

function showPeerStep(step) {
  ['url', 'waiting', 'allowlist', 'done'].forEach((s) => {
    const el = document.getElementById(`peer-federate-step-${s}`);
    if (el) el.hidden = s !== step;
  });
}

document.getElementById('peer-federate-start')?.addEventListener('click', async () => {
  const input = document.getElementById('peer-federate-url');
  const url = input?.value.trim() || '';
  if (!url) return;
  try {
    const data = await apiFetch('/api/v1/peers', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ peerUrl: url }),
      skipErrorToast: true,
    });
    currentPeerId = data.id;
    peerAuthorizeUrl = data.authorizeUrl;
    track('peer_federation_started', null, window.activeWorkspace?.id || null);
    const popup = window.open(data.authorizeUrl, '_blank', 'noopener,noreferrer');
    const fallback = document.getElementById('peer-federate-popup-fallback');
    const link = document.getElementById('peer-federate-authorize-link');
    if (link) link.href = data.authorizeUrl || '#';
    if (fallback) fallback.hidden = !!popup;
    showPeerStep('waiting');
  } catch (_err) {
    showError('peer_unreachable', `Couldn't reach ${url}.`, 'Check the URL and try again.');
  }
});

document.getElementById('peer-federate-manifest')?.addEventListener('click', async () => {
  if (!currentPeerId) return;
  const hint = document.getElementById('peer-federate-waiting-hint');
  if (hint) hint.textContent = 'Fetching tools...';
  try {
    const data = await apiFetch(`/api/v1/peers/${currentPeerId}/manifest`, { skipErrorToast: true });
    renderPeerTools(data.tools || []);
    showPeerStep('allowlist');
  } catch (_err) {
    if (hint) {
      hint.textContent = "Couldn't fetch tools. The peer may still be completing sign-in; try again in a moment.";
    }
  }
});

function renderPeerTools(tools) {
  const fieldset = document.getElementById('peer-federate-tool-list');
  if (!fieldset) return;
  const legend = fieldset.querySelector('legend');
  fieldset.innerHTML = '';
  if (legend) fieldset.appendChild(legend);
  if (!tools.length) {
    const p = document.createElement('p');
    p.className = 'peer-federate-hint';
    p.textContent = 'The peer did not return any tools. You can still confirm with no tools allowed (deny-all), or cancel and retry.';
    fieldset.appendChild(p);
    return;
  }
  tools.forEach((tool) => {
    const wrap = document.createElement('label');
    wrap.className = 'peer-federate-tool';

    const input = document.createElement('input');
    input.type = 'checkbox';
    input.value = tool.name || '';

    const name = document.createElement('span');
    name.className = 'peer-federate-tool-name';
    name.textContent = tool.name || '(unnamed tool)';

    const desc = document.createElement('span');
    desc.className = 'peer-federate-tool-desc';
    desc.textContent = tool.description || '';

    wrap.appendChild(input);
    wrap.appendChild(name);
    wrap.appendChild(desc);
    fieldset.appendChild(wrap);
  });
}

document.getElementById('peer-federate-confirm')?.addEventListener('click', async () => {
  if (!currentPeerId) return;
  const errEl = document.getElementById('peer-federate-allowlist-error');
  if (errEl) {
    errEl.hidden = true;
    errEl.textContent = '';
  }
  const checked = Array.from(document.querySelectorAll('#peer-federate-tool-list input[type=checkbox]:checked'))
    .map((checkbox) => checkbox.value);
  try {
    await apiFetch(`/api/v1/peers/${currentPeerId}/authorize`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ toolAllowlist: checked }),
      skipErrorToast: true,
    });
    track('peer_federation_activated', { toolCount: checked.length }, window.activeWorkspace?.id || null);
    resetWebhookProvision();
    await loadBindings(currentPeerId);
    showPeerStep('done');
  } catch (err) {
    if (errEl) {
      errEl.textContent = err.message || String(err);
      errEl.hidden = false;
    }
  }
});

document.getElementById('peer-bindings-refresh')?.addEventListener('click', () => {
  if (currentPeerId) loadBindings(currentPeerId);
});

document.getElementById('peer-webhook-provision-btn')?.addEventListener('click', async () => {
  if (!currentPeerId) return;
  const button = document.getElementById('peer-webhook-provision-btn');
  const oldText = button?.textContent;
  if (button) {
    button.disabled = true;
    button.textContent = 'Provisioning...';
  }
  try {
    const data = await apiFetch(`/api/v1/peers/${currentPeerId}/webhook/provision`, {
      method: 'POST',
      skipErrorToast: true,
    });
    const panel = document.getElementById('peer-webhook-secret-panel');
    const urlInput = document.getElementById('peer-webhook-url');
    const secretInput = document.getElementById('peer-webhook-secret');
    if (urlInput) urlInput.value = data.webhookUrl || '';
    if (secretInput) secretInput.value = data.signingSecret || '';
    if (panel) panel.hidden = false;
  } catch (err) {
    showError('webhook_provision_failed', err.message || String(err), 'Check peer access and try again.');
  } finally {
    if (button) {
      button.disabled = false;
      button.textContent = oldText || 'Provision webhook';
    }
  }
});

function resetWebhookProvision() {
  const panel = document.getElementById('peer-webhook-secret-panel');
  const urlInput = document.getElementById('peer-webhook-url');
  const secretInput = document.getElementById('peer-webhook-secret');
  if (panel) panel.hidden = true;
  if (urlInput) urlInput.value = '';
  if (secretInput) secretInput.value = '';
}

document.getElementById('binding-edit-close')?.addEventListener('click', () => {
  closeBindingDialog();
});

document.getElementById('binding-edit-save')?.addEventListener('click', async () => {
  const workspaceId = window.activeWorkspace?.id;
  if (!workspaceId || !currentBindingId) return;
  const errEl = document.getElementById('binding-edit-error');
  if (errEl) {
    errEl.hidden = true;
    errEl.textContent = '';
  }
  let scope;
  try {
    scope = JSON.parse(document.getElementById('binding-edit-scope')?.value || '{}');
  } catch (_err) {
    if (errEl) {
      errEl.textContent = 'Scope must be valid JSON.';
      errEl.hidden = false;
    }
    return;
  }
  try {
    await apiFetch(`/api/v1/workspaces/${workspaceId}/bindings/${currentBindingId}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        foreignTenantId: document.getElementById('binding-edit-tenant')?.value || '',
        foreignWorkspaceId: document.getElementById('binding-edit-workspace')?.value || null,
        scope,
      }),
    });
    closeBindingDialog();
    if (currentPeerId) loadBindings(currentPeerId);
  } catch (err) {
    if (errEl) {
      errEl.textContent = err.message || String(err);
      errEl.hidden = false;
    }
  }
});

async function loadBindings(peerId) {
  const body = document.getElementById('peer-bindings-body');
  if (!body) return;
  body.innerHTML = '<tr><td colspan="5">Loading...</td></tr>';
  try {
    const data = await apiFetch(`/api/v1/peers/${peerId}/bindings`, { skipErrorToast: true });
    currentBindings = data.items || [];
    renderBindings(currentBindings);
  } catch (_err) {
    body.innerHTML = '<tr><td colspan="4">Could not load bindings.</td></tr>';
  }
}

function renderBindings(bindings) {
  const body = document.getElementById('peer-bindings-body');
  if (!body) return;
  body.innerHTML = '';
  if (!bindings.length) {
    body.innerHTML = '<tr><td colspan="5">No workspace bindings yet.</td></tr>';
    renderBindingsPendingCallout(bindings);
    return;
  }
  renderBindingsPendingCallout(bindings);
  bindings.forEach((binding) => {
    const status = binding.status || 'pending';
    const row = document.createElement('tr');
    row.innerHTML = `
      <td>${escapeHtml(bindingWorkspaceLabel(binding.workspaceId))}</td>
      <td><span class="binding-status binding-status--${status}">${bindingStatusLabel(status)}</span></td>
      <td>${escapeHtml(binding.foreignTenantId || '')}</td>
      <td>${escapeHtml(binding.foreignWorkspaceId || '')}</td>
      <td class="peer-bindings-actions">
        <button type="button" data-binding-action="refresh" data-id="${binding.id}">Refresh</button>
        <button type="button" data-binding-action="edit" data-id="${binding.id}">Edit</button>
        <button type="button" data-binding-action="delete" data-id="${binding.id}">Delete</button>
      </td>
    `;
    body.appendChild(row);
  });
  body.querySelectorAll('[data-binding-action]').forEach((button) => {
    button.addEventListener('click', handleBindingAction);
  });
}

async function handleBindingAction(event) {
  const button = event.currentTarget;
  const id = button?.dataset?.id;
  const action = button?.dataset?.bindingAction;
  const workspaceId = window.activeWorkspace?.id;
  if (!id || !action || !workspaceId) return;
  if (action === 'edit') {
    editBinding(id);
    return;
  }
  if (action === 'refresh') {
    await apiFetch(`/api/v1/workspaces/${workspaceId}/bindings/${id}/refresh`, {
      method: 'POST',
      skipErrorToast: true,
    }).catch((err) => {
      const drift = err.body?.old && err.body?.new ? ` Stored tenant: ${err.body.old}. Peer reported: ${err.body.new}.` : '';
      showError('binding_refresh_failed', (err.message || String(err)) + drift, 'Check the peer and try again.');
    });
  }
  if (action === 'delete') {
    if (!confirm('Delete this workspace binding?')) return;
    await apiFetch(`/api/v1/workspaces/${workspaceId}/bindings/${id}`, { method: 'DELETE' });
  }
  if (currentPeerId) loadBindings(currentPeerId);
}

function editBinding(bindingId) {
  const binding = currentBindings.find((item) => item.id === bindingId);
  if (!binding) return;
  currentBindingId = bindingId;
  const tenantInput = document.getElementById('binding-edit-tenant');
  const workspaceInput = document.getElementById('binding-edit-workspace');
  const scopeInput = document.getElementById('binding-edit-scope');
  if (tenantInput) tenantInput.value = binding.foreignTenantId || '';
  if (workspaceInput) workspaceInput.value = binding.foreignWorkspaceId || '';
  if (scopeInput) scopeInput.value = JSON.stringify(binding.scope || {}, null, 2);
  const errEl = document.getElementById('binding-edit-error');
  if (errEl) {
    errEl.hidden = true;
    errEl.textContent = '';
  }
  const dialog = document.getElementById('binding-edit-dialog');
  bindingDialogReturnFocus = document.activeElement;
  dialog?.showModal?.();
  tenantInput?.focus?.();
}

function closeBindingDialog() {
  const dialog = document.getElementById('binding-edit-dialog');
  dialog?.close?.();
  if (bindingDialogReturnFocus && typeof bindingDialogReturnFocus.focus === 'function') {
    bindingDialogReturnFocus.focus();
  }
  bindingDialogReturnFocus = null;
}

document.getElementById('binding-edit-dialog')?.addEventListener('close', () => {
  if (bindingDialogReturnFocus && typeof bindingDialogReturnFocus.focus === 'function') {
    bindingDialogReturnFocus.focus();
  }
  bindingDialogReturnFocus = null;
});

function renderBindingsPendingCallout(bindings) {
  const callout = document.getElementById('peer-bindings-pending-callout');
  if (!callout) return;
  callout.hidden = !bindings.some((binding) => (binding.status || 'pending') === 'pending');
}

function bindingWorkspaceLabel(workspaceId) {
  const ws = workspaces.find((item) => item.id === workspaceId);
  return ws ? workspaceLabel(ws) : (workspaceId || '');
}

function bindingStatusLabel(status) {
  const labels = {
    active: 'Active',
    pending: 'Pending - needs tenant ID',
    conflict: 'Conflict - tenant changed',
    inactive: 'Inactive',
  };
  return labels[status] || status;
}

function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;');
}

function openMcpConnectDialog() {
  const dialog = document.getElementById('mcp-connect-dialog');
  const mcpUrl = currentMcpUrl();

  const cursor = document.getElementById('mcp-tile-cursor');
  if (cursor) cursor.href = buildCursorDeepLink(mcpUrl);
  const vscode = document.getElementById('mcp-tile-vscode');
  if (vscode) vscode.href = buildVsCodeDeepLink(mcpUrl);
  const claudeDesktopUrl = document.getElementById('mcp-tile-claude-desktop-url');
  if (claudeDesktopUrl) claudeDesktopUrl.textContent = mcpUrl;
  const claudeCodeCmd = document.getElementById('mcp-tile-claude-code-cmd');
  if (claudeCodeCmd) claudeCodeCmd.textContent = `claude mcp add --transport http ione ${mcpUrl}`;
  const rawJson = document.getElementById('mcp-tile-raw-json');
  if (rawJson) rawJson.textContent = buildRawJsonConfig(mcpUrl);

  wireMcpTileTelemetry();
  loadMcpClients();
  if (mcpClientsInterval) clearInterval(mcpClientsInterval);
  mcpClientsInterval = setInterval(loadMcpClients, 15000);
  dialog?.showModal?.();
}

function wireMcpTileTelemetry() {
  const tiles = [
    ['cursor', document.getElementById('mcp-tile-cursor')],
    ['claudeDesktop', document.getElementById('mcp-tile-claude-desktop-url')?.closest('.mcp-tile')],
    ['claudeCode', document.getElementById('mcp-tile-claude-code-cmd')?.closest('.mcp-tile')],
    ['vscode', document.getElementById('mcp-tile-vscode')],
    ['other', document.getElementById('mcp-tile-raw-json')?.closest('.mcp-tile')],
  ];
  tiles.forEach(([client, tile]) => {
    if (!tile || tile.dataset.telemetryBound === 'true') return;
    tile.dataset.telemetryBound = 'true';
    tile.addEventListener('click', () => {
      track('mcp_install_tile_clicked', { client }, window.activeWorkspace?.id || null);
    });
  });
}

function closeMcpConnectDialog() {
  const dialog = document.getElementById('mcp-connect-dialog');
  dialog?.close?.();
  if (mcpClientsInterval) { clearInterval(mcpClientsInterval); mcpClientsInterval = null; }
}

if (mcpCopyBtn && mcpUrlEl) {
  // Use window.location to derive the correct host/port at runtime.
  const mcpUrl = currentMcpUrl();
  mcpUrlEl.textContent = mcpUrl;

  mcpCopyBtn.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(mcpUrl);
      const original = mcpCopyBtn.textContent;
      mcpCopyBtn.textContent = 'Copied!';
      setTimeout(() => { mcpCopyBtn.textContent = original; }, 1500);
    } catch (_) {
      // Fallback: select the text
      const range = document.createRange();
      range.selectNode(mcpUrlEl);
      window.getSelection().removeAllRanges();
      window.getSelection().addRange(range);
    }
  });
}

document.getElementById('mcp-connect-open-btn')?.addEventListener('click', openMcpConnectDialog);
document.getElementById('mcp-connect-close')?.addEventListener('click', closeMcpConnectDialog);
document.getElementById('mcp-connect-dialog')?.addEventListener('close', () => {
  if (mcpClientsInterval) { clearInterval(mcpClientsInterval); mcpClientsInterval = null; }
});

document.addEventListener('click', (ev) => {
  const btn = ev.target.closest?.('.mcp-copy-btn');
  if (!btn || !btn.dataset.copyTarget) return;
  const target = btn.dataset.copyTarget;
  const el = document.getElementById(target);
  if (!el) return;
  const text = el.textContent || '';
  navigator.clipboard?.writeText(text);
  const orig = btn.textContent;
  btn.textContent = 'Copied';
  setTimeout(() => { btn.textContent = orig; }, 1400);
});
