import { media } from "@kit.MediaKit";
import { audio } from "@kit.AudioKit";
import common from "@ohos.app.ability.common";
import fs from "@ohos.file.fs";
import hilog from "@ohos.hilog";
import {
  AsyncPluginBase,
  BridgeCallContext,
  BridgeContextRequirement,
  BridgePluginContext,
  BridgePluginHookContext,
  BridgeTypedValue,
} from "@ohos-rs/ability";

const DOMAIN: number = 0x0000;
const TAG: string = "PlayNotificationBridgePlugin";

const CREATE_REQUEST = "ohos.play-notification.CreateRequest";
const LOAD_REQUEST = "ohos.play-notification.LoadRequest";
const PLAY_REQUEST = "ohos.play-notification.PlayRequest";
const STREAM_REQUEST = "ohos.play-notification.StreamRequest";
const INTERRUPT_MODE_REQUEST = "ohos.play-notification.InterruptModeRequest";
const VOLUME_REQUEST = "ohos.play-notification.VolumeRequest";
const LOOP_REQUEST = "ohos.play-notification.LoopRequest";
const RATE_REQUEST = "ohos.play-notification.RateRequest";
const PRIORITY_REQUEST = "ohos.play-notification.PriorityRequest";
const UNLOAD_REQUEST = "ohos.play-notification.UnloadRequest";
const RELEASE_REQUEST = "ohos.play-notification.ReleaseRequest";

const ACK_RESPONSE = "ohos.play-notification.AckResponse";
const LOAD_RESPONSE = "ohos.play-notification.LoadResponse";
const PLAY_RESPONSE = "ohos.play-notification.PlayResponse";

const MIN_RATE = 0;
const MAX_RATE = 2;
const MIN_INTERRUPT_MODE = 0;
const MAX_INTERRUPT_MODE = 1;

/** Upper bound for one load; a missing loadComplete must not hang the caller. */
const LOAD_TIMEOUT_MS = 10000;

const SOURCE_RAWFILE = "rawfile";
const SOURCE_FILE = "file";
const SOURCE_FD = "fd";

interface CreatePayload {
  maxStreams: number;
  usage: number;
  rendererFlags: number;
}

interface LoadPayload {
  kind: string;
  source: string;
  fd: number;
  offset: number;
  length: number;
}

interface PlayPayload {
  soundId: number;
  loopCount?: number;
  rate?: number;
  leftVolume?: number;
  rightVolume?: number;
  priority?: number;
}

interface StreamPayload {
  streamId: number;
}

interface InterruptModePayload {
  mode: number;
}

interface VolumePayload {
  streamId: number;
  leftVolume: number;
  rightVolume: number;
}

interface LoopPayload {
  streamId: number;
  loopCount: number;
}

interface RatePayload {
  streamId: number;
  rate: number;
}

interface PriorityPayload {
  streamId: number;
  priority: number;
}

interface UnloadPayload {
  soundId: number;
}

interface SoundPoolErrorInfo {
  soundId?: number;
  streamId?: number;
  errorType?: number;
}

function requireType(payload: BridgeTypedValue, expected: string): ESObject {
  if (payload.typeName !== expected || typeof payload.value !== "object" || payload.value === null) {
    throw new Error(`requires bridge type ${expected}`);
  }
  return payload.value;
}

function ack(): BridgeTypedValue {
  return { typeName: ACK_RESPONSE, value: { accepted: true } };
}

function requireVolume(field: string, value: number): void {
  if (!Number.isFinite(value) || value < 0 || value > 1) {
    throw new Error(`${field} must be between 0.0 and 1.0`);
  }
}

function requireRate(value: number): void {
  if (!Number.isInteger(value) || value < MIN_RATE || value > MAX_RATE) {
    throw new Error(`rate must be between ${MIN_RATE} and ${MAX_RATE}`);
  }
}

function requirePriority(value: number): void {
  if (!Number.isInteger(value) || value < 0) {
    throw new Error("priority must be a non-negative integer");
  }
}

function requireInterruptMode(value: number): void {
  if (!Number.isInteger(value) || value < MIN_INTERRUPT_MODE || value > MAX_INTERRUPT_MODE) {
    throw new Error(`interrupt mode must be between ${MIN_INTERRUPT_MODE} and ${MAX_INTERRUPT_MODE}`);
  }
}

