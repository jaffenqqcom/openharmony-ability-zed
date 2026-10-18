//! Sound effect capability plugin backed by the ArkTS `@ohos-rs/plugin-play-notification` HAR.
//!
//! The plugin mirrors the OpenHarmony `SoundPool` surface: create a pool, load a
//! rawfile / file / file-descriptor source, play it with per-stream volume, loop,
//! rate and priority, control a stream, unload a sound, and release the pool.
//! ArkTS owns every platform object; Rust passes only numeric sound and stream ids.
//!
//! SoundPool's asynchronous notifications (`loadComplete`, `playFinished*`,
//! `errorOccurred`) are observed inside the ArkTS plugin. The bridge's inbound
//! event path is only valid while an ArkTS-initiated N-API callback is active, so
//! those delayed callbacks are logged on the ArkTS side instead of being forwarded
//! across the bridge.

use std::{future::Future, pin::Pin};

use napi_derive_ohos::napi;
use napi_ohos::{Error, Result};
use openharmony_ability::{
    impl_bridge_napi_type, AsyncBridge, BridgeCallOptions, BridgeContextRequirement,
    BridgeNapiType, BridgePlugin, OpenHarmonyApp,
};

/// Timeout for one SoundPool call. Loading a file can take longer than a
/// control call, and every action finishes well within this bound on a device.
const CALL_TIMEOUT_MS: u32 = 30_000;

const ACTION_CREATE: &str = "create";
const ACTION_LOAD: &str = "load";
const ACTION_PLAY: &str = "play";
const ACTION_STOP: &str = "stop";
const ACTION_SET_INTERRUPT_MODE: &str = "set-interrupt-mode";
const ACTION_SET_VOLUME: &str = "set-volume";
const ACTION_SET_LOOP: &str = "set-loop";
const ACTION_SET_RATE: &str = "set-rate";
const ACTION_SET_PRIORITY: &str = "set-priority";
const ACTION_UNLOAD: &str = "unload";
const ACTION_RELEASE: &str = "release";

/// Lowest and highest valid `audio.AudioRendererRate` values.
const MIN_RATE: i32 = 0;
const MAX_RATE: i32 = 2;

/// Lowest and highest valid `media.SoundInterruptMode` values.
const MIN_INTERRUPT_MODE: i32 = 0;
const MAX_INTERRUPT_MODE: i32 = 1;

pub struct PlayNotificationBridgePlugin;

impl BridgePlugin for PlayNotificationBridgePlugin {
    type Mode = AsyncBridge;

