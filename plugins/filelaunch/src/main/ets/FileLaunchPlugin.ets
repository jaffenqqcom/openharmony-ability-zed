import { Want } from "@kit.AbilityKit";
import {
  AsyncPluginBase,
  BridgeCallContext,
  BridgeContextRequirement,
  BridgePluginHookContext,
  BridgeTypedValue,
} from "@ohos-rs/ability";

const FILE_LAUNCH_EVENT = "file-launch";
const FILE_LAUNCH_DATA_TYPE = "ohos.filelaunch.FileLaunchData";
const FILE_LAUNCH_ACK_TYPE = "ohos.filelaunch.FileLaunchAck";

interface FileLaunchParams {
  uri?: Object;
  uris?: Object;
}

/**
 * Forwards file URIs the system asked the host application to open (double-click / "open with")
 * to Rust via the synchronous main-thread bridge. Rust resolves them to paths and
 * opens them in the active window, reusing the same path the file-drop uses.
 */
export class FileLaunchPlugin extends AsyncPluginBase {
  readonly id = "ohos.filelaunch";
  readonly requires: BridgeContextRequirement[] = ["ui-context"];
  // Own context handle. The base class already owns a private `context`, so we
  // use a differently-named field to avoid the declaration clash.
  private bridgeContext?: BridgePluginHookContext;
  // Buffers the want until onInstall provides the bridge context (cold start).
  private pendingWant?: Want;

  override onInstall(context: BridgePluginHookContext): void {
    this.bridgeContext = context;
    if (this.pendingWant !== undefined) {
      this.openFromWant(this.pendingWant);
      this.pendingWant = undefined;
    }
  }

  /** Called from EntryAbility.onCreate / onNewWant with the launch want. */
  openFromWant(want: Want): void {
    const context = this.bridgeContext;
    if (context === undefined) {
      // Plugin not installed yet (cold start): buffer until onInstall.
      this.pendingWant = want;
      return;
    }
    const uris = FileLaunchPlugin.collectUris(want);
    console.info(
      `ohos.filelaunch: openFromWant want.uri=${want.uri ?? "(none)"} ` +
        `collected=${uris.length} first=${uris[0] ?? ""}`,
    );
    if (uris.length === 0) {
      return;
    }
    try {
      context.invokeNativeSync(
        FILE_LAUNCH_EVENT,
        FILE_LAUNCH_DATA_TYPE,
        FILE_LAUNCH_ACK_TYPE,
        { uris } as ESObject,
      );
    } catch (error) {
      console.error(`ohos.filelaunch: invokeNativeSync failed: ${String(error)}`);
    }
  }

  private static collectUris(want: Want): string[] {
    const uris: string[] = [];
    if (typeof want.uri === "string" && want.uri.length > 0) {
      uris.push(want.uri);
    }
    const parameters = want.parameters as FileLaunchParams | undefined;
    if (parameters !== undefined) {
      // Single-file launches may carry the URI under the singular `uri` key
      // (e.g. some "open with" senders), multi-file under `uris`.
      FileLaunchPlugin.pushUri(parameters.uri, uris);
      FileLaunchPlugin.pushUri(parameters.uris, uris);
    }
    return uris;
  }

  // Flattens a want parameter value (string | string[] | nested array) into uris.
  private static pushUri(value: Object | undefined, uris: string[]): void {
    if (value === undefined || value === null) {
      return;
    }
    if (typeof value === "string") {
      if (value.length > 0) {
        uris.push(value);
      }
      return;
    }
    if (value instanceof Array) {
      for (let i = 0; i < value.length; i++) {
        FileLaunchPlugin.pushUri(value[i], uris);
      }
    }
  }

  async invokeAsync(
    _action: string,
    _request: BridgeTypedValue,
    _context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    throw new Error(`Unsupported ohos.filelaunch action '${_action}'`);
  }
}
