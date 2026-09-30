import {
  AbilityConstant,
  Configuration,
  UIAbility,
  Want,
  wantAgent,
  WantAgent,
} from "@kit.AbilityKit";
import { BusinessError } from "@kit.BasicServicesKit";
import { i18n } from "@kit.LocalizationKit";
import { notificationManager } from "@kit.NotificationKit";
import common from "@ohos.app.ability.common";
import window from "@ohos.window";
import * as Entry from "../components/MainPage";
import { BridgeHostRegistry } from "../bridge/BridgeHost";
import { NativeModuleLoader } from "../runtime/NativeModuleLoader";
import { AbilityStateCodec } from "../runtime/AbilityStateCodec";
import { SerialTaskQueue } from "../runtime/SerialTaskQueue";
import {
  AbilityInitContext,
  ApplicationLifecycle,
  BridgeBindings,
  BridgeAbilityCreateLifecyclePayload,
  BridgeEmptyLifecyclePayload,
  BridgeLifecycleEvent,
  BridgeMemoryLevelLifecyclePayload,
  BridgePluginFactory,
  BridgeWindowStageEventLifecyclePayload,
  Module,
  NativeNotificationAction,
  NativeNotificationRequest,
  NativeNotificationResponse,
} from "./type";

export const STATE_KEY = "ohos.rs.ability.application.state";

/**
 * Want parameter carrying the tag of the activated notification back into the Ability. The name is
 * namespaced so it never collides with the launch parameters other senders use.
 */
const NOTIFICATION_TAG_PARAMETER: string = "ohos.rs.ability.notification.tag";
/** Want parameter carrying the id of the pressed notification button; absent for the body. */
const NOTIFICATION_ACTION_ID_PARAMETER: string = "ohos.rs.ability.notification.actionId";

/** Separator of the POSIX `LANGUAGE` list the preferred languages are handed over as. */
const LOCALE_LIST_SEPARATOR: string = ":";

interface LoadedNativeModule {
  moduleName: string;
  module: Module;
}

interface NativeModuleRuntime extends LoadedNativeModule {
  lifecycle: ApplicationLifecycle;
  bridgeOwner: string;
}

export class NativeAbility extends UIAbility {
  public moduleName: string | string[] = "";
  public defaultPage: boolean = true;
  public loadMode: "async" | "sync" = "async";
  /** Application-provided capability plugins. NativeAbility itself has no concrete plugin imports. */
  public bridgePlugins: BridgePluginFactory[] = [];
  /** Whether the window is being held awake on behalf of a running agent. */
  private agentKeepsScreenOn: boolean = false;
  /** Last notification id handed out; distinct tags never share an id. */
  private lastNotificationId: number = 0;
  /** Notification ids keyed by the tag the native module uses. */
  private notificationIds: Map<string, number> = new Map<string, number>();
  /** Activations received before the native modules finished initialising. */
  private pendingNotificationResponses: NativeNotificationResponse[] = [];
  /** Request code handed to the last notification wantAgent; the system needs it unique. */
  private lastWantAgentRequestCode: number = 0;

  private moduleRuntimes: NativeModuleRuntime[] = [];
  private bridgeSessionId: string = "";
  private readonly lifecycleQueue = new SerialTaskQueue((label: string, error: Error): void => {
    console.error(`[NativeAbility] lifecycle operation '${label}' failed: ${String(error)}`);
  });
  private acceptingLifecycle: boolean = false;
  private abilityGeneration: number = 0;
  private windowStageGeneration: number = 0;
  private windowStageActive: boolean = false;
  /**
   * When non-null, `initializeSession` waits for `releaseNativeLoad()` before it loads a native
   * module. Subclasses that must show UI before libhicodeer loads call `holdNativeLoad()` from
   * their `onCreate`. Default null: no behavior change for hosts that do not opt in.
   */
  private nativeLoadGate: Promise<void> | null = null;
  private nativeLoadRelease: (() => void) | null = null;
  /** Resolved home directory (`<picked root>/HiCodeer`) for this session, passed to Rust. */
  protected homeDirectory: string = "";
  private observedWindowStage?: window.WindowStage;
  private observedWindow?: window.Window;

  private readonly onWindowStageEvent = (event: window.WindowStageEventType): void => {
    if (!this.acceptingLifecycle || this.observedWindowStage === undefined) {
      return;
    }
    void this.enqueueLifecycleOperation("window-stage-event", async (): Promise<void> => {
      this.forEachLifecycle((lifecycle) =>
        lifecycle.windowStageEventCallback.onWindowStageEvent(event),
      );
      await this.notifyBridgeLifecycle({
        kind: "window-stage-event",
        payload: new BridgeWindowStageEventLifecyclePayload(event),
      });
    });
  };