    const ID: &'static str = "ohos.play-notification";
    const REQUIRED_CONTEXTS: &'static [BridgeContextRequirement] =
        &[BridgeContextRequirement::Ability];
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundCreateRequest {
    pub max_streams: u32,
    /// `audio.StreamUsage` value.
    pub usage: i32,
    /// `audio.AudioRendererInfo.rendererFlags` value.
    pub renderer_flags: i32,
}

impl_bridge_napi_type!(SoundCreateRequest, "ohos.play-notification.CreateRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundAckResponse {
    pub accepted: bool,
}

impl_bridge_napi_type!(SoundAckResponse, "ohos.play-notification.AckResponse");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundLoadRequest {
    /// `rawfile`, `file`, or `fd`.
    pub kind: String,
    /// Rawfile name or absolute file path; empty when `kind` is `fd`.
    pub source: String,
    /// File descriptor for `kind = fd`; ignored otherwise.
    pub fd: i32,
    /// Byte offset inside the descriptor for `kind = fd`.
    pub offset: i32,
    /// Byte length for `kind = fd`.
    pub length: i32,
}

impl_bridge_napi_type!(SoundLoadRequest, "ohos.play-notification.LoadRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundLoadResponse {
    pub sound_id: u32,
}

impl_bridge_napi_type!(SoundLoadResponse, "ohos.play-notification.LoadResponse");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundPlayRequest {
    pub sound_id: u32,
    /// Number of extra plays. A negative value loops until stopped.
    pub loop_count: Option<i32>,
    /// `audio.AudioRendererRate` value.
    pub rate: Option<i32>,
    pub left_volume: Option<f64>,
    pub right_volume: Option<f64>,
    pub priority: Option<i32>,
}

impl_bridge_napi_type!(SoundPlayRequest, "ohos.play-notification.PlayRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundPlayResponse {
    pub stream_id: u32,
}

impl_bridge_napi_type!(SoundPlayResponse, "ohos.play-notification.PlayResponse");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundStreamRequest {
    pub stream_id: u32,
}

impl_bridge_napi_type!(SoundStreamRequest, "ohos.play-notification.StreamRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundInterruptModeRequest {
    /// `media.SoundInterruptMode` value.
    pub mode: i32,
}

impl_bridge_napi_type!(
    SoundInterruptModeRequest,
    "ohos.play-notification.InterruptModeRequest"
);

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundVolumeRequest {
    pub stream_id: u32,
    pub left_volume: f64,
    pub right_volume: f64,
}

impl_bridge_napi_type!(SoundVolumeRequest, "ohos.play-notification.VolumeRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundLoopRequest {
    pub stream_id: u32,
    pub loop_count: i32,
}

impl_bridge_napi_type!(SoundLoopRequest, "ohos.play-notification.LoopRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundRateRequest {
    pub stream_id: u32,
    pub rate: i32,
}

impl_bridge_napi_type!(SoundRateRequest, "ohos.play-notification.RateRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundPriorityRequest {
    pub stream_id: u32,
    pub priority: i32,
}

impl_bridge_napi_type!(SoundPriorityRequest, "ohos.play-notification.PriorityRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundUnloadRequest {
    pub sound_id: u32,
}

impl_bridge_napi_type!(SoundUnloadRequest, "ohos.play-notification.UnloadRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct SoundReleaseRequest {
    /// Reserved: confirms that the caller wants every loaded sound dropped.
    pub confirmed: bool,
}

impl_bridge_napi_type!(SoundReleaseRequest, "ohos.play-notification.ReleaseRequest");

/// Where a sound comes from.
#[derive(Clone, Debug)]
pub enum SoundSource {
    /// Resource in the HAP's `rawfile` directory, for example `sounds/done.wav`.
    Rawfile(String),
    /// Absolute path to a file the daemon can open.
    File(String),
    /// An already-open descriptor owned by the caller.
    Fd {
        fd: i32,
        offset: i32,
        length: i32,
    },
}

impl SoundSource {
    fn into_request(self) -> Result<SoundLoadRequest> {
        match self {
            Self::Rawfile(name) => {
                if name.trim().is_empty() {
                    return Err(Error::from_reason("rawfile name must not be empty"));
                }
                Ok(SoundLoadRequest {
                    kind: "rawfile".to_owned(),
                    source: name,
                    fd: -1,
                    offset: 0,
                    length: 0,
                })
            }
            Self::File(path) => {
                if path.trim().is_empty() {
                    return Err(Error::from_reason("sound file path must not be empty"));
                }
                Ok(SoundLoadRequest {
                    kind: "file".to_owned(),
                    source: path,
                    fd: -1,
                    offset: 0,
                    length: 0,
                })
            }
            Self::Fd {
                fd,
                offset,
                length,
            } => {
                if fd < 0 || offset < 0 || length <= 0 {
                    return Err(Error::from_reason(
                        "fd sources require fd >= 0, offset >= 0, and length > 0",
                    ));
                }
                Ok(SoundLoadRequest {
                    kind: "fd".to_owned(),
                    source: String::new(),
                    fd,
                    offset,
                    length,
                })
            }
        }
    }
}

/// Optional `PlayParameters` overrides; omitted fields keep the platform default.
#[derive(Clone, Debug, Default)]
pub struct SoundPlayParameters {
    pub loop_count: Option<i32>,
    pub rate: Option<i32>,
    pub left_volume: Option<f64>,
    pub right_volume: Option<f64>,
    pub priority: Option<i32>,
}

impl SoundPlayParameters {
    fn into_request(self, sound_id: u32) -> Result<SoundPlayRequest> {
        if let Some(rate) = self.rate {
            validate_rate(rate)?;
        }
        if let Some(volume) = self.left_volume {
            validate_volume("left_volume", volume)?;
        }
        if let Some(volume) = self.right_volume {
            validate_volume("right_volume", volume)?;
        }
        if let Some(priority) = self.priority {
            validate_priority(priority)?;
        }
        Ok(SoundPlayRequest {
            sound_id,
            loop_count: self.loop_count,
            rate: self.rate,
            left_volume: self.left_volume,
            right_volume: self.right_volume,
            priority: self.priority,
        })
    }
}

fn validate_create(
    max_streams: u32,
    usage: i32,
    renderer_flags: i32,
) -> Result<SoundCreateRequest> {
    if max_streams == 0 {
        return Err(Error::from_reason("max_streams must be greater than zero"));
    }
    if usage < 0 {
        return Err(Error::from_reason(
            "usage must be a non-negative StreamUsage value",
        ));
    }
    if renderer_flags < 0 {
        return Err(Error::from_reason("renderer_flags must be non-negative"));
    }
    Ok(SoundCreateRequest {
        max_streams,
        usage,
        renderer_flags,
    })
}

fn validate_volume(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(Error::from_reason(format!(
            "{field} must be a finite value between 0.0 and 1.0"
        )));
    }
    Ok(())
}

