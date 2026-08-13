const invoke = window.__TAURI__?.core?.invoke;

const state = {
  snapshot: null,
  selectedPeerId: null,
  edge: "right",
  dirty: false,
  busy: false,
};

const elements = Object.fromEntries(
  [
    "platformLabel", "versionLabel", "enabledToggle", "switchTitle", "switchSubtitle",
    "statusOrb", "statePill", "latencyLabel", "statusMessage", "statusDetail",
    "returnButton", "screenLayout", "peerComputer", "peerOs", "peerScreenName",
    "peerAddress", "localScreenName", "layoutHint", "peerList", "manualAddress",
    "edgeDelay", "delayOutput", "hotkeyLabel", "pairingKey", "revealKey", "copyKey",
    "generateKey", "permissionCard", "permissionTitle", "permissionMessage",
    "permissionButton", "deviceName", "saveHint", "saveButton", "scanIndicator", "toast",
    "onlineCount", "securityState", "edgeSummary",
  ].map((id) => [id, document.getElementById(id)])
);

const stateLabels = {
  paused: "已暂停",
  searching: "正在发现",
  ready: "已就绪",
  connecting: "连接中",
  controlling: "正在控制",
  controlled: "正在被控制",
  error: "需要处理",
};

async function call(command, args = {}) {
  if (!invoke) throw new Error("Tauri API 不可用，请通过 DeskBridge 桌面程序打开此页面。");
  return invoke(command, args);
}

async function refresh({ forceFields = false } = {}) {
  try {
    const snapshot = await call("get_snapshot");
    render(snapshot, forceFields || !state.snapshot);
  } catch (error) {
    showToast(String(error), true);
  }
}

function render(snapshot, forceFields = false) {
  state.snapshot = snapshot;
  const { settings, status, permissions, peers } = snapshot;
  if (!state.dirty || forceFields) {
    state.selectedPeerId = settings.selectedPeerId ?? null;
    state.edge = settings.peerEdge;
  }

  elements.platformLabel.textContent = snapshot.platform === "macos" ? "macOS" : "Windows";
  elements.versionLabel.textContent = `v${snapshot.version}`;
  elements.enabledToggle.checked = settings.enabled;
  elements.switchTitle.textContent = settings.enabled ? "共享已开启" : "共享已暂停";
  elements.switchSubtitle.textContent = settings.enabled ? "输入仅发送到已配对设备" : "开启后才会发送或接收输入";

  const selectedOnline = peers.some((peer) => peer.nodeId === settings.selectedPeerId);
  const configuredAndWaiting = settings.enabled
    && status.state === "searching"
    && (selectedOnline || settings.manualAddress);
  const visibleState = configuredAndWaiting ? "ready" : status.state;
  const visibleMessage = configuredAndWaiting
    ? `已就绪；持续推向${settings.peerEdge === "right" ? "右" : "左"}侧边缘即可切换。`
    : status.message;
  elements.statePill.textContent = stateLabels[visibleState] ?? visibleState;
  elements.statusMessage.textContent = visibleMessage;
  elements.latencyLabel.textContent = status.latencyMs == null ? "" : `${status.latencyMs} ms 往返`;
  elements.statusDetail.textContent = status.lastError ?? "两台电脑需连接到同一个 Wi‑Fi 或有线局域网。";
  elements.statusOrb.className = `status-orb ${visibleState === "error" ? "error" : visibleState === "paused" ? "paused" : ""}`;

  const mayRefreshDraft = forceFields || !state.dirty;
  if (mayRefreshDraft && (forceFields || !isFieldFocused(elements.deviceName))) elements.deviceName.value = settings.deviceName;
  if (mayRefreshDraft && (forceFields || !isFieldFocused(elements.manualAddress))) elements.manualAddress.value = settings.manualAddress;
  if (mayRefreshDraft && (forceFields || !isFieldFocused(elements.pairingKey))) elements.pairingKey.value = snapshot.pairingKeyDisplay;
  if (mayRefreshDraft && (forceFields || !isFieldFocused(elements.edgeDelay))) elements.edgeDelay.value = settings.edgeDelayMs;
  elements.delayOutput.textContent = `${elements.edgeDelay.value} ms`;
  elements.localScreenName.textContent = settings.deviceName;
  elements.hotkeyLabel.textContent = snapshot.platform === "macos"
    ? "Control + Option + Shift + Esc"
    : "Ctrl + Alt + Shift + Esc";
  elements.onlineCount.textContent = String(peers.length);
  elements.securityState.textContent = permissions.capture && permissions.injection ? "已就绪" : "待授权";

  renderEdges();
  renderPeers(peers);
  renderPermission(permissions, snapshot.platform);
  updateSelectedPeerVisual(peers);
  updateSaveButton();
}