  private readonly onWindowVisibilityChange = (visible: boolean): void => {
    if (!this.acceptingLifecycle) {
      return;
    }
    this.forEachLifecycle((lifecycle) =>
      lifecycle.windowStageEventCallback.onWindowVisibilityChange(visible),
    );
  };

  protected resolveModuleNames(): string[] {
    const moduleNames = NativeModuleLoader.resolveModuleNames(this.moduleName);
    if (moduleNames.length === 0) {
      throw new Error("moduleName must contain at least one module");
    }
    return moduleNames;
  }

  protected forEachLifecycle(handler: (lifecycle: ApplicationLifecycle) => void): void {
    for (const runtime of this.moduleRuntimes) {
      try {
        handler(runtime.lifecycle);
      } catch {}
    }
  }

  /**
   * Adds framework or subclass work to the one Ability-scoped lifecycle queue. OpenHarmony does
   * not await onCreate/onWindowStageCreate/onWindowStageDestroy, so those callbacks only capture
   * their arguments synchronously; this queue is the actual ordering barrier.
   */
  protected enqueueLifecycleOperation(
    label: string,
    operation: () => Promise<void>,
  ): Promise<void> {
    void this.lifecycleQueue.enqueue(label, operation);
    return this.lifecycleQueue.settled();
  }

  /** Delays the native module load until `releaseNativeLoad()`. Idempotent. */
  protected holdNativeLoad(): void {
    if (this.nativeLoadGate === null) {
      this.nativeLoadGate = new Promise<void>((resolve: () => void): void => {
        this.nativeLoadRelease = resolve;
      });
    }
  }

  /** Releases a pending `holdNativeLoad()`. Idempotent. */
  protected releaseNativeLoad(): void {
    const release = this.nativeLoadRelease;
    this.nativeLoadRelease = null;
    if (release !== null) {
      release();
    }
  }

  protected parseSavedStateMap(rawState: string): Record<string, string> {
    return AbilityStateCodec.decode(rawState);
  }

  protected serializeSavedStateMap(stateMap: Record<string, string>): string {
    return AbilityStateCodec.encode(stateMap);
  }

  /**
   * Reads the system's preferred languages as the POSIX `LANGUAGE` list the native side expects:
   * BCP 47 tags separated by `:`, most preferred first.
   *
   * Every language is expanded into its fallback candidates (see `pushLocaleFallbacks`): the native
   * side matches each candidate against its own locale table by exact id/alias equality, and a
   * language that carries a script subtag - OHOS reports `zh-Hans-CN` rather than `zh-CN` - never
   * equals a table entry on its own. `Configuration.language` stays as the last fallback, since it
   * still carries the system's current language.
   */
  private static resolvePreferredLocales(context: common.UIAbilityContext | undefined): string {
    const locales: string[] = [];
    try {
      for (const language of i18n.System.getPreferredLanguageList()) {
        NativeAbility.pushLocaleFallbacks(language, locales);
      }
    } catch (error) {
      console.warn(`[NativeAbility] unable to read the preferred language list: ${String(error)}`);
    }
    if (locales.length === 0) {
      try {
        NativeAbility.pushLocaleFallbacks(i18n.System.getSystemLocale(), locales);
      } catch (error) {
        console.warn(`[NativeAbility] unable to read the system locale: ${String(error)}`);
      }
    }
    if (locales.length === 0) {
      NativeAbility.pushLocaleFallbacks(context?.config?.language ?? "", locales);
    }
    return locales.join(LOCALE_LIST_SEPARATOR);
  }

  /**
   * Appends `locale` and each of its shorter subtag prefixes, in order, skipping duplicates:
   * `zh-Hans-CN` becomes `zh-Hans-CN`, `zh-Hans`, `zh`. The native side reads the list as an
   * ordered preference list and picks the first entry it recognizes, so the more specific
   * candidates have to come first.
   */
  private static pushLocaleFallbacks(locale: string, out: string[]): void {
    let tag = locale.trim();
    while (tag.length > 0) {
      if (!out.includes(tag)) {
        out.push(tag);
      }
      const separator = tag.lastIndexOf("-");
      if (separator < 0) {
        break;
      }
      tag = tag.substring(0, separator);
    }
  }

