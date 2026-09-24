import fileUri from "@ohos.file.fileuri";
import Want from "@ohos.app.ability.Want";
import wantConstant from "@ohos.app.ability.wantConstant";
import {
  BridgeCallContext,
  BridgeTypedValue,
  AsyncPluginBase,
  BridgeContextRequirement,
} from "@ohos-rs/ability";

const OPEN_REQUEST_TYPE = "ohos.openbysys.OpenRequest";
const OPEN_RESPONSE_TYPE = "ohos.openbysys.OpenResponse";
const REVEAL_REQUEST_TYPE = "ohos.openbysys.RevealRequest";
const REVEAL_RESPONSE_TYPE = "ohos.openbysys.RevealResponse";

// The system file manager declares this link in the skills of its main ability
// (`scheme: filemanager`, `host: openDirectory`), so it opens on the `file://` URI
// passed as the `fileUri` parameter.
const OPEN_DIRECTORY_LINK = "filemanager://openDirectory";

// The one action an implicit file-open want may carry.
const VIEW_DATA_ACTION = "ohos.want.action.viewData";

interface PathRequest {
  path: string;
}

class AcceptedResponse {
  readonly accepted: boolean;

  constructor(accepted: boolean) {
    this.accepted = accepted;
  }
}

function parsePathRequest(
  payload: BridgeTypedValue,
  requestType: string,
  action: string,
): PathRequest {
  if (payload.typeName !== requestType || typeof payload.value !== "object") {
    throw new Error(`ohos.openbysys.${action} requires bridge type ${requestType}`);
  }
  const request = payload.value as PathRequest;
  if (typeof request.path !== "string" || !request.path.trim() || !request.path.startsWith("/")) {
    throw new Error(`${action}.path must be an absolute local path`);
  }
  return request;
}

function pathToFileUri(path: string): string {
  try {
    return fileUri.getUriFromPath(path);
  } catch (error) {
    throw new Error(`could not map '${path}' to a file URI: ${String(error)}`);
  }
}

export class OpenBySysPlugin extends AsyncPluginBase {
  readonly id = "ohos.openbysys";
  readonly requires: BridgeContextRequirement[] = ["ability"];

  async invokeAsync(
    action: string,
    payload: BridgeTypedValue,
    context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    if (action === "open-file") {
      return this.openFile(payload, context);
    }
    if (action === "reveal-file") {
      return this.revealFile(payload, context);
    }
    throw new Error(`Unsupported ohos.openbysys action '${action}'`);
  }

  // Hands the path to the application registered for its file type. `type` is left unset on
  // purpose: the system then derives it from the URI suffix, whereas a named type has to
  // match the file exactly or no application is found.
  private async openFile(
    payload: BridgeTypedValue,
    context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    const request = parsePathRequest(payload, OPEN_REQUEST_TYPE, "open-file");
    const want: Want = {
      action: VIEW_DATA_ACTION,
      uri: pathToFileUri(request.path),
      flags:
        wantConstant.Flags.FLAG_AUTH_READ_URI_PERMISSION |
        wantConstant.Flags.FLAG_AUTH_WRITE_URI_PERMISSION,
    };
    try {
      await context.abilityContext.startAbility(want);
    } catch (error) {
      throw new Error(`startAbility failed for '${request.path}': ${String(error)}`);
    }
    return this.accepted(context, OPEN_RESPONSE_TYPE);
  }

  private async revealFile(
    payload: BridgeTypedValue,
    context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    const request = parsePathRequest(payload, REVEAL_REQUEST_TYPE, "reveal-file");
    try {
      await context.abilityContext.openLink(OPEN_DIRECTORY_LINK, {
        parameters: { fileUri: pathToFileUri(request.path) },
      });
    } catch (error) {
      throw new Error(`openLink failed for '${request.path}': ${String(error)}`);
    }
    return this.accepted(context, REVEAL_RESPONSE_TYPE);
  }

  private accepted(context: BridgeCallContext, responseType: string): BridgeTypedValue {
    if (!context.isActive()) {
      throw new Error("Ability was destroyed while the system was opening the path");
    }
    return { typeName: responseType, value: new AcceptedResponse(true) };
  }
}
