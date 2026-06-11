const invoke = window.__TAURI__?.core?.invoke;
const listen = window.__TAURI__?.event?.listen;

const els = {
  loginSection: document.getElementById("login-section"),
  driveSection: document.getElementById("drive-section"),
  error: document.getElementById("error"),
  pinnedList: document.getElementById("pinned-list"),
  cacheUsage: document.getElementById("cache-usage"),
  loginBtn: document.getElementById("login-btn"),
  statusCard: document.getElementById("status-card"),
  statusTitle: document.getElementById("status-title"),
  statusDetail: document.getElementById("status-detail"),
  subtitle: document.getElementById("subtitle"),
  mountCta: document.getElementById("mount-cta"),
  openFolderBtn: document.getElementById("open-folder-btn"),
  mountBtn: document.getElementById("mount-btn"),
  unmountBtn: document.getElementById("unmount-btn"),
  folderPicker: document.getElementById("folder-picker"),
  folderBreadcrumb: document.getElementById("folder-breadcrumb"),
  updateBanner: document.getElementById("update-banner"),
};

let updateDownloadUrl = null;
let folderBrowsePath = "";

function showError(message) {
  els.error.textContent = message;
  els.error.classList.remove("hidden");
}

function clearError() {
  els.error.textContent = "";
  els.error.classList.add("hidden");
}

function formatInvokeError(error) {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object") {
    if (typeof error.message === "string") return error.message;
    return JSON.stringify(error);
  }
  return String(error);
}

async function tauriInvoke(command, args = {}) {
  if (!invoke) {
    throw new Error("Tauri API unavailable. Reinstall the desktop app.");
  }
  return invoke(command, args);
}

function setLoginLoading(loading) {
  els.loginBtn.disabled = loading;
  els.loginBtn.textContent = loading ? "Signing in…" : "Sign in";
}

function setButtonLoading(button, loading, idleLabel, loadingLabel) {
  button.disabled = loading;
  button.textContent = loading ? loadingLabel : idleLabel;
}

function applyStatusCard(state) {
  const card = els.statusCard;
  card.className = "status-card";

  if (!state.logged_in) {
    card.classList.add("status-disconnected");
    els.statusTitle.textContent = "Not connected";
    els.statusDetail.textContent = "Sign in to mount your company drive";
    els.subtitle.textContent = "Your company files, one click away";
    return;
  }

  els.subtitle.textContent = state.user_email || state.company_name;

  if (state.sync_phase === "syncing") {
    card.classList.add("status-syncing");
    els.statusTitle.textContent = "Syncing";
    els.statusDetail.textContent = state.sync_detail || "Updating files…";
    return;
  }

  if (state.sync_phase === "error") {
    card.classList.add("status-error");
    els.statusTitle.textContent = "Sync issue";
    els.statusDetail.textContent = state.sync_detail || "Try Sync now";
    return;
  }

  if (!state.online) {
    card.classList.add("status-offline");
    els.statusTitle.textContent = "Offline";
    els.statusDetail.textContent = "Pinned folders remain available";
    return;
  }

  if (state.mounted) {
    card.classList.add("status-mounted");
    els.statusTitle.textContent = "Drive mounted";
    els.statusDetail.textContent = `${state.mount_point} · ${state.last_sync_human}`;
    return;
  }

  card.classList.add("status-connected");
  els.statusTitle.textContent = state.company_name;
  els.statusDetail.textContent = "Ready to mount";
}