/**
 * SoundPool plugin.
 *
 * ArkTS owns the SoundPool instance, the loaded sound ids and every descriptor
 * opened for a load. Rust receives only numeric sound and stream ids, so no
 * platform object crosses the N-API boundary.
 */
export class PlayNotificationPlugin extends AsyncPluginBase {
  readonly id = "ohos.play-notification";
  readonly requires: BridgeContextRequirement[] = ["ability"];

  private pool?: media.SoundPool;
  /** In-flight pool creation shared by concurrent first plays. */
  private creatingPool?: Promise<media.SoundPool>;
  private abilityContext?: common.UIAbilityContext;
  /** Rawfile paths kept open until their sound is unloaded. */
  private rawFiles: Map<number, string> = new Map<number, string>();
  private fileFds: Map<number, number> = new Map<number, number>();
  /** Sounds whose loadComplete event already arrived. */
  private loadedSounds: Set<number> = new Set<number>();
  /** Loads still waiting for their loadComplete event. */
  private pendingLoads: Map<number, (error?: Error) => void> =
    new Map<number, (error?: Error) => void>();

  override attachContext(context: BridgePluginContext): void {
    super.attachContext(context);
    this.abilityContext = context.abilityContext;
  }

  override async onDispose(context: BridgePluginHookContext): Promise<void> {
    await this.releasePool(context);
  }

  async invokeAsync(
    action: string,
    request: BridgeTypedValue,
    context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    try {
      switch (action) {
        case "create":
          return await this.create(request);
        case "load":
          return await this.load(request, context);
        case "play":
          return await this.play(request);
        case "stop":
          return await this.stop(request);
        case "set-interrupt-mode":
          return this.setInterruptMode(request);
        case "set-volume":
          return await this.setVolume(request);
        case "set-loop":
          return await this.setLoop(request);
        case "set-rate":
          return await this.setRate(request);
        case "set-priority":
          return await this.setPriority(request);
        case "unload":
          return await this.unload(request, context);
        case "release":
          await this.releasePool(context);
          return ack();
        default:
          throw new Error(`Unsupported ohos.play-notification action '${action}'`);
      }
    } catch (error) {
      hilog.error(DOMAIN, TAG, "action %{public}s failed: %{public}s", action, String(error));
      throw error as Error;
    }
  }

  private requirePool(): media.SoundPool {
    if (this.pool === undefined) {
      throw new Error("SoundPool is not created; call 'create' first");
    }
    return this.pool;
  }

  private async create(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, CREATE_REQUEST) as CreatePayload;
    if (!Number.isInteger(payload.maxStreams) || payload.maxStreams <= 0) {
      throw new Error("maxStreams must be a positive integer");
    }
    if (!Number.isInteger(payload.usage) || payload.usage < 0) {
      throw new Error("usage must be a non-negative StreamUsage value");
    }
    if (!Number.isInteger(payload.rendererFlags) || payload.rendererFlags < 0) {
      throw new Error("rendererFlags must be a non-negative integer");
    }

    if (this.pool !== undefined) {
      hilog.info(DOMAIN, TAG, "SoundPool already created; reusing it");
      return ack();
    }