function renderEdges() {
  document.querySelectorAll("[data-edge]").forEach((button) => {
    const selected = button.dataset.edge === state.edge;
    button.classList.toggle("selected", selected);
    button.setAttribute("aria-pressed", String(selected));
  });
  elements.screenLayout.classList.toggle("peer-right", state.edge === "right");
  const delay = elements.edgeDelay.value;
  elements.edgeSummary.textContent = `向${state.edge === "right" ? "右" : "左"}切换`;
  elements.layoutHint.textContent = `持续把鼠标推向${state.edge === "right" ? "右" : "左"}侧边缘约 ${delay} ms 后切换；四角保留防误触区。`;
}

function renderPeers(peers) {
  if (!peers.length) {
    elements.scanIndicator.innerHTML = "<i></i>扫描中";
    elements.peerList.innerHTML = `
      <div class="empty-state">
        <div class="radar"><i></i></div>
        <strong>正在寻找 DeskBridge 设备</strong>
        <p>请在另一台电脑上打开 DeskBridge。</p>
      </div>`;
    return;
  }
  elements.scanIndicator.innerHTML = `<i></i>${peers.length} 台在线`;
  elements.peerList.innerHTML = peers.map((peer) => `
    <button class="peer-option ${peer.nodeId === state.selectedPeerId ? "selected" : ""}" data-peer-id="${escapeHtml(peer.nodeId)}">
      <span class="peer-logo">${peer.os === "macOS" ? "Mac" : "Win"}</span>
      <span class="peer-copy">
        <strong>${escapeHtml(peer.name)}</strong>
        <small>${escapeHtml(peer.os)} · ${escapeHtml(peer.address)}:${peer.port}</small>
      </span>
      <span class="radio"></span>
    </button>`).join("");
  elements.peerList.querySelectorAll("[data-peer-id]").forEach((button) => {
    button.addEventListener("click", () => {
      state.selectedPeerId = button.dataset.peerId;
      elements.manualAddress.value = "";
      markDirty();
      renderPeers(peers);
      updateSelectedPeerVisual(peers);
    });
  });
}

function updateSelectedPeerVisual(peers) {
  const peer = peers.find((item) => item.nodeId === state.selectedPeerId);
  if (peer) {
    elements.peerOs.textContent = peer.os;
    elements.peerScreenName.textContent = peer.name;
    elements.peerAddress.textContent = `${peer.address}:${peer.port}`;
  } else if (elements.manualAddress.value.trim()) {
    elements.peerOs.textContent = "手动";
    elements.peerScreenName.textContent = "手动连接";
    elements.peerAddress.textContent = elements.manualAddress.value.trim();
  } else {
    elements.peerOs.textContent = "对端";
    elements.peerScreenName.textContent = "选择一台设备";
    elements.peerAddress.textContent = "等待发现";
  }
}

function renderPermission(permission, platform) {
  const ready = permission.capture && permission.injection;
  elements.permissionCard.classList.toggle("warning", !ready);
  elements.permissionCard.querySelector(".permission-icon").textContent = ready ? "✓" : "!";
  elements.permissionTitle.textContent = ready ? "系统权限已就绪" : "需要系统权限";
  elements.permissionMessage.textContent = permission.message;
  elements.permissionButton.textContent = platform === "macos" && !ready ? "打开系统设置" : "重新检测";
}

function markDirty() {
  state.dirty = true;
  elements.saveHint.textContent = "有未保存的修改";
  elements.saveHint.style.color = "var(--amber)";
}

function activationIssue() {
  const permissions = state.snapshot?.permissions;
  if (!permissions?.capture || !permissions?.injection) return "请先授予鼠标键盘控制权限";
  if (!state.selectedPeerId && !elements.manualAddress.value.trim()) return "请先选择另一台电脑，或填写手动 IP";
  return null;
}

function updateSaveButton() {
  elements.enabledToggle.disabled = state.busy;
  elements.saveButton.disabled = state.busy;
  if (state.busy) return;
  elements.saveButton.textContent = elements.enabledToggle.checked ? "保存设置" : "保存并开启";
}