fn validate_rate(rate: i32) -> Result<()> {
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(Error::from_reason(format!(
            "rate must be an AudioRendererRate between {MIN_RATE} and {MAX_RATE}"
        )));
    }
    Ok(())
}

fn validate_priority(priority: i32) -> Result<()> {
    if priority < 0 {
        return Err(Error::from_reason("priority must be non-negative"));
    }
    Ok(())
}

fn validate_interrupt_mode(mode: i32) -> Result<()> {
    if !(MIN_INTERRUPT_MODE..=MAX_INTERRUPT_MODE).contains(&mode) {
        return Err(Error::from_reason(format!(
            "interrupt mode must be a SoundInterruptMode between {MIN_INTERRUPT_MODE} and {MAX_INTERRUPT_MODE}"
        )));
    }
    Ok(())
}

/// Builds one asynchronous bridge call, deferring validation errors into the
/// returned future so every facade method has a uniform shape.
fn call<P, Request, Response>(
    app: &OpenHarmonyApp,
    action: &'static str,
    request: Result<Request>,
) -> Pin<Box<dyn Future<Output = Result<Response>> + Send>>
where
    P: BridgePlugin<Mode = AsyncBridge>,
    Request: BridgeNapiType,
    Response: BridgeNapiType,
{
    let bridge = app.bridge();
    Box::pin(async move {
        bridge?
            .call_async::<P, Request, Response>(
                action,
                request?,
                BridgeCallOptions::default().with_timeout_ms(CALL_TIMEOUT_MS),
            )
            .await
    })
}