  protected createInitContext(moduleName: string): AbilityInitContext {
    const context = this.context as common.UIAbilityContext;
    return {
      basePath: context?.filesDir ?? "",
      prefPath: context?.filesDir ?? "",
      preferredLocales: NativeAbility.resolvePreferredLocales(context),
      colorMode: context?.config?.colorMode ?? -1,
      moduleName,
      homeDirectory: this.homeDirectory,
      resourceManager: context?.resourceManager,
    };
  }

  /**
   * Loads application content inside the framework's WindowStage lifecycle transaction. Override
   * this instead of starting detached Promise work from the platform callback.
   */
  protected async loadWindowStageContent(windowStage: window.WindowStage): Promise<void> {
    if (this.defaultPage) {
      try {
        await windowStage.loadContentByName(Entry.RouteName);
      } catch (error) {
        throw new Error(`Unable to load default WindowStage content: ${String(error)}`);
      }
    }
  }

  private updateAppStorage<T>(key: string, value: T): void {
    AppStorage.setOrCreate(key, value);
    AppStorage.set(key, value);
  }

  /**
   * Wires both scoped ArkTS -> Rust ports immediately after module.init. The lifecycle sink lets
   * BridgeHost serialize UI readiness with ArkTS plugin installation instead of relying on
   * Promise microtask timing in DefaultXComponent.
   */
  private attachBridgeSinks(sessionId: string, moduleName: string, module: Module): void {
    const mainThreadSink =
      typeof module.onBridgeSyncEvent === "function"
        ? (
            pluginId: string,
            event: string,
            requestTypeName: string,
            responseTypeName: string,
            value: ESObject,
          ): ESObject =>
            module.onBridgeSyncEvent!(pluginId, event, requestTypeName, responseTypeName, value)
        : undefined;
    const lifecycleSink =
      typeof module.onBridgeLifecycle === "function"
        ? (kind: string): void => module.onBridgeLifecycle!(kind)
        : undefined;
    BridgeHostRegistry.attachEventSink(sessionId, moduleName, mainThreadSink, lifecycleSink);
  }

  /**
   * Hands the ability host's main-window actions to the native module. The closures read the
   * observed main window lazily: init runs before `onWindowStageCreate`, so the window does not
   * exist yet when they are registered.
   */
  private attachWindowActions(module: Module): void {
    if (typeof module.setWindowActions !== "function") {
      console.warn("[NativeAbility] the native module exports no setWindowActions port");
      return;
    }
    module.setWindowActions!(
      (): void => {
        this.minimizeMainWindow();
      },
      (): void => {
        this.restoreMainWindow();
      },
    );
  }

  private minimizeMainWindow(): void {
    const win = this.observedWindow;
    if (win === undefined) {
      console.warn("[NativeAbility] minimize requested before the main window was observed");
      return;
    }
    win.minimize().catch((error: BusinessError): void => {
      console.error(`[NativeAbility] unable to minimize the main window: ${String(error)}`);
    });
  }

  private restoreMainWindow(): void {
    const win = this.observedWindow;
    if (win === undefined) {
      console.warn("[NativeAbility] restore requested before the main window was observed");
      return;
    }
    win.restore().catch((error: BusinessError): void => {
      console.error(`[NativeAbility] unable to restore the main window: ${String(error)}`);
    });
  }

  /**
   * Hands the ability host's own lifecycle actions to the native module. Finishing an ability is
   * ArkTS-only, so the closure calls back into this host rather than into a platform object.
   */
  private attachAbilityActions(module: Module): void {
    if (typeof module.setAbilityActions !== "function") {
      console.warn("[NativeAbility] the native module exports no setAbilityActions port");
      return;
    }
    module.setAbilityActions!((): void => {
      this.terminateAbility();
    });
  }

  private terminateAbility(): void {
    const context = this.context as common.UIAbilityContext;
    context.terminateSelf().catch((error: BusinessError): void => {
      console.error(`[NativeAbility] unable to terminate the ability: ${String(error)}`);
    });
  }

  /**
   * Hands the ability host's idle-sleep-prevention actions to the native module. Holding the
   * screen awake is ArkTS-only, so the host supplies its own closures instead.
   */
  private attachRunningLockActions(module: Module): void {
    if (typeof module.setRunningLockActions !== "function") {
      console.warn("[NativeAbility] the native module exports no setRunningLockActions port");
      return;
    }
    module.setRunningLockActions!(
      (reason: string): void => {
        this.applyKeepScreenOn(true, reason);
      },
      (): void => {
        this.applyKeepScreenOn(false);
      },
    );
  }