async function saveSettings({ enabled = elements.enabledToggle.checked, successMessage } = {}) {
  if (!state.snapshot || state.busy) return false;
  if (enabled) {
    const issue = activationIssue();
    if (issue) {
      elements.enabledToggle.checked = false;
      updateSaveButton();
      showToast(issue, true);
      if (!state.snapshot.permissions.capture || !state.snapshot.permissions.injection) {
        elements.permissionCard.scrollIntoView({ behavior: "smooth", block: "center" });
      }
      return false;
    }
  }
  state.busy = true;
  updateSaveButton();
  elements.saveButton.textContent = "保存中…";
  try {
    const snapshot = await call("update_settings", {
      update: {
        deviceName: elements.deviceName.value,
        pairingKey: elements.pairingKey.value,
        selectedPeerId: state.selectedPeerId,
        manualAddress: elements.manualAddress.value,
        peerEdge: state.edge,
        edgeDelayMs: Number(elements.edgeDelay.value),
        enabled,
      },
    });
    state.dirty = false;
    elements.saveHint.textContent = "设置已保存";
    elements.saveHint.style.color = "var(--green)";
    render(snapshot, true);
    showToast(successMessage ?? (enabled ? "设置已保存，共享已开启" : "设置已保存，共享已暂停"));
    return true;
  } catch (error) {
    elements.enabledToggle.checked = state.snapshot.settings.enabled;
    showToast(String(error), true);
    return false;
  } finally {
    state.busy = false;
    updateSaveButton();
  }
}

elements.enabledToggle.addEventListener("change", async () => {
  const enabled = elements.enabledToggle.checked;
  await saveSettings({
    enabled,
    successMessage: enabled ? "设置已保存，共享已开启" : "共享已暂停，输入已返回本机",
  });
});

elements.returnButton.addEventListener("click", async () => {
  try {
    render(await call("return_control"));
    showToast("输入已返回本机");
  } catch (error) { showToast(String(error), true); }
});

document.querySelectorAll("[data-edge]").forEach((button) => {
  button.addEventListener("click", () => {
    state.edge = button.dataset.edge;
    renderEdges();
    markDirty();
  });
});

elements.edgeDelay.addEventListener("input", () => {
  elements.delayOutput.textContent = `${elements.edgeDelay.value} ms`;
  renderEdges();
  markDirty();
});

[elements.deviceName, elements.manualAddress, elements.pairingKey].forEach((field) => {
  field.addEventListener("input", () => {
    if (field === elements.manualAddress && field.value.trim()) {
      state.selectedPeerId = null;
      renderPeers(state.snapshot?.peers ?? []);
    }
    markDirty();
    if (field === elements.manualAddress) updateSelectedPeerVisual(state.snapshot?.peers ?? []);
  });
});

elements.revealKey.addEventListener("click", () => {
  const revealing = elements.pairingKey.type === "password";
  elements.pairingKey.type = revealing ? "text" : "password";
  elements.revealKey.setAttribute("aria-pressed", String(revealing));
  elements.revealKey.setAttribute("aria-label", revealing ? "隐藏密钥" : "显示密钥");
});

elements.copyKey.addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(elements.pairingKey.value);
    showToast("配对密钥已复制");
  } catch (_) { showToast("无法访问剪贴板，请手动选择复制", true); }
});

elements.generateKey.addEventListener("click", async () => {
  if (!confirm("重置配对会暂停共享，并使原密钥立即失效。要继续吗？")) return;
  try {
    const snapshot = await call("generate_pairing_key");
    state.dirty = false;
    render(snapshot, true);
    showToast("配对已重置；请把新密钥复制到另一台电脑");
  } catch (error) { showToast(String(error), true); }
});

elements.permissionButton.addEventListener("click", async () => {
  if (state.busy) return;
  state.busy = true;
  elements.permissionButton.disabled = true;
  elements.permissionButton.textContent = "检测中…";
  try { render(await call("request_permissions")); }
  catch (error) { showToast(String(error), true); }
  finally {
    state.busy = false;
    elements.permissionButton.disabled = false;
    updateSaveButton();
  }
});

elements.saveButton.addEventListener("click", () => {
  saveSettings({ enabled: true, successMessage: "设置已保存，共享已开启" });
});

document.querySelectorAll("[data-scroll]").forEach((button) => {
  button.addEventListener("click", () => {
    document.getElementById(button.dataset.scroll)?.scrollIntoView({ behavior: "smooth" });
    document.querySelectorAll(".nav-item").forEach((item) => item.classList.remove("active"));
    button.classList.add("active");
  });
});

function isFieldFocused(field) { return document.activeElement === field; }
function escapeHtml(value) {
  return String(value).replace(/[&<>'"]/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;",
  })[character]);
}

let toastTimer;
function showToast(message, error = false) {
  clearTimeout(toastTimer);
  elements.toast.textContent = message;
  elements.toast.className = `toast show${error ? " error" : ""}`;
  toastTimer = setTimeout(() => { elements.toast.className = "toast"; }, 3000);
}

refresh({ forceFields: true });
setInterval(() => {
  if (!state.busy && !document.hidden) refresh();
}, 2000);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && !state.busy) refresh();
});
