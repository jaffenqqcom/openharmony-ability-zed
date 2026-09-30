//! Idle-sleep prevention driven from the native side.
//!
//! Holding the screen awake is ArkTS-only: it goes through the window-level
//! `setWindowKeepScreenOn`, and the NDK exposes no equivalent. The ArkTS ability
//! host therefore hands the acquire/release closures over, kept as threadsafe
//! functions so the application's own thread can drive them from outside the UI
//! one.
//!
//! (`@ohos.runningLock` cannot serve this: `RunningLockType.BACKGROUND_USER_IDLE`
//! is reported unsupported by the target device, and the remaining types are
//! either deprecated or tied to the proximity sensor.)
//!
//! The application holds at most one awake state: the first [`RunningLockGuard`]
//! acquired turns it on and the last one dropped turns it off, so concurrent
//! requests (several running agent threads) share a single window state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use napi_derive_ohos::napi;
use napi_ohos::{
    bindgen_prelude::Function,
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
    Result,
};

/// The ArkTS-side running-lock actions.
struct RunningLockActions {
    acquire: ThreadsafeFunction<String, ()>,
    release: ThreadsafeFunction<(), ()>,
}

static RUNNING_LOCK_ACTIONS: LazyLock<RwLock<Option<Arc<RunningLockActions>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Live guards. The system lock is taken when the count rises from zero and
/// released when it falls back to zero.
static ACTIVE_GUARDS: AtomicUsize = AtomicUsize::new(0);

/// Registers the ArkTS running-lock actions for the current ability session.
///
/// `env` only ties the `Function` lifetimes; it is not read.
#[napi]
pub fn set_running_lock_actions(
    _env: &napi_ohos::Env,
    acquire: Function<'_, String, ()>,
    release: Function<'_, (), ()>,
) -> Result<()> {
    let acquire = acquire
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;
    let release = release
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;

    let mut guard = (*RUNNING_LOCK_ACTIONS)
        .write()
        .map_err(|_| napi_ohos::Error::from_reason("Failed to write RUNNING_LOCK_ACTIONS"))?;
    // A recreated ability session replaces the previous session's closures; the
    // replaced threadsafe functions are released with the old value.
    guard.replace(Arc::new(RunningLockActions { acquire, release }));
    Ok(())
}

/// Keeps the system out of idle sleep until the returned guard is dropped.
///
/// Fails only when ArkTS has not registered its running-lock actions yet.
pub fn acquire_running_lock(reason: &str) -> Result<RunningLockGuard> {
    let actions = running_lock_actions()?;
    if ACTIVE_GUARDS.fetch_add(1, Ordering::SeqCst) == 0 {
        actions
            .acquire
            .call(Ok(reason.to_owned()), ThreadsafeFunctionCallMode::NonBlocking);
    }
    Ok(RunningLockGuard)
}

/// Releases the system lock once every guard has been dropped.
pub struct RunningLockGuard;

impl Drop for RunningLockGuard {
    fn drop(&mut self) {
        if ACTIVE_GUARDS.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        match RUNNING_LOCK_ACTIONS.read() {
            Ok(guard) => match guard.as_ref() {
                Some(actions) => {
                    actions
                        .release
                        .call(Ok(()), ThreadsafeFunctionCallMode::NonBlocking);
                }
                None => log::warn!(
                    "running_lock::RunningLockGuard::drop: ArkTS has not registered the running-lock actions"
                ),
            },
            Err(error) => log::error!(
                "running_lock::RunningLockGuard::drop: the running-lock actions lock is poisoned: {error}"
            ),
        }
    }
}

fn running_lock_actions() -> Result<Arc<RunningLockActions>> {
    match RUNNING_LOCK_ACTIONS.read() {
        Ok(guard) => guard.clone().ok_or_else(|| {
            napi_ohos::Error::from_reason("ArkTS has not registered the running-lock actions yet")
        }),
        Err(_) => Err(napi_ohos::Error::from_reason(
            "The running-lock actions lock is poisoned",
        )),
    }
}