    const info: audio.AudioRendererInfo = {
      usage: payload.usage as audio.StreamUsage,
      rendererFlags: payload.rendererFlags,
    };
    // Concurrent first plays share one creation attempt: a second caller that
    // arrives while the first is awaiting must not create another pool, or the
    // later assignment would leak the earlier instance.
    if (this.creatingPool === undefined) {
      this.creatingPool = media.createSoundPool(payload.maxStreams, info);
    }
    let pool: media.SoundPool;
    try {
      pool = await this.creatingPool;
    } finally {
      this.creatingPool = undefined;
    }
    if (this.pool !== undefined) {
      hilog.info(DOMAIN, TAG, "SoundPool already created; reusing it");
      return ack();
    }
    pool.on("playFinishedWithStreamId", (streamId: number): void => {
      hilog.debug(DOMAIN, TAG, "play finished stream %{public}d", streamId);
    });
    pool.on("loadComplete", (soundId: number): void => {
      const pending = this.pendingLoads.get(soundId);
      if (pending !== undefined) {
        this.pendingLoads.delete(soundId);
        pending();
      } else {
        // The event can arrive before load() resolves; remember it for then.
        this.loadedSounds.add(soundId);
      }
      hilog.debug(DOMAIN, TAG, "load complete for sound %{public}d", soundId);
    });
    pool.on("errorOccurred", (error: ESObject): void => {
      const info = error as SoundPoolErrorInfo;
      if (typeof info.soundId === "number") {
        const pending = this.pendingLoads.get(info.soundId);
        if (pending !== undefined) {
          this.pendingLoads.delete(info.soundId);
          pending(new Error(`SoundPool failed to load sound ${info.soundId}`));
        }
      }
      hilog.error(DOMAIN, TAG, "SoundPool error: %{public}s", String(error));
    });
    this.pool = pool;
    hilog.info(DOMAIN, TAG, "created SoundPool maxStreams=%{public}d usage=%{public}d",
      payload.maxStreams, payload.usage);
    return ack();
  }

  private async load(
    request: BridgeTypedValue,
    context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    const payload = requireType(request, LOAD_REQUEST) as LoadPayload;
    const pool = this.requirePool();
    let soundId: number;

    if (payload.kind === SOURCE_RAWFILE) {
      const rawFd = await context.abilityContext.resourceManager.getRawFd(payload.source);
      try {
        soundId = await pool.load(rawFd.fd, rawFd.offset, rawFd.length);
      } catch (error) {
        await context.abilityContext.resourceManager.closeRawFd(payload.source);
        throw error as Error;
      }
      this.rawFiles.set(soundId, payload.source);
    } else if (payload.kind === SOURCE_FILE) {
      const file = fs.openSync(payload.source, fs.OpenMode.READ_ONLY);
      try {
        const stat = fs.statSync(payload.source);
        soundId = await pool.load(file.fd, 0, stat.size);
      } catch (error) {
        fs.closeSync(file);
        throw error as Error;
      }
      this.fileFds.set(soundId, file.fd);
    } else if (payload.kind === SOURCE_FD) {
      if (!Number.isInteger(payload.fd) || payload.fd < 0 ||
        !Number.isInteger(payload.offset) || payload.offset < 0 ||
        !Number.isInteger(payload.length) || payload.length <= 0) {
        throw new Error("fd sources require fd >= 0, offset >= 0, and length > 0");
      }
      soundId = await pool.load(payload.fd, payload.offset, payload.length);
    } else {
      throw new Error(`unsupported sound source '${payload.kind}'`);
    }

    // load() resolves once the request is accepted; playback is only allowed
    // after the matching loadComplete event.
    await this.awaitSoundLoaded(soundId);
    hilog.info(DOMAIN, TAG, "loaded sound %{public}d from %{public}s", soundId, payload.kind);
    return { typeName: LOAD_RESPONSE, value: { soundId } };
  }

  private awaitSoundLoaded(soundId: number): Promise<void> {
    if (this.loadedSounds.has(soundId)) {
      this.loadedSounds.delete(soundId);
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pendingLoads.delete(soundId);
        reject(new Error(`Timed out waiting for sound ${soundId} to load`));
      }, LOAD_TIMEOUT_MS);
      this.pendingLoads.set(soundId, (error?: Error): void => {
        clearTimeout(timeout);
        if (error !== undefined) {
          reject(error);
        } else {
          resolve();
        }
      });
    });
  }

  private async play(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, PLAY_REQUEST) as PlayPayload;
    if (!Number.isInteger(payload.soundId) || payload.soundId < 0) {
      throw new Error("soundId must be a non-negative integer");
    }

    const parameters: media.PlayParameters = {};
    if (payload.loopCount !== undefined) {
      if (!Number.isInteger(payload.loopCount)) {
        throw new Error("loopCount must be an integer");
      }
      parameters.loop = payload.loopCount;
    }
    if (payload.rate !== undefined) {
      requireRate(payload.rate);
      parameters.rate = payload.rate;
    }
    if (payload.leftVolume !== undefined) {
      requireVolume("leftVolume", payload.leftVolume);
      parameters.leftVolume = payload.leftVolume;
    }
    if (payload.rightVolume !== undefined) {
      requireVolume("rightVolume", payload.rightVolume);
      parameters.rightVolume = payload.rightVolume;
    }
    if (payload.priority !== undefined) {
      requirePriority(payload.priority);
      parameters.priority = payload.priority;
    }

    const streamId = await this.requirePool().play(payload.soundId, parameters);
    return { typeName: PLAY_RESPONSE, value: { streamId } };
  }

  private async stop(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, STREAM_REQUEST) as StreamPayload;
    await this.requirePool().stop(payload.streamId);
    return ack();
  }

  private setInterruptMode(request: BridgeTypedValue): BridgeTypedValue {
    const payload = requireType(request, INTERRUPT_MODE_REQUEST) as InterruptModePayload;
    requireInterruptMode(payload.mode);
    this.requirePool().setInterruptMode(payload.mode as media.SoundInterruptMode);
    return ack();
  }

  private async setVolume(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, VOLUME_REQUEST) as VolumePayload;
    requireVolume("leftVolume", payload.leftVolume);
    requireVolume("rightVolume", payload.rightVolume);
    await this.requirePool().setVolume(payload.streamId, payload.leftVolume, payload.rightVolume);
    return ack();
  }

  private async setLoop(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, LOOP_REQUEST) as LoopPayload;
    if (!Number.isInteger(payload.loopCount)) {
      throw new Error("loopCount must be an integer");
    }
    await this.requirePool().setLoop(payload.streamId, payload.loopCount);
    return ack();
  }

  private async setRate(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, RATE_REQUEST) as RatePayload;
    requireRate(payload.rate);
    await this.requirePool().setRate(payload.streamId, payload.rate as audio.AudioRendererRate);
    return ack();
  }

  private async setPriority(request: BridgeTypedValue): Promise<BridgeTypedValue> {
    const payload = requireType(request, PRIORITY_REQUEST) as PriorityPayload;
    requirePriority(payload.priority);
    await this.requirePool().setPriority(payload.streamId, payload.priority);
    return ack();
  }

  private async unload(
    request: BridgeTypedValue,
    context: BridgeCallContext,
  ): Promise<BridgeTypedValue> {
    const payload = requireType(request, UNLOAD_REQUEST) as UnloadPayload;
    this.loadedSounds.delete(payload.soundId);
    const pending = this.pendingLoads.get(payload.soundId);
    if (pending !== undefined) {
      this.pendingLoads.delete(payload.soundId);
      pending(new Error(`sound ${payload.soundId} was unloaded while loading`));
    }
    await this.requirePool().unload(payload.soundId);

    const rawfileName = this.rawFiles.get(payload.soundId);
    if (rawfileName !== undefined) {
      await context.abilityContext.resourceManager.closeRawFd(rawfileName);
      this.rawFiles.delete(payload.soundId);
    }
    const fd = this.fileFds.get(payload.soundId);
    if (fd !== undefined) {
      fs.closeSync(fd);
      this.fileFds.delete(payload.soundId);
    }
    return ack();
  }

  private async releasePool(context: BridgePluginContext | undefined): Promise<void> {
    const pendingLoads = Array.from(this.pendingLoads.values());
    for (const pending of pendingLoads) {
      pending(new Error("SoundPool released while a sound was loading"));
    }
    this.pendingLoads.clear();
    this.loadedSounds.clear();

    const pool = this.pool;
    this.pool = undefined;
    if (pool !== undefined) {
      try {
        await pool.release();
      } catch (error) {
        hilog.error(DOMAIN, TAG, "SoundPool release failed: %{public}s", String(error));
      }
    }

    const abilityContext = context !== undefined ? context.abilityContext : this.abilityContext;
    const rawSoundIds = Array.from(this.rawFiles.keys());
    for (const soundId of rawSoundIds) {
      const rawfileName = this.rawFiles.get(soundId);
      if (rawfileName !== undefined && abilityContext !== undefined) {
        try {
          await abilityContext.resourceManager.closeRawFd(rawfileName);
        } catch (error) {
          hilog.error(DOMAIN, TAG, "closeRawFd failed: %{public}s", String(error));
        }
      }
    }
    this.rawFiles.clear();

    const fileSoundIds = Array.from(this.fileFds.keys());
    for (const soundId of fileSoundIds) {
      const fd = this.fileFds.get(soundId);
      if (fd !== undefined) {
        try {
          fs.closeSync(fd);
        } catch (error) {
          hilog.error(DOMAIN, TAG, "close sound file failed: %{public}s", String(error));
        }
      }
    }
    this.fileFds.clear();
  }
}