async function refreshState() {
  const state = await tauriInvoke("get_state");
  document.getElementById("api-url").value =
    state.api_base_url || document.getElementById("api-url").value;

  applyStatusCard(state);

  if (state.logged_in) {
    els.loginSection.classList.add("hidden");
    els.driveSection.classList.remove("hidden");

    const showMountCta = !state.mounted;
    els.mountCta.classList.toggle("hidden", !showMountCta);
    els.mountBtn.classList.toggle("hidden", showMountCta);
    els.unmountBtn.classList.toggle("hidden", !state.mounted);

    els.openFolderBtn.disabled = !state.mounted;

    if (state.app_version) {
      document.getElementById("app-version").textContent =
        `v${state.app_version}`;
    }

    if (!els.folderPicker.dataset.loaded) {
      await loadFolderPicker("");
    }

    const paths = state.pinned_paths || [];
    els.pinnedList.innerHTML = paths.length
      ? paths
          .map(
            (p) =>
              `<li><span class="path">${escapeHtml(p)}</span><button data-unpin="${escapeAttr(p)}" class="btn-ghost">Unpin</button></li>`,
          )
          .join("")
      : `<li class="pinned-empty">No offline folders yet. Pin a project folder to work without Wi‑Fi.</li>`;

    els.cacheUsage.textContent = state.cache_usage_human || "";
    document.getElementById("auto-start").checked = !!state.auto_start;
    document.getElementById("auto-mount").checked = !!state.auto_mount;
  } else {
    els.loginSection.classList.remove("hidden");
    els.driveSection.classList.add("hidden");
  }
}

function escapeHtml(value) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function escapeAttr(value) {
  return escapeHtml(value).replaceAll('"', "&quot;");
}

function folderBreadcrumbLabel(path) {
  if (!path) return "Drive";
  return path.split("/").filter(Boolean).join(" / ");
}

function parentFolderPath(path) {
  if (!path) return "";
  const parts = path.split("/").filter(Boolean);
  parts.pop();
  return parts.join("/");
}

async function loadFolderPicker(path) {
  folderBrowsePath = path || "";
  els.folderBreadcrumb.textContent = folderBreadcrumbLabel(folderBrowsePath);
  document.getElementById("folder-up-btn").disabled = !folderBrowsePath;

  try {
    const folders = await tauriInvoke("list_drive_folders", {
      path: folderBrowsePath || null,
    });
    els.folderPicker.innerHTML = `<option value="">Select a folder…</option>`;
    for (const folder of folders) {
      const option = document.createElement("option");
      option.value = folder.path;
      option.textContent = folder.name;
      els.folderPicker.appendChild(option);
    }
    els.folderPicker.dataset.loaded = "1";
  } catch (e) {
    els.folderPicker.innerHTML = `<option value="">Could not load folders</option>`;
  }
}

async function checkForUpdate() {
  try {
    const update = await tauriInvoke("check_for_update");
    if (!update.update_available || !update.download_url) {
      els.updateBanner.classList.add("hidden");
      return;
    }
    updateDownloadUrl = update.download_url;
    document.getElementById("update-title").textContent =
      `Update available · v${update.latest_version}`;
    document.getElementById("update-notes").textContent =
      update.release_notes ||
      "A newer version of Remnant Finder Drive is ready.";
    els.updateBanner.classList.remove("hidden");
  } catch (_) {
    els.updateBanner.classList.add("hidden");
  }
}

async function handleLogin() {
  clearError();
  setLoginLoading(true);
  try {
    await tauriInvoke("login", {
      apiUrl: document.getElementById("api-url").value.trim(),
      email: document.getElementById("email").value.trim(),
      password: document.getElementById("password").value,
    });
    document.getElementById("password").value = "";
    els.folderPicker.dataset.loaded = "";
    await refreshState();
    await checkForUpdate();
  } catch (e) {
    showError(formatInvokeError(e));
  } finally {
    setLoginLoading(false);
  }
}

async function handleMount(button) {
  clearError();
  setButtonLoading(
    button,
    true,
    button.dataset.idle || "Mount Drive",
    "Mounting…",
  );
  try {
    await tauriInvoke("mount_drive");
    await refreshState();
  } catch (e) {
    showError(formatInvokeError(e));
  } finally {
    setButtonLoading(
      button,
      false,
      button.dataset.idle || "Mount Drive",
      "Mounting…",
    );
  }
}

els.loginBtn.addEventListener("click", handleLogin);
document.getElementById("password").addEventListener("keydown", (e) => {
  if (e.key === "Enter") handleLogin();
});

