<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import Select from './lib/Select.svelte';
  import TransportIcon from './lib/TransportIcon.svelte';
  import {
    clearClipboard,
    clearLogs,
    clearNotifications,
    clearSmsCache,
    clearTransfers,
    chooseSharedFolder,
    copyClipboardLocal,
    createCommand,
    createVirtualDisplay,
    deleteCommand,
    destroyVirtualDisplay,
    dismissNotification,
    getDesktopState,
    importDroppedFiles,
    mediaCommand,
    notificationIcon,
    openSharedFolder,
    openTransfer,
    respondToPairing,
    runCommand,
    screenPreview,
    selectOutput,
    selectSmsThread,
    sendClipboard,
    sendSms,
    smsContactPhoto,
    setCameraAutoLoad,
    setCameraDevice,
    setCameraExposure,
    setCameraStreamParams,
    setCameraTorch,
    setCameraZoom,
    setSideboatVisible,
    setStreaming,
    setupCamera,
    switchCamera,
    unpairDevice,
    updateCommand,
    type DesktopState,
    type Media,
    type SavedCommand,
  } from './lib/anchor';

  type Section = 'sideboat' | 'messages' | 'foghorn' | 'clipboard' | 'files' | 'commands' | 'media' | 'settings' | 'logs';
  const navigation: Array<{ id: Section; label: string }> = [
    { id: 'sideboat', label: 'Sideboat' },
    { id: 'messages', label: 'Messages' },
    { id: 'foghorn', label: 'Foghorn' },
    { id: 'clipboard', label: 'Clipboard' },
    { id: 'files', label: 'Files' },
    { id: 'commands', label: 'Commands' },
    { id: 'media', label: 'Media' },
    { id: 'settings', label: 'Settings' },
    { id: 'logs', label: 'Logs' },
  ];

  let active: Section = 'sideboat';
  let desktop: DesktopState | undefined;
  let appVersion: string | undefined;
  let actionError = '';
  let busy = false;
  let smsDraft = '';
  let smsSearch = '';
  let editingCommandId: string | undefined;
  let commandEditorOpen = false;
  let commandName = '';
  let commandLine = '';
  let commandDescription = '';
  let commandDetach = false;
  let logFilter = '';
  let notificationIcons = new Map<string, string>();
  let smsPhotos = new Map<number, string>();
  let filesDragActive = false;
  let fileDropStatus = '';
  let messageList: HTMLDivElement | undefined;
  let lastMessageRenderKey = '';
  let logList: HTMLDivElement | undefined;
  let lastLogRenderKey = '';
  let refreshRequestId = 0;
  let latestAppliedRequestId = 0;
  let sectionLoading = true;
  const sectionState = new Map<Section, DesktopState>();
  let previewSequence = 0;
  let previewUrl = '';
  let previewRequestRunning = false;
  let windowFocused = true;
  let previewTimer = 0;
  let virtualDisplayOpen = false;
  let virtualSizeMode: 'phone' | 'custom' = 'phone';
  let virtualOrientation: 'portrait' | 'landscape' = 'portrait';
  let virtualScale = 1;
  let virtualWidth = 1920;
  let virtualHeight = 1080;
  let copiedClipboardHash = '';
  let clipboardFeedbackTimer = 0;
  const attemptedNotificationIcons = new Set<string>();
  const attemptedSmsPhotos = new Set<number>();

  const scrollMessagesToBottom = async () => {
    await tick();
    if (messageList) messageList.scrollTop = messageList.scrollHeight;
  };

  const scrollLogsToBottom = async () => {
    await tick();
    if (logList) logList.scrollTop = logList.scrollHeight;
  };

  const refreshScreenPreview = async () => {
    if (!windowFocused || active !== 'sideboat' || previewRequestRunning || desktop?.capture.status === 'streaming') return;
    previewRequestRunning = true;
    try {
      const preview = await screenPreview(previewSequence);
      if (active === 'sideboat' && preview) {
        previewSequence = preview.sequence;
        previewUrl = preview.dataUrl;
      }
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      previewRequestRunning = false;
    }
  };

  const schedulePreview = () => {
    window.clearTimeout(previewTimer);
    if (!windowFocused || active !== 'sideboat' || desktop?.capture.status === 'streaming') return;
    previewTimer = window.setTimeout(async () => {
      await refreshScreenPreview();
      schedulePreview();
    }, 500);
  };

  const loadNotificationIcons = async (state: DesktopState) => {
    const packages = [...new Set(state.notifications.map((notification) => notification.iconKey).filter((iconKey) => !iconKey.endsWith(':')))];
    const missing = packages.filter((iconKey) => !attemptedNotificationIcons.has(iconKey));
    if (!missing.length) return;
    missing.forEach((iconKey) => attemptedNotificationIcons.add(iconKey));
    const loaded = await Promise.all(missing.map(async (iconKey) => [iconKey, await notificationIcon(iconKey)] as const));
    let changed = false;
    for (const [iconKey, icon] of loaded) {
      if (icon) {
        notificationIcons.set(iconKey, icon);
        changed = true;
      }
    }
    if (changed) notificationIcons = new Map(notificationIcons);
  };

  const loadSmsPhotos = async (state: DesktopState) => {
    const missing = state.smsThreads
      .filter((thread) => thread.hasPhoto && !attemptedSmsPhotos.has(thread.threadId))
      .map((thread) => thread.threadId);
    if (!missing.length) return;
    missing.forEach((threadId) => attemptedSmsPhotos.add(threadId));
    const loaded = await Promise.all(missing.map(async (threadId) => [threadId, await smsContactPhoto(threadId)] as const));
    let changed = false;
    for (const [threadId, photo] of loaded) {
      if (photo) {
        smsPhotos.set(threadId, photo);
        changed = true;
      }
    }
    if (changed) smsPhotos = new Map(smsPhotos);
  };

  const refresh = async (section: Section = active) => {
    const requestId = ++refreshRequestId;
    try {
      const state = await getDesktopState(section);
      if (section !== active || requestId < latestAppliedRequestId) return;
      latestAppliedRequestId = requestId;
      sectionState.set(section, state);
      const messages = state.smsMessages;
      const newest = messages.at(-1);
      const messageRenderKey = section === 'messages'
        ? `${state.selectedSmsThread ?? ''}:${messages.length}:${newest?.uid ?? ''}:${newest?.date ?? ''}`
        : '';
      const shouldScrollMessages = messageRenderKey !== '' && messageRenderKey !== lastMessageRenderKey;
      if (section === 'messages') lastMessageRenderKey = messageRenderKey;
      const logQuery = logFilter.trim().toLowerCase();
      const visibleLogs = section === 'logs'
        ? state.logs.filter((entry) => !logQuery || `${entry.source} ${entry.level} ${entry.message}`.toLowerCase().includes(logQuery))
        : [];
      const newestLog = visibleLogs.at(-1);
      const logRenderKey = newestLog
        ? `${visibleLogs.length}:${newestLog.timestamp}:${newestLog.source}:${newestLog.message}`
        : '';
      const shouldScrollLogs = logRenderKey !== '' && logRenderKey !== lastLogRenderKey;
      if (section === 'logs') lastLogRenderKey = logRenderKey;
      desktop = state;
      appVersion = state.version;
      sectionLoading = false;
      if (section === 'sideboat') schedulePreview();
      if (section === 'foghorn') void loadNotificationIcons(state);
      if (section === 'messages') void loadSmsPhotos(state);
      if (shouldScrollMessages) void scrollMessagesToBottom();
      if (shouldScrollLogs) void scrollLogsToBottom();
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    }
  };

  onMount(() => {
    let stopped = false;
    let stateDirtyTimer = 0;
    let startupReconcileTimer = 0;
    let unlistenDragDrop: (() => void) | undefined;
    let unlistenStateDirty: (() => void) | undefined;
    let unlistenFocus: (() => void) | undefined;
    const scheduleRefresh = (delay = 60) => {
      window.clearTimeout(stateDirtyTimer);
      stateDirtyTimer = window.setTimeout(() => {
        if (!stopped) void refresh();
      }, delay);
    };
    const initialiseState = () => {
      if (stopped) return;
      void refresh();
      // Native services start alongside the webview. Reconcile once after
      // their initial Wayland/device events settle in case they predate the
      // frontend event subscription; this is not a recurring poll.
      startupReconcileTimer = window.setTimeout(() => {
        if (!stopped) void refresh();
      }, 750);
      void refreshScreenPreview().finally(schedulePreview);
      void setSideboatVisible(windowFocused && active === 'sideboat').catch((error) => {
        actionError = error instanceof Error ? error.message : String(error);
      });
    };
    void listen('state-dirty', () => scheduleRefresh()).then((unlisten) => {
      if (stopped) unlisten();
      else {
        unlistenStateDirty = unlisten;
        // The backend can publish its initial output list while the webview
        // is mounting. Subscribe first so that initial state-dirty signal
        // cannot be lost between this first fetch and the listener setup.
        initialiseState();
      }
    }).catch((error) => {
      actionError = `State updates are unavailable: ${String(error)}`;
      initialiseState();
    });
    void getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      windowFocused = focused;
      void setSideboatVisible(focused && active === 'sideboat').catch((error) => {
        actionError = error instanceof Error ? error.message : String(error);
      });
      if (focused) scheduleRefresh(0);
      schedulePreview();
    }).then((unlisten) => {
      if (stopped) unlisten();
      else unlistenFocus = unlisten;
    });
    void getCurrentWindow().onDragDropEvent(({ payload }) => {
      if (active !== 'files') return;
      if (payload.type === 'enter' || payload.type === 'over') {
        filesDragActive = true;
      } else if (payload.type === 'leave') {
        filesDragActive = false;
      } else if (payload.type === 'drop') {
        filesDragActive = false;
        void acceptDroppedFiles(payload.paths);
      }
    }).then((unlisten) => {
      if (stopped) unlisten();
      else unlistenDragDrop = unlisten;
    }).catch((error) => {
      actionError = `File drag and drop is unavailable: ${String(error)}`;
    });
    return () => {
      stopped = true;
      window.clearTimeout(stateDirtyTimer);
      window.clearTimeout(startupReconcileTimer);
      window.clearTimeout(previewTimer);
      window.clearTimeout(clipboardFeedbackTimer);
      unlistenDragDrop?.();
      unlistenStateDirty?.();
      unlistenFocus?.();
    };
  });

  const runAction = async (action: () => Promise<unknown>) => {
    busy = true;
    actionError = '';
    try {
      await action();
      await refresh();
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  };

  const acceptDroppedFiles = async (paths: string[]) => {
    if (!paths.length) return;
    busy = true;
    actionError = '';
    fileDropStatus = 'Adding files…';
    try {
      const result = await importDroppedFiles(paths);
      if (result.imported) {
        const noun = result.imported === 1 ? 'file' : 'files';
        fileDropStatus = `${result.imported} ${noun} queued to send`;
      } else {
        fileDropStatus = 'No files were added';
      }
      if (result.errors.length) actionError = result.errors.join('\n');
      await refresh();
    } catch (error) {
      fileDropStatus = '';
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  };

  const copyClipboardEntry = async (hash: string) => {
    await copyClipboardLocal(hash);
    copiedClipboardHash = hash;
    window.clearTimeout(clipboardFeedbackTimer);
    clipboardFeedbackTimer = window.setTimeout(() => {
      if (copiedClipboardHash === hash) copiedClipboardHash = '';
    }, 1500);
  };

  const selectSection = async (section: Section) => {
    if (section === active) return;
    const sideboatVisibilityChanged = (active === 'sideboat') !== (section === 'sideboat');
    if (section === 'messages' && active !== 'messages') lastMessageRenderKey = '';
    if (section === 'logs' && active !== 'logs') lastLogRenderKey = '';
    active = section;
    const cached = sectionState.get(section);
    // Paint the last known snapshot immediately. A first visit gets a stable
    // empty shell instead of keeping the previous panel on screen while IPC
    // builds the new snapshot.
    desktop = cached;
    sectionLoading = !cached;
    // Let Svelte commit the navigation before serializing state through
    // Tauri IPC. This keeps tab selection responsive even on a cold panel.
    await tick();
    requestAnimationFrame(() => {
      void refresh(section);
      if (section === 'sideboat') void refreshScreenPreview().finally(schedulePreview);
    });
  };

  const changeOutput = (value: string) => {
    void runAction(() => selectOutput(Number(value)));
  };

  const phoneDisplay = () => desktop?.devices.find((device) => device.online && device.displayWidth && device.displayHeight);
  const virtualDimensions = () => {
    if (virtualSizeMode === 'custom') return { width: Number(virtualWidth), height: Number(virtualHeight) };
    const display = phoneDisplay();
    if (!display) return undefined;
    const shortEdge = Math.min(display.displayWidth!, display.displayHeight!);
    const longEdge = Math.max(display.displayWidth!, display.displayHeight!);
    const [width, height] = virtualOrientation === 'portrait'
      ? [shortEdge, longEdge]
      : [longEdge, shortEdge];
    return {
      width: Math.round(width * virtualScale / 2) * 2,
      height: Math.round(height * virtualScale / 2) * 2,
    };
  };
  const virtualLoad = () => {
    const { width, height } = virtualDimensions() ?? { width: 0, height: 0 };
    const pixels = width * height;
    if (pixels <= 1_100_000) return 'Light capture load';
    if (pixels <= 3_000_000) return 'Standard capture load';
    return 'High-detail capture load';
  };
  const createVirtualOutput = () => {
    const dimensions = virtualDimensions();
    if (!dimensions) return;
    void runAction(async () => {
      await createVirtualDisplay(dimensions.width, dimensions.height);
      virtualDisplayOpen = false;
    });
  };

  const chooseSmsThread = (threadId: number) => {
    smsDraft = '';
    lastMessageRenderKey = '';
    void runAction(() => selectSmsThread(threadId));
  };

  const submitSms = async (event: SubmitEvent) => {
    event.preventDefault();
    const threadId = desktop?.selectedSmsThread;
    if (threadId === undefined || !desktop?.smsAvailable || !smsDraft.trim()) return;
    const body = smsDraft;
    busy = true;
    actionError = '';
    try {
      await sendSms(threadId, body);
      smsDraft = '';
      await refresh();
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  };

  const deleteLocalSmsCopy = () => {
    if (!window.confirm('Delete Anchor’s local copy of all message and contact data on this desktop?')) return;
    void runAction(async () => {
      await clearSmsCache();
      smsPhotos = new Map();
      smsDraft = '';
      smsSearch = '';
    });
  };

  const submitCommand = (event: SubmitEvent) => {
    event.preventDefault();
    if (!commandName.trim() || !commandLine.trim()) return;
    void runAction(async () => {
      if (editingCommandId) {
        await updateCommand(editingCommandId, commandName, commandLine, commandDescription, commandDetach);
      } else {
        await createCommand(commandName, commandLine, commandDescription, commandDetach);
      }
      resetCommandForm();
    });
  };

  const editCommand = (saved: SavedCommand) => {
    commandEditorOpen = true;
    editingCommandId = saved.id;
    commandName = saved.name;
    commandLine = saved.command;
    commandDescription = saved.description;
    commandDetach = saved.detach;
  };

  const resetCommandForm = () => {
    commandEditorOpen = false;
    editingCommandId = undefined;
    commandName = '';
    commandLine = '';
    commandDescription = '';
    commandDetach = false;
  };

  const newCommand = () => {
    resetCommandForm();
    commandEditorOpen = true;
  };

  const removeCommand = (saved: SavedCommand) => {
    if (window.confirm(`Delete “${saved.name}”?`)) {
      void runAction(() => deleteCommand(saved.id));
    }
  };

  const changeCameraDevice = (value: string) => {
    const deviceId = value || undefined;
    void runAction(() => setCameraDevice(deviceId));
  };

  const changeCameraAutoLoad = (event: Event) => {
    void runAction(() => setCameraAutoLoad((event.currentTarget as HTMLInputElement).checked));
  };

  const changeCameraFps = (value: string) => {
    if (!desktop) return;
    void runAction(() => setCameraStreamParams(Number(value), desktop!.camera.bitrateKbps));
  };

  const changeCameraBitrate = (event: Event) => {
    if (!desktop) return;
    void runAction(() => setCameraStreamParams(desktop!.camera.fps, Number((event.currentTarget as HTMLInputElement).value)));
  };

  const changeCameraZoom = (event: Event) => {
    void runAction(() => setCameraZoom(Number((event.currentTarget as HTMLInputElement).value)));
  };

  const changeCameraExposure = (event: Event) => {
    void runAction(() => setCameraExposure(Number((event.currentTarget as HTMLInputElement).value)));
  };

  const changeCameraTorch = (event: Event) => {
    void runAction(() => setCameraTorch((event.currentTarget as HTMLInputElement).checked));
  };

  const isStreaming = () => desktop?.capture.status === 'streaming';
  const isTransitioning = () => ['initializing', 'starting', 'switching', 'stopping'].includes(desktop?.capture.status ?? '');
  const statusLabel = (status?: string) => (status ?? 'connecting').replace(/_/g, ' ');
  const pageTitle = () => navigation.find((item) => item.id === active)?.label ?? 'Anchor';
  const selectedThread = () => desktop?.smsThreads.find((thread) => thread.threadId === desktop?.selectedSmsThread);
  const filteredSmsThreads = () => {
    const query = smsSearch.trim().toLowerCase();
    return (desktop?.smsThreads ?? []).filter((thread) => !query || `${thread.name} ${thread.snippet}`.toLowerCase().includes(query));
  };
  const formatRefreshRate = (millihertz: number) => `${(millihertz / 1000).toFixed(millihertz % 1000 === 0 ? 0 : 2)} Hz`;
  const captureOption = (output: DesktopState['capture']['outputs'][number]) => ({
    value: String(output.index),
    label: output.name,
    detail: `${output.width} × ${output.height}  ·  ${formatRefreshRate(output.refreshRateMhz)}`,
  });
  const selectedCaptureOption = () => {
    const capture = desktop?.capture;
    if (!capture) return undefined;
    const output = capture.outputs.find((item) => item.index === capture.selectedOutputIndex);
    return output ? captureOption(output) : undefined;
  };
  const formatBytes = (bytes: number) => bytes < 1024 ? `${bytes} B` : bytes < 1048576 ? `${Math.round(bytes / 1024)} KB` : `${(bytes / 1048576).toFixed(1)} MB`;
  const formatTime = (timestamp: number) => timestamp <= 0 ? 'Unknown time' : new Date(timestamp < 100_000_000_000 ? timestamp * 1000 : timestamp).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' });
  const formatDuration = (milliseconds: number) => `${Math.floor(milliseconds / 60000)}:${String(Math.floor(milliseconds / 1000) % 60).padStart(2, '0')}`;
  const artworkUrl = (media: Media) => /^(https?:|data:image\/)/.test(media.artUrl) ? media.artUrl : '';
  const filteredLogs = () => {
    const query = logFilter.trim().toLowerCase();
    return (desktop?.logs ?? []).filter((entry) => !query || `${entry.source} ${entry.level} ${entry.message}`.toLowerCase().includes(query));
  };
</script>

<svelte:head>
  <meta name="description" content="Anchor desktop controls" />
</svelte:head>

<main class="shell">
  <aside class="sidebar" aria-label="Anchor navigation">
    <div class="brand">Anchor</div>
    <nav>
      {#each navigation as item}
        <button class:active={active === item.id} onclick={() => void selectSection(item.id)} aria-current={active === item.id ? 'page' : undefined}>
          {item.label}
        </button>
      {/each}
    </nav>
    <div class="sidebar-footer">v{appVersion ?? '…'}</div>
  </aside>

  <section class="workspace">
    <header class="topbar">
      <h1>{pageTitle()}</h1>
    </header>

    <div class="page">
      {#if active === 'sideboat'}
        <section class="section-block sideboat-panel">
          <div class="section-title">
            <div><h2>Screen extension</h2><p>Stream a Wayland output to your connected phone.</p></div>
            <div class="stream-controls">
              <div class="stream-state" data-state={desktop?.capture.status}><span>Stream</span><strong>{statusLabel(desktop?.capture.status)}</strong></div>
              {#if isTransitioning()}
                <button class="button primary" disabled>Working…</button>
              {:else if isStreaming()}
                <button class="button danger" disabled={busy} onclick={() => void runAction(() => setStreaming(false))}>Stop streaming</button>
              {:else}
                <button class="button primary" disabled={busy || desktop?.capture.status === 'unavailable'} onclick={() => void runAction(() => setStreaming(true))}>Start streaming</button>
              {/if}
            </div>
          </div>
          {#if desktop?.capture.status === 'unavailable'}
            <div class="notice"><strong>Screen capture unavailable</strong><span>{desktop.capture.unavailableReason ?? 'The capture backend could not start.'}</span></div>
          {/if}
          <div class="form-row">
            <div class="field grow"><span>Output</span>
              {#key `${desktop?.capture.selectedOutputIndex ?? -1}:${(desktop?.capture.outputs ?? []).map((output) => `${output.index}:${output.name}`).join('|')}`}
                <Select
                  ariaLabel="Capture output"
                  value={String(desktop?.capture.selectedOutputIndex ?? 0)}
                  disabled={busy || !desktop?.capture.outputs.length}
                  selected={selectedCaptureOption()}
                  options={(desktop?.capture.outputs ?? []).map(captureOption)}
                  onselect={changeOutput}
                />
              {/key}
            </div>
          </div>
          <div class="virtual-display-section">
            <div class="virtual-display-heading">
                <div><strong>Extended display</strong></div>
              <button class="button" disabled={busy} onclick={() => virtualDisplayOpen = !virtualDisplayOpen}>{virtualDisplayOpen ? 'Cancel' : 'Create virtual display'}</button>
            </div>
            {#if virtualDisplayOpen}
              <div class="virtual-display-form">
                <div class="virtual-mode-buttons" role="group" aria-label="Virtual display size">
                  <button class:active={virtualSizeMode === 'phone'} disabled={!phoneDisplay()} onclick={() => virtualSizeMode = 'phone'}>Match phone</button>
                  <button class:active={virtualSizeMode === 'custom'} onclick={() => virtualSizeMode = 'custom'}>Custom size</button>
                </div>
                {#if virtualSizeMode === 'phone'}
                  {#if phoneDisplay()}
                    <div class="virtual-phone-options">
                      <div class="virtual-option-row">
                        <span>Orientation</span>
                        <div class="virtual-orientation-options" role="group" aria-label="Virtual display orientation">
                          <button class:active={virtualOrientation === 'portrait'} onclick={() => virtualOrientation = 'portrait'}>Portrait</button>
                          <button class:active={virtualOrientation === 'landscape'} onclick={() => virtualOrientation = 'landscape'}>Landscape</button>
                        </div>
                      </div>
                      <div class="virtual-option-row">
                        <span>Scale</span>
                        <div class="virtual-scale-options" role="group" aria-label="Virtual display scale">
                          {#each [0.25, 0.5, 1, 2] as scale}
                            <button class:active={virtualScale === scale} onclick={() => virtualScale = scale}>{scale === 1 ? 'Native' : `${scale}×`}</button>
                          {/each}
                        </div>
                      </div>
                    </div>
                  {:else}
                    <p class="setting-note">Reconnect the phone after updating Anchor to use its display dimensions. Custom size is available now.</p>
                  {/if}
                {:else}
                  <div class="virtual-custom-size">
                    <label class="field"><span>Width</span><input type="number" min="64" step="2" bind:value={virtualWidth} /></label>
                    <span>×</span>
                    <label class="field"><span>Height</span><input type="number" min="64" step="2" bind:value={virtualHeight} /></label>
                  </div>
                {/if}
                {#if virtualDimensions()}
                  <div class="virtual-summary"><strong>{virtualDimensions()?.width} × {virtualDimensions()?.height}</strong><span>{virtualLoad()}</span><button class="button primary" disabled={busy} onclick={createVirtualOutput}>Create display</button></div>
                {/if}
              </div>
            {/if}
            {#if desktop?.virtualDisplays.length}
              <div class="virtual-display-list">
                {#each desktop.virtualDisplays as display}
                  <div><span><strong>{display.name}</strong><small>{display.width} × {display.height} · Created by Anchor</small></span><button class="text-button danger-text" disabled={busy} onclick={() => void runAction(() => destroyVirtualDisplay(display.name))}>Destroy</button></div>
                {/each}
              </div>
            {/if}
          </div>
          <div class="screen-preview">
            {#if previewUrl}
              <img src={previewUrl} alt="Preview of the selected display" />
            {:else}
              <div class="preview-empty">Preparing display preview…</div>
            {/if}
          </div>
          {#if desktop?.capture.metrics}
            <dl class="metrics-line">
              <div><dt>Frame rate</dt><dd>{desktop.capture.metrics.fps} fps</dd></div>
              <div><dt>Average frame</dt><dd>{desktop.capture.metrics.avgFrameMs} ms</dd></div>
              <div><dt>Frame size</dt><dd>{formatBytes(desktop.capture.metrics.frameSizeBytes)}</dd></div>
            </dl>
          {/if}
        </section>

      {:else if active === 'messages'}
        <div class="split-view messages-view">
          <section class="list-pane" aria-label="Conversations">
            <div class="list-filter">
              <input aria-label="Filter conversations" placeholder="Search" bind:value={smsSearch} />
              <button class="button subtle danger-text sms-delete" disabled={!desktop?.smsThreads.length} onclick={deleteLocalSmsCopy}>Delete local copy</button>
            </div>
            {#each filteredSmsThreads() as thread}
              <button class="select-row sms-thread-row" class:with-photo={Boolean(smsPhotos.get(thread.threadId))} class:selected={desktop?.selectedSmsThread === thread.threadId} onclick={() => chooseSmsThread(thread.threadId)}>
                {#if smsPhotos.get(thread.threadId)}<img class="contact-photo thread-photo" src={smsPhotos.get(thread.threadId)} alt="" />{/if}
                <span class="thread-copy"><strong>{thread.name}</strong><span>{thread.snippet || 'No message preview'}</span></span>
              </button>
            {:else}
              <p class="empty">{sectionLoading ? 'Loading conversations…' : 'No conversations yet.'}</p>
            {/each}
          </section>
          <section class="detail-pane">
            {#if selectedThread()}
              <div class="conversation-heading">
                {#if smsPhotos.get(selectedThread()!.threadId)}<img class="contact-photo conversation-photo" src={smsPhotos.get(selectedThread()!.threadId)} alt="" />{/if}
                <div><h2>{selectedThread()?.name}</h2><p>SMS conversation</p></div>
              </div>
              <div class="message-list" bind:this={messageList}>
                {#each desktop?.smsMessages ?? [] as message}
                  <article class:sent={message.sent} class="chat-message">
                    <span class="message-author">{message.sent ? 'You' : selectedThread()?.name}</span>
                    <div class="chat-bubble"><p>{message.body || 'Attachment'}</p><time>{formatTime(message.date)}</time></div>
                  </article>
                {:else}<p class="empty">Loading this conversation…</p>{/each}
              </div>
              <form class="compose-row" onsubmit={submitSms}>
                <input aria-label="Message" placeholder={desktop?.smsAvailable ? 'Message' : 'Connect a phone to send messages'} disabled={!desktop?.smsAvailable} bind:value={smsDraft} />
                <button class="button primary" disabled={busy || !desktop?.smsAvailable || !smsDraft.trim()}>Send</button>
              </form>
            {:else}<p class="empty">Select a conversation.</p>{/if}
          </section>
        </div>

      {:else if active === 'foghorn'}
        <section class="notifications-page">
          <div class="page-toolbar"><p>Notifications received from your phone and desktop.</p><button class="button" disabled={!desktop?.notifications.length} onclick={() => void runAction(clearNotifications)}>Clear all</button></div>
          <div class="notification-list">
            {#each desktop?.notifications ?? [] as notification, index}
              <article class:has-icon={Boolean(notificationIcons.get(notification.iconKey))} class="notification-row">
                {#if notificationIcons.get(notification.iconKey)}<div class="notification-icon"><img src={notificationIcons.get(notification.iconKey)} alt="" /></div>{/if}
                <div class="notification-copy">
                  <div class="notification-meta"><strong>{notification.appName || 'Unknown application'}</strong><span>From {notification.source || 'unknown source'}</span><time>{formatTime(notification.timestamp)}</time></div>
                  <h2>{notification.title || 'Notification'}</h2>
                  {#if notification.body}<p>{notification.body}</p>{/if}
                </div>
                <button class="button subtle" onclick={() => void runAction(() => dismissNotification(index))}>Dismiss</button>
              </article>
            {:else}<div class="empty-state"><strong>No notifications</strong><p>New phone and desktop notifications will appear here.</p></div>{/each}
          </div>
        </section>

      {:else if active === 'clipboard'}
        <section class="section-block">
          <div class="section-title"><div><h2>Clipboard sync</h2></div><button class="button" disabled={!desktop?.clipboard.length} onclick={() => void runAction(clearClipboard)}>Clear all</button></div>
          <div class="rows">
            {#each desktop?.clipboard ?? [] as entry}
              <article class="data-row"><div class="row-main"><div class="row-heading"><strong>{entry.contentType === 'image' ? 'Image' : 'Text'}</strong><span>{formatBytes(entry.sizeBytes)} · {formatTime(entry.timestamp)}</span></div><p class="preserve">{entry.preview}</p></div><div class="actions">{#if entry.contentType === 'text'}<button class="button" onclick={() => void runAction(() => copyClipboardEntry(entry.hash))}>{copiedClipboardHash === entry.hash ? 'Copied' : 'Copy'}</button>{/if}<button class="button" title="Send this older clipboard item to the phone again" onclick={() => void runAction(() => sendClipboard(entry.hash))}>Send again</button></div></article>
            {:else}<p class="empty">Copy something on either device to see it here.</p>{/each}
          </div>
        </section>

      {:else if active === 'files'}
        <section class:active={filesDragActive} class="section-block files-panel">
          <div class="section-title"><div><h2>Files</h2><p class="mono">{desktop?.sharedFolder ?? 'Loading shared folder…'}</p></div><div class="actions"><button class="button" onclick={() => void runAction(chooseSharedFolder)}>Change folder</button><button class="button" onclick={() => void runAction(openSharedFolder)}>Open folder</button><button class="button" disabled={!desktop?.transfers.length} onclick={() => void runAction(clearTransfers)}>Clear list</button></div></div>
          <div class="rows">
            {#each desktop?.transfers ?? [] as transfer}
              <article class="data-row"><div class="row-main"><div class="row-heading"><strong>{transfer.name}</strong><span>{transfer.direction} · {formatBytes(transfer.sizeBytes)} · {formatTime(transfer.timestamp)}</span></div><p class="mono">{transfer.path}</p></div><button class="button" disabled={!transfer.exists} onclick={() => void runAction(() => openTransfer(transfer.path))}>Open</button></article>
            {:else}<p class="empty">{filesDragActive ? 'Release to send' : fileDropStatus || 'Drop files here to send to connected device'}</p>{/each}
          </div>
        </section>

      {:else if active === 'commands'}
        <section class="commands-page">
          <div class="page-toolbar"><p>Saved desktop actions available from your phone.</p><button class="button primary" onclick={newCommand}>New command</button></div>
          <div class="command-list">
            {#each desktop?.commands ?? [] as saved}
              <article class="command-row">
                <div class="command-copy">
                  <div class="command-heading"><strong>{saved.name}</strong><span>{saved.detach ? 'Runs detached' : 'Waits for completion'}</span></div>
                  {#if saved.description}<p>{saved.description}</p>{/if}
                  <code>{saved.command}</code>
                </div>
                <div class="command-result" data-status={saved.runStatus}>
                  {#if saved.runStatus}<strong>{saved.runStatus === 'started' ? (saved.detach ? 'Started' : 'Running') : saved.runStatus === 'done' ? 'Completed' : 'Failed'}</strong>{/if}
                  {#if saved.runDetail}<span>{saved.runDetail}</span>{/if}
                </div>
                <div class="command-actions">
                  <button class="button run-button" disabled={saved.runStatus === 'started' && !saved.detach} onclick={() => void runAction(() => runCommand(saved.id))}>{saved.runStatus === 'started' && !saved.detach ? 'Running…' : 'Run'}</button>
                  <button class="button subtle" onclick={() => editCommand(saved)}>Edit</button>
                  <button class="button subtle danger-text" onclick={() => removeCommand(saved)}>Delete</button>
                </div>
              </article>
            {:else}<div class="empty-state"><strong>No commands configured</strong><p>Add a command to run a trusted desktop action from your phone.</p><button class="button primary" onclick={newCommand}>New command</button></div>{/each}
          </div>
        </section>

      {:else if active === 'media'}
        <section class="section-block">
          <div class="section-title"><div><h2>Media</h2><p>Playback on this desktop and the connected phone.</p></div></div>
          <div class="media-grid">
            {#each [['desktop', 'Desktop', desktop?.desktopMedia], ['phone', 'Phone', desktop?.phoneMedia]] as card}
              <article class:has-art={Boolean(card[2] && artworkUrl(card[2] as Media))} class="media-panel">
                {#if card[2]}
                  {@const media = card[2] as Media}
                  {@const art = artworkUrl(media)}
                  {#if art}<img class="media-art-backdrop" src={art} alt="" /><div class="media-art-scrim"></div>{/if}
                  <div class="media-content">
                    <span class="eyebrow">{card[1]}</span>
                    <div class="now-playing" class:with-cover={Boolean(art)}>
                      {#if art}<img class="media-cover" src={art} alt={`Album artwork for ${media.title || 'current track'}`} />{/if}
                      <div class="media-copy"><h3>{media.title || 'Unknown title'}</h3><p>{[media.artist, media.album].filter(Boolean).join(' — ') || media.app}</p></div>
                    </div>
                    {#if media.durationMs > 0}<div class="progress"><span style={`width:${Math.min(100, media.positionMs / media.durationMs * 100)}%`}></span></div><small>{formatDuration(media.positionMs)} / {formatDuration(media.durationMs)}</small>{/if}
                    <div class="transport">
                      <button class="transport-button" aria-label="Previous track" title="Previous" disabled={!media.canPrev} onclick={() => void runAction(() => mediaCommand(card[0] as 'desktop' | 'phone', 'previous'))}><TransportIcon name="previous" /></button>
                      <button class="transport-button primary-transport" aria-label={media.state === 'playing' ? 'Pause' : 'Play'} title={media.state === 'playing' ? 'Pause' : 'Play'} disabled={media.state === 'playing' ? !media.canPause : !media.canPlay} onclick={() => void runAction(() => mediaCommand(card[0] as 'desktop' | 'phone', media.state === 'playing' ? 'pause' : 'play'))}><TransportIcon name={media.state === 'playing' ? 'pause' : 'play'} /></button>
                      <button class="transport-button" aria-label="Next track" title="Next" disabled={!media.canNext} onclick={() => void runAction(() => mediaCommand(card[0] as 'desktop' | 'phone', 'next'))}><TransportIcon name="next" /></button>
                    </div>
                  </div>
                {:else}<div class="media-content"><span class="eyebrow">{card[1]}</span><p class="empty compact">Nothing playing.</p></div>{/if}
              </article>
            {/each}
          </div>
        </section>

      {:else if active === 'settings'}
        <div class="settings-page">
          <section class="settings-section">
            <div class="settings-heading"><div><h2>Paired devices</h2><p>Approve nearby pairing requests from this screen.</p></div><div class="actions"><span>{desktop?.devices.filter((device) => device.online).length ?? 0} connected</span></div></div>
            <div class="device-list">
              {#each desktop?.devices ?? [] as device}
                <article class:connected={device.online} class="device-row">
                  <div class="device-summary">
                    <div class="device-name"><strong>{device.name}</strong><span class:online-text={device.online}>{device.online ? 'Connected' : 'Offline'}</span></div>
                    {#if device.online && device.batteryLevel != null}<span class="device-battery">{device.batteryLevel}% battery{device.batteryCharging ? ' · Charging' : ''}</span>{/if}
                    <div class="device-identity">
                      <span><small>ID</small><code title={device.id}>{device.id.length > 12 ? `${device.id.slice(0, 12)}…` : device.id}</code></span>
                      <span><small>Fingerprint</small><code title={device.fingerprint}>{device.fingerprint.length > 23 ? `${device.fingerprint.slice(0, 23)}…` : device.fingerprint}</code></span>
                    </div>
                  </div>
                  <button class="button subtle danger-text" onclick={() => void runAction(() => unpairDevice(device.id))}>Unpair</button>
                </article>
              {:else}<div class="empty-state"><strong>No paired devices</strong><p>Connect a device to begin pairing.</p></div>{/each}
            </div>
          </section>

          {#if desktop}
            <section class="settings-section">
              <div class="settings-heading"><div><h2>Phone As Camera</h2></div><span class:online-text={desktop.camera.status === 'ready'}>{desktop.camera.status}</span></div>
              <div class="settings-columns">
                <article class="settings-column">
                  <div class="column-heading"><strong>Setup</strong><span>v4l2loopback</span></div>
                  <div class="setting-stack">
                    <div class="setting-copy"><strong>Virtual camera</strong><p>{desktop.camera.device ?? 'No camera device available'}</p></div>
                    {#if desktop.camera.status !== 'ready'}<button class="button" onclick={() => void runAction(() => setupCamera(desktop?.camera.status === 'device unavailable'))}>{desktop.camera.status === 'device unavailable' ? 'Retry camera' : 'Create camera'}</button>{/if}
                    <label class="setting-check"><input type="checkbox" checked={desktop.camera.autoLoad} onchange={changeCameraAutoLoad} /><span><strong>Enable on startup</strong><small>May request administrator permission.</small></span></label>
                    <div class="field"><span>Camera source</span><Select ariaLabel="Camera source" value={desktop.camera.selectedDeviceId ?? ''} options={[{ value: '', label: 'Automatic' }, ...desktop.devices.filter((device) => device.cameraCapable).map((device) => ({ value: device.id, label: device.name }))]} onselect={changeCameraDevice} /></div>
                    {#if !desktop.devices.some((device) => device.cameraCapable)}<p class="setting-note">No camera-capable phone is connected.</p>{/if}
                  </div>
                </article>

                <article class="settings-column">
                  <div class="column-heading"><strong>Live controls</strong></div>
                  <div class="setting-stack">
                    <label class="range-field" for="camera-zoom"><span><strong>Zoom</strong><output>{desktop.camera.zoomRatio.toFixed(1)}×</output></span><input id="camera-zoom" type="range" min="1" max="10" step="0.1" value={desktop.camera.zoomRatio} onchange={changeCameraZoom} /></label>
                    <label class="range-field" for="camera-exposure"><span><strong>Exposure</strong><output>{desktop.camera.exposure} EV</output></span><input id="camera-exposure" type="range" min="-6" max="6" step="1" value={desktop.camera.exposure} onchange={changeCameraExposure} /></label>
                    <label class="setting-check"><input type="checkbox" checked={desktop.camera.torchOn} onchange={changeCameraTorch} /><span><strong>Torch</strong></span></label>
                    <div class="actions"><button class="button" onclick={() => void runAction(switchCamera)}>Switch camera</button><button class="button subtle" onclick={() => void runAction(() => setCameraZoom(1))}>Reset zoom</button></div>
                  </div>
                </article>

                <article class="settings-column">
                  <div class="column-heading"><strong>Stream defaults</strong></div>
                  <div class="setting-stack">
                    <div class="field"><span>Frame rate</span><Select ariaLabel="Camera frame rate" value={String(desktop.camera.fps)} options={[15, 24, 30, 48, 60].map((fps) => ({ value: String(fps), label: `${fps} fps` }))} onselect={changeCameraFps} /></div>
                    <label class="range-field" for="camera-bitrate"><span><strong>Bitrate</strong><output>{(desktop.camera.bitrateKbps / 1000).toFixed(1)} Mbps</output></span><input id="camera-bitrate" type="range" min="500" max="8000" step="100" value={desktop.camera.bitrateKbps} onchange={changeCameraBitrate} /></label>
                  </div>
                </article>
              </div>
            </section>
          {/if}

          <section class="settings-section about-section">
            <div class="settings-heading"><div><h2>About</h2></div></div>
            <dl class="about-grid">
              <div><dt>Version</dt><dd>{desktop?.version}</dd></div>
              <div><dt>Build</dt><dd class="mono">{desktop?.build}</dd></div>
              <div class="environment-cell">
                <dt>Compositor</dt>
                <dd class="environment-value">
                  <span>{desktop?.compositor}</span>
                  {#if desktop}
                    <details class="capabilities-menu">
                      <summary>Capabilities</summary>
                      <div class="capabilities-popover">
                        <strong>Environment support</strong>
                        <ul>
                          {#each [
                            ['Screen sharing', 'Mirror or extend a display.', desktop.environment.screenSharing],
                            ['Remote pointer', 'Control the cursor from another device.', desktop.environment.pointerInput],
                            ['Remote keyboard', 'Type from another device.', desktop.environment.keyboardInput],
                            ['Virtual displays', 'Create and remove a private display.', desktop.environment.virtualDisplays]
                          ] as capability}
                            <li>
                              <span><b>{capability[0]}</b><small>{capability[1]}</small></span>
                              <em data-state={capability[2] == null ? 'checking' : capability[2] ? 'supported' : 'unavailable'}>{capability[2] == null ? 'Checking' : capability[2] ? 'Supported' : 'Unavailable'}</em>
                            </li>
                          {/each}
                        </ul>
                      </div>
                    </details>
                  {/if}
                </dd>
              </div>
            </dl>
            <div class="settings-heading support-heading">
              <div>
                <h3>Support &amp; feedback</h3>
                <p>Email <a href="mailto:devs@anchor-software.org?subject=Anchor%20desktop%20feedback">devs@anchor-software.org</a> with your version, compositor, and a description of the problem. Do not include private messages, certificates, or credentials.</p>
              </div>
            </div>
          </section>
        </div>

      {:else if active === 'logs'}
        <section class="section-block logs-block">
          <div class="section-title"><div><h2>Logs</h2></div><div class="actions"><input class="filter-input" placeholder="Filter logs" bind:value={logFilter} /><button class="button" disabled={!desktop?.logs.length} onclick={() => void runAction(clearLogs)}>Clear</button></div></div>
          <div class="log-list" bind:this={logList}>
            {#each filteredLogs() as entry}<div class="log-row"><time>{formatTime(entry.timestamp)}</time><span>{entry.source}</span><strong data-level={entry.level.toLowerCase()}>{entry.level}</strong><code>{entry.message}</code></div>{:else}<p class="empty">No matching logs.</p>{/each}
          </div>
        </section>
      {/if}
    </div>
  </section>
</main>

{#if commandEditorOpen}
  <div class="modal-backdrop" role="presentation">
    <dialog class="command-dialog" open aria-labelledby="command-dialog-title">
      <div class="dialog-heading">
        <div><h2 id="command-dialog-title">{editingCommandId ? 'Edit command' : 'New command'}</h2><p>Commands run locally using the desktop shell.</p></div>
      </div>
      <form onsubmit={submitCommand}>
        <div class="editor-fields">
          <label class="field"><span>Name</span><input bind:value={commandName} placeholder="Lock screen" /></label>
          <label class="field"><span>Command</span><input class="mono" bind:value={commandLine} placeholder="loginctl lock-session" /></label>
          <label class="field"><span>Description</span><input bind:value={commandDescription} placeholder="Optional" /></label>
          <label class="check-field"><input type="checkbox" bind:checked={commandDetach} /> Run detached</label>
        </div>
        <div class="dialog-actions"><button type="button" class="button" onclick={resetCommandForm}>Cancel</button><button class="button primary" disabled={busy || !commandName.trim() || !commandLine.trim()}>{editingCommandId ? 'Save changes' : 'Add command'}</button></div>
      </form>
    </dialog>
  </div>
{/if}

{#if desktop?.pairingRequest}
  <div class="modal-backdrop" role="presentation">
    <dialog class="pairing-dialog" open aria-labelledby="pairing-title">
      <span class="eyebrow">Pairing request</span><h2 id="pairing-title">Trust {desktop.pairingRequest.deviceName}?</h2>
      <dl><div><dt>Type</dt><dd>{desktop.pairingRequest.deviceType}</dd></div><div><dt>Safety number</dt><dd class="mono">{desktop.pairingRequest.safetyNumber}</dd></div><div><dt>Fingerprint</dt><dd class="mono">{desktop.pairingRequest.fingerprint}</dd></div></dl>
      <p class="setting-note">Accept only if the phone shows the same safety number.</p>
      <div class="dialog-actions"><button class="button" onclick={() => void runAction(() => respondToPairing(false))}>Reject</button><button class="button primary" onclick={() => void runAction(() => respondToPairing(true))}>Accept</button></div>
    </dialog>
  </div>
{/if}

{#if actionError}<div class="toast" role="status"><span>{actionError}</span><button onclick={() => actionError = ''}>Dismiss</button></div>{/if}
