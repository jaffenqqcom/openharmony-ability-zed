//! Ability lifecycle control driven from the native side.
//!
//! Finishing an ability is ArkTS-only: the host calls
//! `UIAbilityContext.terminateSelf()`, and the NDK exposes no equivalent. The
//! ArkTS ability host therefore hands a closure over, which is kept as a
//! threadsafe function so the application's own thread can invoke it from
//! outside the UI one.

use std::sync::{Arc, LazyLock, RwLock};

use napi_derive_ohos::napi;
use napi_ohos::{
    bindgen_prelude::Function,
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
    Result,
};

/// The ArkTS-side ability actions.
struct AbilityActions {
    terminate: ThreadsafeFunction<(), ()>,
}

static ABILITY_ACTIONS: LazyLock<RwLock<Option<Arc<AbilityActions>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Registers the ArkTS ability actions for the current ability session.
///
/// `env` only ties the `Function` lifetimes; it is not read.
#[napi]
pub fn set_ability_actions(_env: &napi_ohos::Env, terminate: Function<'_, (), ()>) -> Result<()> {
    log::info!("ability_control::set_ability_actions: registering the ArkTS ability actions");
    let terminate = terminate
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;

    let mut guard = (*ABILITY_ACTIONS)
        .write()
        .map_err(|_| napi_ohos::Error::from_reason("Failed to write ABILITY_ACTIONS"))?;
    // A recreated ability session replaces the previous session's closure; the
    // replaced threadsafe function is released with the old value.
    guard.replace(Arc::new(AbilityActions { terminate }));
    Ok(())
}

/// Finishes the ability, so the application closes instead of staying alive
/// without a window.
///
/// Returns `false` when ArkTS has not registered its ability actions yet.
pub fn terminate_ability() -> bool {
    let actions = match ABILITY_ACTIONS.read() {
        Ok(guard) => guard.clone(),
        Err(error) => {
            log::error!(
                "ability_control::terminate_ability: the ability actions lock is poisoned: {error}"
            );
            return false;
        }
    };
    let Some(actions) = actions else {
        log::warn!(
            "ability_control::terminate_ability: ArkTS has not registered the ability actions"
        );
        return false;
    };
    log::info!("ability_control::terminate_ability: dispatching to ArkTS");
    actions
        .terminate
        .call(Ok(()), ThreadsafeFunctionCallMode::NonBlocking);
    true
}