document.getElementById("mount-btn-primary").addEventListener("click", (e) => {
  handleMount(e.currentTarget);
});
document.getElementById("mount-btn-primary").dataset.idle = "Mount Drive";

document.getElementById("mount-btn").addEventListener("click", (e) => {
  handleMount(e.currentTarget);
});
document.getElementById("mount-btn").dataset.idle = "Mount";

document.getElementById("unmount-btn").addEventListener("click", async () => {
  clearError();
  const btn = els.unmountBtn;
  setButtonLoading(btn, true, "Unmount", "Unmounting…");
  try {
    await tauriInvoke("unmount_drive");
    await refreshState();
  } catch (e) {
    showError(formatInvokeError(e));
  } finally {
    setButtonLoading(btn, false, "Unmount", "Unmounting…");
  }
});

els.folderPicker.addEventListener("change", (e) => {
  const path = e.target.value;
  if (path) {
    document.getElementById("pin-path").value = path;
  }
});

document
  .getElementById("folder-open-btn")
  .addEventListener("click", async () => {
    const path = els.folderPicker.value;
    if (!path) return;
    document.getElementById("pin-path").value = path;
    await loadFolderPicker(path);
  });

document.getElementById("folder-up-btn").addEventListener("click", async () => {
  await loadFolderPicker(parentFolderPath(folderBrowsePath));
});

document.getElementById("update-btn").addEventListener("click", async () => {
  if (!updateDownloadUrl) return;
  try {
    await window.__TAURI__.opener.openUrl(updateDownloadUrl);
  } catch (e) {
    showError(formatInvokeError(e));
  }
});

document.getElementById("sync-btn").addEventListener("click", async () => {
  clearError();
  const btn = document.getElementById("sync-btn");
  setButtonLoading(btn, true, "Sync now", "Syncing…");
  try {
    await tauriInvoke("sync_now");
    await refreshState();
  } catch (e) {
    showError(formatInvokeError(e));
  } finally {
    setButtonLoading(btn, false, "Sync now", "Syncing…");
  }
});

document
  .getElementById("open-folder-btn")
  .addEventListener("click", async () => {
    try {
      await tauriInvoke("open_mount");
    } catch (e) {
      showError(formatInvokeError(e));
    }
  });

document.getElementById("logout-btn").addEventListener("click", async () => {
  try {
    await tauriInvoke("logout");
    await refreshState();
  } catch (e) {
    showError(formatInvokeError(e));
  }
});

document.getElementById("pin-btn").addEventListener("click", async () => {
  clearError();
  const path = document.getElementById("pin-path").value.trim();
  if (!path) return;
  const btn = document.getElementById("pin-btn");
  setButtonLoading(btn, true, "Pin", "Pinning…");
  try {
    await tauriInvoke("pin_folder", { path });
    document.getElementById("pin-path").value = "";
    await refreshState();
  } catch (e) {
    showError(formatInvokeError(e));
  } finally {
    setButtonLoading(btn, false, "Pin", "Pinning…");
  }
});

document.getElementById("auto-start").addEventListener("change", async (e) => {
  try {
    await tauriInvoke("set_auto_start", { enabled: e.target.checked });
  } catch (err) {
    showError(formatInvokeError(err));
  }
});

document.getElementById("auto-mount").addEventListener("change", async (e) => {
  try {
    await tauriInvoke("set_auto_mount", { enabled: e.target.checked });
  } catch (err) {
    showError(formatInvokeError(err));
  }
});

els.pinnedList.addEventListener("click", async (e) => {
  const path = e.target.getAttribute("data-unpin");
  if (!path) return;
  try {
    await tauriInvoke("unpin_folder", { path });
    await refreshState();
  } catch (err) {
    showError(formatInvokeError(err));
  }
});

if (listen) {
  listen("status-changed", () => {
    refreshState().catch(() => {});
  });
}

refreshState()
  .then(() => checkForUpdate())
  .catch((e) => showError(formatInvokeError(e)));