  /**
   * Holds the screen awake while an agent thread is running.
   *
   * The OHOS running-lock API cannot serve this: `RunningLockType.BACKGROUND_USER_IDLE` is
   * reported unsupported on this device (`runningLock.isSupported` returns false), and the other
   * types are either deprecated or tied to the proximity sensor. `Window.setWindowKeepScreenOn`
   * is the supported way for an application to hold the screen awake, and it also prevents the
   * screen lock that would otherwise follow an idle screen-off.
   */
  private applyKeepScreenOn(enabled: boolean, reason?: string): void {
    if (enabled === this.agentKeepsScreenOn) {
      return;
    }
    const currentWindow = this.observedWindow;
    if (currentWindow === undefined) {
      console.warn(
        `[NativeAbility] no window to ${enabled ? "keep the screen on" : "restore the screen timeout"} (reason: ${reason ?? ""})`,
      );
      return;
    }
    this.agentKeepsScreenOn = enabled;
    currentWindow.setWindowKeepScreenOn(enabled).catch((error: BusinessError): void => {
      this.agentKeepsScreenOn = !enabled;
      console.error(
        `[NativeAbility] unable to ${enabled ? "keep the screen on" : "restore the screen timeout"}: ${String(error)}`,
      );
    });
  }

  /**
   * Hands the ability host's notification actions to the native module. Publishing and removing a
   * notification is ArkTS-only, so the host supplies its own closures instead.
   */
  private attachNotificationManagerActions(module: Module): void {
    if (typeof module.setNotificationManagerActions !== "function") {
      console.warn("[NativeAbility] the native module exports no setNotificationManagerActions port");
      return;
    }
    module.setNotificationManagerActions!(
      (request: NativeNotificationRequest): void => {
        this.publishNotification(request).catch((error: BusinessError): void => {
          console.error(`[NativeAbility] unable to publish a notification: ${String(error)}`);
        });
      },
      (tag: string): void => {
        this.cancelNotification(tag);
      },
    );
  }

  private async publishNotification(request: NativeNotificationRequest): Promise<void> {
    const known = this.notificationIds.get(request.tag);
    // Reposting under a known tag reuses its id so the system replaces the shown notification
    // instead of stacking a second one.
    const id: number = known ?? ++this.lastNotificationId;
    this.notificationIds.set(request.tag, id);
    const content: notificationManager.NotificationContent = {
      notificationContentType: notificationManager.ContentType.NOTIFICATION_CONTENT_BASIC_TEXT,
      normal: {
        title: request.title,
        text: request.body,
      },
    };
    const published: notificationManager.NotificationRequest = {
      id: id,
      label: request.tag,
      content: content,
      wantAgent: await this.createNotificationWantAgent(request.tag, null),
    };
    if (request.actions.length > 0) {
      const buttons: notificationManager.NotificationActionButton[] = [];
      for (const action of request.actions) {
        buttons.push({
          title: action.title,
          wantAgent: await this.createNotificationWantAgent(request.tag, action.id),
        });
      }
      published.actionButtons = buttons;
    }
    await notificationManager.publish(published);
  }

  /**
   * Builds a wantAgent that relaunches this Ability with the activated notification's tag and, for
   * a button, its id. The Ability reads them back from the launch want.
   */
  private async createNotificationWantAgent(
    tag: string,
    actionId: string | null,
  ): Promise<WantAgent> {
    const context = this.context as common.UIAbilityContext;
    const capability = context.abilityInfo;
    const parameters: Record<string, Object> = {};
    parameters[NOTIFICATION_TAG_PARAMETER] = tag;
    if (actionId !== null) {
      parameters[NOTIFICATION_ACTION_ID_PARAMETER] = actionId;
    }
    const want: Want = {
      bundleName: capability.bundleName,
      abilityName: capability.name,
      moduleName: capability.moduleName,
      parameters: parameters,
    };
    const info: wantAgent.WantAgentInfo = {
      wants: [want],
      actionType: wantAgent.OperationType.START_ABILITY,
      requestCode: ++this.lastWantAgentRequestCode,
    };
    return await wantAgent.getWantAgent(info);
  }

  /**
   * Delivers an activation carried by a launch want. Before the first session initialises there is
   * no native module to hand it to, so it waits for `flushNotificationResponses` instead.
   */
  private handleNotificationWant(want: Want): void {
    const response = NativeAbility.parseNotificationResponse(want);
    if (response === null) {
      return;
    }
    if (this.moduleRuntimes.length === 0) {
      this.pendingNotificationResponses.push(response);
      return;
    }
    this.deliverNotificationResponse(response);
  }

