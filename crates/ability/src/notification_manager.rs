//! System notifications driven from the native side.
//!
//! OHOS publishes notifications only through the ArkTS `@ohos.notificationManager`
//! module, and the NDK exposes no equivalent. The ArkTS ability host therefore
//! hands the publish/dismiss closures over, kept as threadsafe functions so the
//! application's own thread can post notifications from outside the UI one.
//!
//! User activation travels the other way: ArkTS calls
//! [`dispatch_notification_response`] with the activated notification, which is
//! forwarded to the handler the application registered with
//! [`set_notification_response_handler`].

use std::cell::RefCell;
use std::sync::{Arc, LazyLock, RwLock};

use napi_derive_ohos::napi;
use napi_ohos::{
    bindgen_prelude::Function,
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
    Result,
};

/// A button the application offers on a notification.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct NotificationAction {
    /// Identifies the action in [`NotificationResponse::action_id`] when the button is pressed.
    pub id: String,
    /// The button's user-visible label.
    pub title: String,
}

/// A notification the application asks the system to show.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct NotificationRequest {
    /// Stable identity: reposting the same tag replaces the shown notification,
    /// and cancelling by tag removes it.
    pub tag: String,
    /// The notification's headline.
    pub title: String,
    /// Additional text displayed below the title.
    pub body: String,
    /// Buttons offered on the notification; empty when the platform shows none.
    pub actions: Vec<NotificationAction>,
}

/// The application's activation of a shown notification.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct NotificationResponse {
    /// The [`NotificationRequest::tag`] of the activated notification.
    pub tag: String,
    /// The pressed action button, or `None` when the notification body itself
    /// was activated.
    pub action_id: Option<String>,
}

/// The ArkTS-side notification actions.
struct NotificationActions {
    publish: ThreadsafeFunction<NotificationRequest, ()>,
    dismiss: ThreadsafeFunction<String, ()>,
}

static NOTIFICATION_MANAGER_ACTIONS: LazyLock<RwLock<Option<Arc<NotificationActions>>>> =
    LazyLock::new(|| RwLock::new(None));

// Handler invoked when the user activates a notification.
//
// Stored per thread: ArkTS reports activations on the N-API main thread, which is also the
// thread the application registers its handler on when the ability session starts.
thread_local! {
    static NOTIFICATION_RESPONSE_HANDLER: RefCell<Option<Box<dyn FnMut(String, Option<String>)>>> =
        RefCell::new(None);
}

/// Registers the ArkTS notification actions for the current ability session.
///
/// `env` only ties the `Function` lifetimes; it is not read.
#[napi]
pub fn set_notification_manager_actions(
    _env: &napi_ohos::Env,
    publish: Function<'_, NotificationRequest, ()>,
    dismiss: Function<'_, String, ()>,
) -> Result<()> {
    let publish = publish
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;
    let dismiss = dismiss
        .build_threadsafe_function()
        .callee_handled::<true>()
        .build()?;

    let mut guard = (*NOTIFICATION_MANAGER_ACTIONS)
        .write()
        .map_err(|_| napi_ohos::Error::from_reason("Failed to write NOTIFICATION_MANAGER_ACTIONS"))?;
    // A recreated ability session replaces the previous session's closures; the
    // replaced threadsafe functions are released with the old value.
    guard.replace(Arc::new(NotificationActions { publish, dismiss }));
    Ok(())
}

/// Registers the handler that user activations are delivered to.
///
/// A later registration replaces the previous handler.
pub fn set_notification_response_handler(handler: impl FnMut(String, Option<String>) + 'static) {
    NOTIFICATION_RESPONSE_HANDLER.with(|cell| *cell.borrow_mut() = Some(Box::new(handler)));
}

/// Posts a notification to the system notification center.
///
/// Returns `false` when ArkTS has not registered its notification actions yet.
pub fn publish_notification(request: NotificationRequest) -> bool {
    let actions = match NOTIFICATION_MANAGER_ACTIONS.read() {
        Ok(guard) => guard.clone(),
        Err(error) => {
            log::error!(
                "notification_manager::publish_notification: the notification actions lock is poisoned: {error}"
            );
            return false;
        }
    };
    let Some(actions) = actions else {
        log::warn!(
            "notification_manager::publish_notification: ArkTS has not registered the notification actions"
        );
        return false;
    };
    actions
        .publish
        .call(Ok(request), ThreadsafeFunctionCallMode::NonBlocking);
    true
}

/// Removes a posted notification from the notification center.
///
/// Returns `false` when ArkTS has not registered its notification actions yet.
pub fn cancel_notification(tag: &str) -> bool {
    let actions = match NOTIFICATION_MANAGER_ACTIONS.read() {
        Ok(guard) => guard.clone(),
        Err(error) => {
            log::error!(
                "notification_manager::cancel_notification: the notification actions lock is poisoned: {error}"
            );
            return false;
        }
    };
    let Some(actions) = actions else {
        log::warn!(
            "notification_manager::cancel_notification: ArkTS has not registered the notification actions"
        );
        return false;
    };
    actions
        .dismiss
        .call(Ok(tag.to_owned()), ThreadsafeFunctionCallMode::NonBlocking);
    true
}

/// Forwards a user activation reported by ArkTS to the registered handler.
///
/// Called from ArkTS when the user taps a notification.
#[napi]
pub fn dispatch_notification_response(response: NotificationResponse) {
    NOTIFICATION_RESPONSE_HANDLER.with(|cell| match cell.borrow_mut().as_mut() {
        Some(handler) => handler(response.tag, response.action_id),
        None => log::warn!(
            "notification_manager::dispatch_notification_response: the application registered no response handler"
        ),
    });
}
