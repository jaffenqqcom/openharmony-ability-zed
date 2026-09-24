//! Main-window visibility control driven from the native side.
//!
//! OHOS exposes window minimize/restore only through the ArkTS `window.Window`
//! object, and the NDK window manager has no equivalent (it only offers
//! show/focusable/status-bar calls). The ArkTS ability host therefore hands two
//! closures over, which are kept as threadsafe functions so the warp main thread
//! can invoke them from a thread other than the UI one.

use std::sync::{Arc, LazyLock, RwLock};

use napi_derive_ohos::napi;
use napi_ohos::{
    bindgen_prelude::Function,
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
    Result,
};

/// The ArkTS-side main-window actions.
struct WindowActions {
    minimize: ThreadsafeFunction<(), ()>,
    show_and_focus: ThreadsafeFunction<(), ()>,
}

static WINDOW_ACTIONS: LazyLock<RwLock<Option<Arc<WindowActions>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Registers the ArkTS main-window actions for the current ability session.
///
/// `env` only ties the `Function` lifetimes; it is not read.
#[napi]
pub fn set_window_actions(
    _env: &napi_ohos::Env,
    minimize: Function<'_, (), ()>,
    show_and_focus: Function<'_, (), ()>,
) -> Result<()> {
    log::info!("window_control::set_window_actions: registering the ArkTS main-window actions");
    let minimize = minimize
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;
    let show_and_focus = show_and_focus
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;

    let mut guard = (*WINDOW_ACTIONS)
        .write()
        .map_err(|_| napi_ohos::Error::from_reason("Failed to write WINDOW_ACTIONS"))?;
    // A recreated ability session replaces the previous session's closures; the
    // replaced threadsafe functions are released with the old value.
    guard.replace(Arc::new(WindowActions {
        minimize,
        show_and_focus,
    }));
    Ok(())
}

/// Minimizes the ability's main window.
///
/// Returns `false` when ArkTS has not registered its window actions yet.
pub fn minimize_main_window() -> bool {
    dispatch_window_action("minimize_main_window", |actions| &actions.minimize)
}

/// Brings the ability's main window back from the minimized state and focuses it.
///
/// Returns `false` when ArkTS has not registered its window actions yet.
pub fn show_and_focus_main_window() -> bool {
    dispatch_window_action("show_and_focus_main_window", |actions| {
        &actions.show_and_focus
    })
}

fn dispatch_window_action(
    name: &str,
    select: impl Fn(&WindowActions) -> &ThreadsafeFunction<(), ()>,
) -> bool {
    let actions = match WINDOW_ACTIONS.read() {
        Ok(guard) => guard.clone(),
        Err(error) => {
            log::error!("window_control::{name}: the window actions lock is poisoned: {error}");
            return false;
        }
    };
    let Some(actions) = actions else {
        log::warn!("window_control::{name}: ArkTS has not registered the main-window actions");
        return false;
    };
    log::info!("window_control::{name}: dispatching to ArkTS");
    select(&actions).call(Ok(()), ThreadsafeFunctionCallMode::NonBlocking);
    true
}