  /** Hands one activation to every native module that exports the response port. */
  private deliverNotificationResponse(response: NativeNotificationResponse): void {
    for (const runtime of this.moduleRuntimes) {
      if (typeof runtime.module.dispatchNotificationResponse !== "function") {
        continue;
      }
      try {
        runtime.module.dispatchNotificationResponse!(response);
      } catch (error) {
        console.error(
          `[NativeAbility] unable to deliver a notification response to ${runtime.moduleName}: ` +
            `${String(error)}`,
        );
      }
    }
  }

  /** Replays activations that arrived before the native modules were ready. */
  private flushNotificationResponses(): void {
    const pending = this.pendingNotificationResponses;
    if (pending.length === 0) {
      return;
    }
    this.pendingNotificationResponses = [];
    for (const response of pending) {
      this.deliverNotificationResponse(response);
    }
  }

  /** Reads an activation out of a launch want, or null when the want carries none. */
  private static parseNotificationResponse(want: Want): NativeNotificationResponse | null {
    const parameters = want.parameters;
    if (parameters === undefined) {
      return null;
    }
    const tagValue = parameters[NOTIFICATION_TAG_PARAMETER];
    if (tagValue === undefined) {
      return null;
    }
    const actionValue = parameters[NOTIFICATION_ACTION_ID_PARAMETER];
    return {
      tag: tagValue.toString(),
      actionId: actionValue === undefined ? null : actionValue.toString(),
    };
  }

  private cancelNotification(tag: string): void {
    const id = this.notificationIds.get(tag);
    if (id === undefined) {
      return;
    }
    this.notificationIds.delete(tag);
    notificationManager.cancel(id, tag).catch((error: BusinessError): void => {
      console.error(`[NativeAbility] unable to cancel a notification: ${String(error)}`);
    });
  }

  /**
   * Creates the Ability-session transport for one native module. It deliberately lives here,
   * not in DefaultXComponent: ability-only plugins must work before a render surface appears and
   * must survive a component disappear/reappear cycle within the same Ability.
   */
  private createBridgeBindings(sessionId: string, moduleName: string): BridgeBindings {
    return {
      bridgeInvoke: async (
        pluginId: string,
        action: string,
        requestTypeName: string,
        responseTypeName: string,
        value: ESObject,
        timeoutMs: number,
      ): Promise<ESObject> => {
        return await BridgeHostRegistry.invokeAsync(
          sessionId,
          moduleName,
          pluginId,
          action,
          requestTypeName,
          responseTypeName,
          value,
          timeoutMs,
        );
      },
      bridgeInvokeSync: (
        pluginId: string,
        action: string,
        requestTypeName: string,
        responseTypeName: string,
        value: ESObject,
      ): ESObject => {
        return BridgeHostRegistry.invokeSync(
          sessionId,
          moduleName,
          pluginId,
          action,
          requestTypeName,
          responseTypeName,
          value,
        );
      },
      // MainThreadScheduler runs the Rust closure inside this TSFN callback before invoking this
      // no-op function. It intentionally carries no component or capability state.
      bridgeDispatch: (): void => {},
    };
  }

  private releaseModuleBridges(runtimes: NativeModuleRuntime[]): void {
    for (const runtime of runtimes.slice().reverse()) {
      try {
        runtime.module.disposeBridge(runtime.bridgeOwner);
      } catch (error) {
        console.error(
          `[NativeAbility] failed to release bridge for ${runtime.moduleName}: ${String(error)}`,
        );
      }
    }
  }

  protected async notifyBridgeLifecycle(event: BridgeLifecycleEvent): Promise<void> {
    const sessionId = this.bridgeSessionId;
    if (!sessionId) {
      return;
    }
    const deliveries: Promise<void>[] = [];
    for (const runtime of this.moduleRuntimes) {
      deliveries.push(this.notifyModuleBridgeLifecycle(sessionId, runtime.moduleName, event));
    }
    await Promise.all(deliveries);
  }

  private async notifyModuleBridgeLifecycle(
    sessionId: string,
    moduleName: string,
    event: BridgeLifecycleEvent,
  ): Promise<void> {
    try {
      await BridgeHostRegistry.emitLifecycle(sessionId, moduleName, event);
    } catch (error) {
      console.error(
        `[NativeAbility] bridge lifecycle '${event.kind}' failed for ${moduleName}: ${String(error)}`,
      );
    }
  }