/// Extension trait supplied by the capability package, never by `openharmony-ability` core.
pub trait PlayNotificationExt {
    /// Creates the process-wide SoundPool. Calling it again replaces the pool.
    fn create_sound_pool(
        &self,
        max_streams: u32,
        usage: i32,
        renderer_flags: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    /// Loads one sound and returns its sound id.
    fn load_sound(
        &self,
        source: SoundSource,
    ) -> Pin<Box<dyn Future<Output = Result<u32>> + Send>>;

    /// Plays a loaded sound and returns its stream id.
    fn play_sound(
        &self,
        sound_id: u32,
        parameters: SoundPlayParameters,
    ) -> Pin<Box<dyn Future<Output = Result<u32>> + Send>>;

    fn stop_sound(&self, stream_id: u32) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    /// Sets how simultaneously played sounds interrupt each other.
    fn set_sound_interrupt_mode(
        &self,
        mode: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    fn set_sound_volume(
        &self,
        stream_id: u32,
        left_volume: f64,
        right_volume: f64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    fn set_sound_loop(
        &self,
        stream_id: u32,
        loop_count: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    fn set_sound_rate(
        &self,
        stream_id: u32,
        rate: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    fn set_sound_priority(
        &self,
        stream_id: u32,
        priority: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    fn unload_sound(&self, sound_id: u32) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    /// Releases the pool and every resource the ArkTS side holds for it.
    fn release_sound_pool(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;
}

impl PlayNotificationExt for OpenHarmonyApp {
    fn create_sound_pool(
        &self,
        max_streams: u32,
        usage: i32,
        renderer_flags: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!(
            "sound: create_sound_pool max_streams={max_streams} usage={usage} renderer_flags={renderer_flags}"
        );
        let request = validate_create(max_streams, usage, renderer_flags);
        let future = call::<PlayNotificationBridgePlugin, SoundCreateRequest, SoundAckResponse>(
            self,
            ACTION_CREATE,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn load_sound(
        &self,
        source: SoundSource,
    ) -> Pin<Box<dyn Future<Output = Result<u32>> + Send>> {
        log::debug!("sound: load_sound");
        let request = source.into_request();
        let future = call::<PlayNotificationBridgePlugin, SoundLoadRequest, SoundLoadResponse>(
            self, ACTION_LOAD, request,
        );
        Box::pin(async move { Ok(future.await?.sound_id) })
    }

    fn play_sound(
        &self,
        sound_id: u32,
        parameters: SoundPlayParameters,
    ) -> Pin<Box<dyn Future<Output = Result<u32>> + Send>> {
        log::debug!("sound: play_sound sound_id={sound_id}");
        let request = parameters.into_request(sound_id);
        let future = call::<PlayNotificationBridgePlugin, SoundPlayRequest, SoundPlayResponse>(
            self, ACTION_PLAY, request,
        );
        Box::pin(async move { Ok(future.await?.stream_id) })
    }

    fn stop_sound(&self, stream_id: u32) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: stop_sound stream_id={stream_id}");
        let request = Ok(SoundStreamRequest { stream_id });
        let future = call::<PlayNotificationBridgePlugin, SoundStreamRequest, SoundAckResponse>(
            self,
            ACTION_STOP,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn set_sound_interrupt_mode(
        &self,
        mode: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: set_sound_interrupt_mode mode={mode}");
        let request = validate_interrupt_mode(mode).map(|()| SoundInterruptModeRequest { mode });
        let future = call::<PlayNotificationBridgePlugin, SoundInterruptModeRequest, SoundAckResponse>(
            self,
            ACTION_SET_INTERRUPT_MODE,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn set_sound_volume(
        &self,
        stream_id: u32,
        left_volume: f64,
        right_volume: f64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!(
            "sound: set_sound_volume stream_id={stream_id} left={left_volume} right={right_volume}"
        );
        let request = match (
            validate_volume("left_volume", left_volume),
            validate_volume("right_volume", right_volume),
        ) {
            (Ok(()), Ok(())) => Ok(SoundVolumeRequest {
                stream_id,
                left_volume,
                right_volume,
            }),
            (Err(error), _) | (_, Err(error)) => Err(error),
        };
        let future = call::<PlayNotificationBridgePlugin, SoundVolumeRequest, SoundAckResponse>(
            self,
            ACTION_SET_VOLUME,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn set_sound_loop(
        &self,
        stream_id: u32,
        loop_count: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: set_sound_loop stream_id={stream_id} loop_count={loop_count}");
        let request = Ok(SoundLoopRequest {
            stream_id,
            loop_count,
        });
        let future = call::<PlayNotificationBridgePlugin, SoundLoopRequest, SoundAckResponse>(
            self,
            ACTION_SET_LOOP,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn set_sound_rate(
        &self,
        stream_id: u32,
        rate: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: set_sound_rate stream_id={stream_id} rate={rate}");
        let request = validate_rate(rate).map(|()| SoundRateRequest { stream_id, rate });
        let future = call::<PlayNotificationBridgePlugin, SoundRateRequest, SoundAckResponse>(
            self,
            ACTION_SET_RATE,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn set_sound_priority(
        &self,
        stream_id: u32,
        priority: i32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: set_sound_priority stream_id={stream_id} priority={priority}");
        let request = validate_priority(priority)
            .map(|()| SoundPriorityRequest {
                stream_id,
                priority,
            });
        let future = call::<PlayNotificationBridgePlugin, SoundPriorityRequest, SoundAckResponse>(
            self,
            ACTION_SET_PRIORITY,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn unload_sound(&self, sound_id: u32) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: unload_sound sound_id={sound_id}");
        let request = Ok(SoundUnloadRequest { sound_id });
        let future = call::<PlayNotificationBridgePlugin, SoundUnloadRequest, SoundAckResponse>(
            self,
            ACTION_UNLOAD,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn release_sound_pool(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        log::debug!("sound: release_sound_pool");
        let request = Ok(SoundReleaseRequest { confirmed: true });
        let future = call::<PlayNotificationBridgePlugin, SoundReleaseRequest, SoundAckResponse>(
            self,
            ACTION_RELEASE,
            request,
        );
        Box::pin(async move { future.await.map(|_| ()) })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        validate_create, validate_interrupt_mode, validate_priority, validate_rate,
        validate_volume, SoundPlayParameters, SoundSource,
    };
    use openharmony_ability::BridgeNapiType;

    #[test]
    fn sound_uses_stable_named_napi_contracts() {
        assert_eq!(
            <super::SoundCreateRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.CreateRequest"
        );
        assert_eq!(
            <super::SoundLoadRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.LoadRequest"
        );
        assert_eq!(
            <super::SoundPlayRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.PlayRequest"
        );
        assert_eq!(
            <super::SoundPlayResponse as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.PlayResponse"
        );
        assert_eq!(
            <super::SoundStreamRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.StreamRequest"
        );
        assert_eq!(
            <super::SoundInterruptModeRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.InterruptModeRequest"
        );
        assert_eq!(
            <super::SoundVolumeRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.VolumeRequest"
        );
        assert_eq!(
            <super::SoundLoopRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.LoopRequest"
        );
        assert_eq!(
            <super::SoundRateRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.RateRequest"
        );
        assert_eq!(
            <super::SoundPriorityRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.PriorityRequest"
        );
        assert_eq!(
            <super::SoundUnloadRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.UnloadRequest"
        );
        assert_eq!(
            <super::SoundReleaseRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.ReleaseRequest"
        );
        assert_eq!(
            <super::SoundAckResponse as BridgeNapiType>::TYPE_NAME,
            "ohos.play-notification.AckResponse"
        );
    }

    #[test]
    fn rejects_invalid_play_parameters() {
        assert!(SoundPlayParameters::default().into_request(1).is_ok());
        assert!(SoundPlayParameters {
            left_volume: Some(1.5),
            ..Default::default()
        }
        .into_request(1)
        .is_err());
        assert!(SoundPlayParameters {
            right_volume: Some(f64::NAN),
            ..Default::default()
        }
        .into_request(1)
        .is_err());
        assert!(SoundPlayParameters {
            rate: Some(3),
            ..Default::default()
        }
        .into_request(1)
        .is_err());
        assert!(SoundPlayParameters {
            priority: Some(-1),
            ..Default::default()
        }
        .into_request(1)
        .is_err());
    }

    #[test]
    fn validates_sound_sources() {
        assert!(SoundSource::Rawfile("sounds/done.wav".to_owned())
            .into_request()
            .is_ok());
        assert!(SoundSource::Rawfile(" ".to_owned()).into_request().is_err());
        assert!(SoundSource::File("/data/x.wav".to_owned())
            .into_request()
            .is_ok());
        assert!(SoundSource::File(String::new()).into_request().is_err());
        assert!(SoundSource::Fd {
            fd: 3,
            offset: 0,
            length: 128,
        }
        .into_request()
        .is_ok());
        assert!(SoundSource::Fd {
            fd: -1,
            offset: 0,
            length: 128,
        }
        .into_request()
        .is_err());
        assert!(SoundSource::Fd {
            fd: 3,
            offset: 0,
            length: 0,
        }
        .into_request()
        .is_err());
    }

    #[test]
    fn validates_pool_and_stream_parameters() {
        assert!(validate_create(4, 7, 0).is_ok());
        assert!(validate_create(0, 7, 0).is_err());
        assert!(validate_create(4, -1, 0).is_err());
        assert!(validate_create(4, 7, -1).is_err());
        assert!(validate_volume("volume", 0.0).is_ok());
        assert!(validate_volume("volume", 1.0).is_ok());
        assert!(validate_volume("volume", -0.1).is_err());
        assert!(validate_rate(0).is_ok());
        assert!(validate_rate(2).is_ok());
        assert!(validate_rate(3).is_err());
        assert!(validate_priority(0).is_ok());
        assert!(validate_priority(-1).is_err());
        assert!(validate_interrupt_mode(0).is_ok());
        assert!(validate_interrupt_mode(1).is_ok());
        assert!(validate_interrupt_mode(2).is_err());
    }
}