  private async initializeSession(
    requestedModules: string[],
    restoredStateMap: Record<string, string>,
    fallbackState: string,
    generation: number,
  ): Promise<void> {
    const previousSessionId = this.bridgeSessionId;
    if (previousSessionId) {
      BridgeHostRegistry.beginClosing(previousSessionId);
      try {
        await this.destroyWindowStageIfActive();
        await this.notifyBridgeLifecycle({
          kind: "ability-destroy",
          payload: new BridgeEmptyLifecyclePayload(),
        });
        this.forEachLifecycle((lifecycle) => lifecycle.windowStageEventCallback.onAbilityDestroy());
      } finally {
        // A duplicated platform create is not expected, but treating replacement as a complete
        // teardown prevents an old component root from surviving into the new bridge session.
        for (const runtime of this.moduleRuntimes) {
          try {
            runtime.module.disposeAllRenders();
          } catch {}
        }
        this.releaseModuleBridges(this.moduleRuntimes);
        await BridgeHostRegistry.dispose(previousSessionId);
      }
    }
    this.assertInitializationActive(generation);
    this.bridgeSessionId = "";
    this.moduleRuntimes = [];
    this.windowStageActive = false;
    this.updateAppStorage("bridgeSessionId", "");

    // Prepare hosts without activating `ability` plugins. Their onInstall hooks are allowed to
    // call Rust synchronously, so module.init and both sinks must exist first.
    const sessionId = BridgeHostRegistry.prepare(
      requestedModules,
      this.context as common.UIAbilityContext,
      this.bridgePlugins,
    );
    this.bridgeSessionId = sessionId;

    const loadedModules: LoadedNativeModule[] = [];
    const initializedRuntimes: NativeModuleRuntime[] = [];
    try {
      // Loading is transactional. Parallel arrays and partial "continue after error" states made
      // a module name point at the wrong lifecycle object after one dynamic import failed.
      for (const moduleName of requestedModules) {
        const module = await NativeModuleLoader.load(moduleName, this.loadMode);
        this.assertInitializationActive(generation);
        loadedModules.push({ moduleName, module });
      }

      for (const loaded of loadedModules) {
        const bridgeOwner = `${sessionId}:${loaded.moduleName}`;
        const lifecycle = loaded.module.init(
          this.createBridgeBindings(sessionId, loaded.moduleName),
          bridgeOwner,
          this.createInitContext(loaded.moduleName),
        );
        const runtime: NativeModuleRuntime = {
          moduleName: loaded.moduleName,
          module: loaded.module,
          lifecycle,
          bridgeOwner,
        };
        initializedRuntimes.push(runtime);
        BridgeHostRegistry.configurePlugins(sessionId, loaded.moduleName, lifecycle.bridgePlugins);
        BridgeHostRegistry.attachApplicationLifecycle(sessionId, loaded.moduleName, lifecycle);
        this.attachBridgeSinks(sessionId, loaded.moduleName, loaded.module);
        this.attachWindowActions(loaded.module);
        this.attachAbilityActions(loaded.module);
        this.attachRunningLockActions(loaded.module);
        this.attachNotificationManagerActions(loaded.module);

        const restoredState = restoredStateMap[loaded.moduleName] ?? fallbackState;
        try {
          lifecycle.windowStageEventCallback.onAbilityCreate(restoredState);
        } catch (error) {
          console.error(
            `[NativeAbility] Rust ability-create failed for ${loaded.moduleName}: ${String(error)}`,
          );
        }
      }

      this.moduleRuntimes = initializedRuntimes;
      this.flushNotificationResponses();
      const abilityActivations: Promise<void>[] = [];
      for (const runtime of initializedRuntimes) {
        abilityActivations.push(
          BridgeHostRegistry.activateAbility(sessionId, runtime.moduleName, {
            kind: "ability-create",
            payload: new BridgeAbilityCreateLifecyclePayload(
              restoredStateMap[runtime.moduleName] ?? fallbackState,
            ),
          }),
        );
      }
      await Promise.all(abilityActivations);
      this.assertInitializationActive(generation);
      // Publish only a fully initialized session. DefaultXComponent must never observe a host
      // whose native module has not attached its bridge endpoints and lifecycle sinks yet.
      this.updateAppStorage("bridgeSessionId", sessionId);
    } catch (error) {
      // Promise.all may reject while another module's install hook is still active. Close every
      // host first so no late hook can enter Rust while its bridge is being released below.
      BridgeHostRegistry.beginClosing(sessionId);
      for (const runtime of initializedRuntimes.slice().reverse()) {
        try {
          runtime.lifecycle.windowStageEventCallback.onAbilityDestroy();
        } catch {}
      }
      this.releaseModuleBridges(initializedRuntimes);
      await BridgeHostRegistry.dispose(sessionId);
      if (this.bridgeSessionId === sessionId) {
        this.bridgeSessionId = "";
        this.moduleRuntimes = [];
        this.updateAppStorage("bridgeSessionId", "");
      }
      throw new Error(`Unable to initialize native Ability session: ${String(error)}`);
    }
  }

  private assertInitializationActive(generation: number): void {
    if (!this.acceptingLifecycle || generation !== this.abilityGeneration) {
      throw new Error("Native Ability session initialization was cancelled by lifecycle teardown");
    }
  }

  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    this.handleNotificationWant(want);
    const isRestore: boolean =
      (want.parameters?.["ohos.ability.params.abilityRecoveryRestart"] as boolean) ?? false;
    const savedState = want.parameters?.[STATE_KEY];
    const state = isRestore && savedState !== undefined ? savedState.toString() : "";
    const requestedModules = this.resolveModuleNames();
    // A single module owns its state string verbatim, even if business data happens to begin with
    // the framework's multi-module prefix. Only multi-module sessions interpret the state map.
    let restoredStateMap: Record<string, string> = {};
    if (requestedModules.length > 1) {
      restoredStateMap = this.parseSavedStateMap(state);
    }

    this.acceptingLifecycle = true;
    const generation = ++this.abilityGeneration;
    this.updateAppStorage("moduleName", this.moduleName);
    this.updateAppStorage("loadMode", this.loadMode);
    void this.enqueueLifecycleOperation("ability-create", async (): Promise<void> => {
      const gate = this.nativeLoadGate;
      if (gate !== null) {
        await gate;
      }
      await this.initializeSession(requestedModules, restoredStateMap, state, generation);
    });
  }

  onNewWant(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    // An already-running Ability receives a notification activation as a new launch want here.
    this.handleNotificationWant(want);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    if (!this.acceptingLifecycle) {
      return;
    }
    const generation = ++this.windowStageGeneration;
    void this.enqueueLifecycleOperation("window-stage-create", async (): Promise<void> => {
      this.assertWindowStageActive(generation);
      const sessionId = this.bridgeSessionId;
      if (!sessionId) {
        throw new Error("Bridge session is unavailable during window-stage-create");
      }

      // First inject the ArkTS WindowStage object. Rust readiness is then advanced before ArkTS
      // plugin installation/lifecycle delivery can emit a synchronous event back into Rust.
      const stageAssignments: Promise<void>[] = [];
      for (const runtime of this.moduleRuntimes) {
        stageAssignments.push(
          BridgeHostRegistry.setWindowStage(sessionId, runtime.moduleName, windowStage),
        );
      }
      await Promise.all(stageAssignments);
      this.assertWindowStageActive(generation);
      this.forEachLifecycle((lifecycle) =>
        lifecycle.windowStageEventCallback.onWindowStageCreate(),
      );
      this.windowStageActive = true;
      await this.notifyBridgeLifecycle({
        kind: "window-stage-create",
        payload: new BridgeEmptyLifecyclePayload(),
      });
      this.assertWindowStageActive(generation);

      try {
        this.observedWindowStage = windowStage;
        windowStage.on("windowStageEvent", this.onWindowStageEvent);
        // Window visibility (windowVisibilityChange) is the reliable signal for
        // titlebar minimize/hide on 2in1, which does not fire windowStageEvent.
        const win = await windowStage.getMainWindow();
        if (win !== null && win !== undefined) {
          win.on("windowVisibilityChange", this.onWindowVisibilityChange);
          this.observedWindow = win;
        }
      } catch {}

      await this.loadWindowStageContent(windowStage);
      this.assertWindowStageActive(generation);
    });
  }

  private assertWindowStageActive(generation: number): void {
    if (!this.acceptingLifecycle || generation !== this.windowStageGeneration) {
      throw new Error("WindowStage creation was cancelled by lifecycle teardown");
    }
  }

  onWindowStageDestroy(): void {
    if (!this.acceptingLifecycle) {
      return;
    }
    this.windowStageGeneration += 1;
    this.detachWindowListeners();
    const sessionId = this.bridgeSessionId;
    if (sessionId) {
      for (const runtime of this.moduleRuntimes) {
        BridgeHostRegistry.invalidateWindowStage(sessionId, runtime.moduleName);
      }
    }
    void this.enqueueLifecycleOperation("window-stage-destroy", async (): Promise<void> => {
      await this.destroyWindowStageIfActive();
    });
  }

  private async destroyWindowStageIfActive(): Promise<void> {
    if (!this.windowStageActive) {
      return;
    }
    this.windowStageActive = false;
    this.detachWindowListeners();
    const sessionId = this.bridgeSessionId;
    if (sessionId) {
      const stageClears: Promise<void>[] = [];
      for (const runtime of this.moduleRuntimes) {
        stageClears.push(BridgeHostRegistry.clearWindowStage(sessionId, runtime.moduleName));
      }
      await Promise.all(stageClears);
    }
    for (const runtime of this.moduleRuntimes) {
      try {
        runtime.module.disposeAllRenders();
      } catch {}
    }
    this.forEachLifecycle((lifecycle) => lifecycle.windowStageEventCallback.onWindowStageDestroy());
  }

  private detachWindowListeners(): void {
    const windowStage = this.observedWindowStage;
    const win = this.observedWindow;
    this.observedWindowStage = undefined;
    this.observedWindow = undefined;
    if (windowStage !== undefined) {
      try {
        windowStage.off("windowStageEvent", this.onWindowStageEvent);
      } catch {}
    }
    if (win !== undefined) {
      try {
        win.off("windowVisibilityChange", this.onWindowVisibilityChange);
      } catch {}
    }
  }

  onMemoryLevel(level: AbilityConstant.MemoryLevel): void {
    if (!this.acceptingLifecycle) {
      return;
    }
    void this.enqueueLifecycleOperation("memory-level", async (): Promise<void> => {
      this.forEachLifecycle((lifecycle) => lifecycle.environmentCallback.onMemoryLevel(level));
      await this.notifyBridgeLifecycle({
        kind: "memory-level",
        payload: new BridgeMemoryLevelLifecyclePayload(level),
      });
    });
  }

  onDestroy(): Promise<void> {
    this.acceptingLifecycle = false;
    this.abilityGeneration += 1;
    this.windowStageGeneration += 1;
    this.detachWindowListeners();
    if (this.bridgeSessionId) {
      // Reject new calls and cancel an active plugin hook immediately; the queued teardown below
      // still preserves lifecycle ordering for state mutation and platform callbacks.
      BridgeHostRegistry.beginClosing(this.bridgeSessionId);
    }
    return this.enqueueLifecycleOperation("ability-destroy", async (): Promise<void> => {
      const sessionId = this.bridgeSessionId;
      if (!sessionId) {
        return;
      }
      BridgeHostRegistry.beginClosing(sessionId);
      try {
        await this.destroyWindowStageIfActive();
        await this.notifyBridgeLifecycle({
          kind: "ability-destroy",
          payload: new BridgeEmptyLifecyclePayload(),
        });
        this.forEachLifecycle((lifecycle) => lifecycle.windowStageEventCallback.onAbilityDestroy());
      } finally {
        const runtimes = this.moduleRuntimes;
        this.releaseModuleBridges(runtimes);
        this.bridgeSessionId = "";
        this.moduleRuntimes = [];
        this.windowStageActive = false;
        this.updateAppStorage("bridgeSessionId", "");
        await BridgeHostRegistry.dispose(sessionId);
      }
    });
  }

  onConfigurationUpdate(newConfig: Configuration): void {
    if (!this.acceptingLifecycle) {
      return;
    }
    void this.enqueueLifecycleOperation("configuration-updated", async (): Promise<void> => {
      this.forEachLifecycle((lifecycle) =>
        lifecycle.environmentCallback.onConfigurationUpdated(newConfig),
      );
      await this.notifyBridgeLifecycle({
        kind: "configuration-updated",
        payload: new BridgeEmptyLifecyclePayload(),
      });
    });
  }

  onSaveState(
    reason: AbilityConstant.StateType,
    wantParam: Record<string, object>,
  ): AbilityConstant.OnSaveResult {
    const stateMap: Record<string, string> = {};

    for (const runtime of this.moduleRuntimes) {
      try {
        stateMap[runtime.moduleName] =
          runtime.lifecycle.windowStageEventCallback.onAbilitySaveState() ?? "";
      } catch {}
    }

    const stateValues = Object.values(stateMap);
    let stateToSave: string = "";
    if (stateValues.length === 1) {
      stateToSave = stateValues[0];
    } else if (stateValues.length > 1) {
      stateToSave = this.serializeSavedStateMap(stateMap);
    }
    wantParam[STATE_KEY] = new String(stateToSave);

    return AbilityConstant.OnSaveResult.RECOVERY_AGREE;
  }
}
