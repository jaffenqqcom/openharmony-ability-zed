//! OHOS global hotkeys via the NDK C API.
//!
//! Wraps `libohinput` (`OH_Input_*Hotkey*`) so Rust can subscribe to a system
//! shortcut directly, without the ArkTS `@ohos.multimodalInput.inputConsumer`
//! bridge. The hotkey C API is available since API 14 and carries no permission
//! gate (`multimodalinput/oh_input_manager.h`).
//!
//! A hotkey is a non-empty list of modifier keys plus exactly one final key;
//! the system reports the combination even while another application holds
//! focus. Every subscription is registered with a trampoline entry point of its
//! own, because the `Input_Hotkey` the service hands to a callback cannot be
//! read back — `OH_Input_GetPreKeys` answers `INPUT_PARAMETER_ERROR` on the
//! object the callback receives — so the entry point is what identifies the
//! subscription that fired. The application installs its own handler through
//! [`set_hotkey_triggered_handler`].

use std::ffi::{c_int, c_void};
use std::sync::Mutex;

/// Modifier keys a hotkey may combine. The system accepts at most two.
pub const MAX_PRE_KEYS: usize = 2;

/// Hotkeys this module can subscribe to at once.
///
/// Every subscription is registered with its own trampoline entry point, so
/// this is also the number of trampolines defined below.
pub const MAX_SUBSCRIPTIONS: usize = 8;

/// The operation succeeded.
pub const INPUT_SUCCESS: i32 = 0;
/// The application lacks the permission the operation needs.
pub const INPUT_PERMISSION_DENIED: i32 = 201;
/// An argument was malformed.
pub const INPUT_PARAMETER_ERROR: i32 = 401;
/// The device has no keyboard capable of the combination.
pub const INPUT_DEVICE_NOT_SUPPORTED: i32 = 801;
/// The combination is reserved by the system.
pub const INPUT_OCCUPIED_BY_SYSTEM: i32 = 4200002;
/// The combination is already subscribed by this or another application.
pub const INPUT_OCCUPIED_BY_OTHER: i32 = 4200003;

/// libohinput.so (API 14+).
#[link(name = "ohinput")]
unsafe extern "C" {
    fn OH_Input_CreateHotkey() -> *mut c_void;
    fn OH_Input_DestroyHotkey(hotkey: *mut *mut c_void);
    fn OH_Input_SetPreKeys(hotkey: *mut c_void, pre_keys: *mut c_int, size: c_int);
    fn OH_Input_SetFinalKey(hotkey: *mut c_void, final_key: c_int);
    fn OH_Input_SetRepeat(hotkey: *mut c_void, is_repeat: bool);
    fn OH_Input_AddHotkeyMonitor(hotkey: *const c_void, callback: HotkeyCallback) -> c_int;
    fn OH_Input_RemoveHotkeyMonitor(hotkey: *const c_void, callback: HotkeyCallback) -> c_int;
}

/// `typedef void (*Input_HotkeyCallback)(Input_Hotkey *hotkey);`
type HotkeyCallback = extern "C" fn(*mut c_void);

/// An `Input_Hotkey` pointer the subscription registry owns.
struct HotkeyPtr(*mut c_void);

// SAFETY: the pointer is only ever handed back to the libohinput APIs, which
// address the hotkey service by IPC and impose no thread affinity, and it is
// never dereferenced on the Rust side.
unsafe impl Send for HotkeyPtr {}

/// A live subscription, including the `Input_Hotkey` object it was created
/// from.
///
/// The object is kept until the subscription is removed: the monitor may
/// reference it for as long as it is registered, so destroying it right after
/// `OH_Input_AddHotkeyMonitor` could leave the service with a dangling pointer.
struct Subscription {
    pre_keys: Vec<i32>,
    final_key: i32,
    hotkey: HotkeyPtr,
}

impl Subscription {
    fn matches(&self, pre_keys: &[i32], final_key: i32) -> bool {
        self.final_key == final_key && self.pre_keys == pre_keys
    }
}

/// The live subscriptions, indexed by the trampoline slot they were registered
/// with.
struct Subscriptions {
    slots: [Option<Subscription>; MAX_SUBSCRIPTIONS],
}

impl Subscriptions {
    const fn new() -> Self {
        Self {
            slots: [const { None }; MAX_SUBSCRIPTIONS],
        }
    }

    /// The slot `pre_keys` + `final_key` is subscribed in, if any.
    fn find(&self, pre_keys: &[i32], final_key: i32) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.as_ref()
                .is_some_and(|subscription| subscription.matches(pre_keys, final_key))
        })
    }

    /// The first unused slot, if any trampoline is still free.
    fn first_free(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }
}

/// Handler invoked when a subscribed hotkey fires, with the modifier keys and
/// the final key that were registered.
type HotkeyTriggeredHandler = Box<dyn Fn(&[i32], i32) + Send + Sync>;

static SUBSCRIPTIONS: Mutex<Subscriptions> = Mutex::new(Subscriptions::new());
static HANDLER: Mutex<Option<HotkeyTriggeredHandler>> = Mutex::new(None);

/// Defines the trampoline each subscription slot registers with.
///
/// One function per slot is what makes the fired subscription identifiable, so
/// the number of entries has to match [`MAX_SUBSCRIPTIONS`] — the array below
/// fails to compile otherwise.
macro_rules! define_trampolines {
    ($($trampoline:ident = $slot:expr),+ $(,)?) => {
        $(
            extern "C" fn $trampoline(hotkey: *mut c_void) {
                dispatch_fired($slot, hotkey);
            }
        )+

        /// The entry point each subscription slot registers with.
        const TRAMPOLINES: [HotkeyCallback; MAX_SUBSCRIPTIONS] = [$($trampoline),+];
    };
}

define_trampolines!(
    hotkey_slot_0 = 0,
    hotkey_slot_1 = 1,
    hotkey_slot_2 = 2,
    hotkey_slot_3 = 3,
    hotkey_slot_4 = 4,
    hotkey_slot_5 = 5,
    hotkey_slot_6 = 6,
    hotkey_slot_7 = 7,
);

/// Reports a fire on `slot` to the installed handler.
fn dispatch_fired(slot: usize, hotkey: *mut c_void) {
    let keys = match SUBSCRIPTIONS.lock() {
        Ok(subscriptions) => subscriptions
            .slots
            .get(slot)
            .and_then(|slot| slot.as_ref())
            .map(|subscription| (subscription.pre_keys.clone(), subscription.final_key)),
        Err(error) => {
            log::error!("hotkey::dispatch_fired: cannot lock the registry: {error}");
            None
        }
    };

    let Some((pre_keys, final_key)) = keys else {
        // A fire whose slot was freed while the event was in flight has nothing
        // to report.
        log::warn!("hotkey::dispatch_fired: slot {slot} fired with no subscription");
        return;
    };

    log::info!(
        "hotkey::dispatch_fired: slot={slot} pre_keys={pre_keys:?} final_key={final_key} \
         hotkey={hotkey:p}"
    );

    match HANDLER.lock() {
        Ok(handler) => match handler.as_ref() {
            Some(handler) => handler(&pre_keys, final_key),
            None => log::warn!(
                "hotkey::dispatch_fired: pre_keys={pre_keys:?} final_key={final_key} fired \
                 before a handler was installed"
            ),
        },
        Err(error) => log::error!("hotkey::dispatch_fired: cannot lock the handler: {error}"),
    }
}

/// Installs the handler every subscribed hotkey reports through.
///
/// Replaces the handler installed earlier, so a restarted application layer
/// does not have to unregister the hotkeys first; the subscriptions themselves
/// stay valid because the callback the system holds is this module's
/// trampoline, not the handler.
pub fn set_hotkey_triggered_handler(handler: HotkeyTriggeredHandler) {
    match HANDLER.lock() {
        Ok(mut slot) => {
            log::info!("hotkey::set_hotkey_triggered_handler: replacing the hotkey handler");
            *slot = Some(handler);
        }
        Err(error) => {
            log::error!("hotkey::set_hotkey_triggered_handler: cannot lock the handler: {error}");
        }
    }
}

/// Subscribes to `pre_keys` + `final_key`.
///
/// Subscribing to a combination that is already subscribed by this process is a
/// no-op, so a repeated request reports success rather than
/// `INPUT_OCCUPIED_BY_OTHER`.
///
/// # Errors
///
/// Returns the raw `Input_Result` code when the system rejects the
/// subscription, e.g. [`INPUT_OCCUPIED_BY_SYSTEM`] for a reserved
/// combination or [`INPUT_DEVICE_NOT_SUPPORTED`] when the attached
/// keyboard cannot produce it.
pub fn register_hotkey(pre_keys: &[i32], final_key: i32) -> Result<(), i32> {
    log::info!("hotkey::register_hotkey: pre_keys={pre_keys:?} final_key={final_key}");

    if pre_keys.is_empty() || pre_keys.len() > MAX_PRE_KEYS {
        log::error!(
            "hotkey::register_hotkey: {} modifier key(s) is outside the supported 1..={MAX_PRE_KEYS}",
            pre_keys.len()
        );
        return Err(INPUT_PARAMETER_ERROR);
    }

    let mut subscriptions = match SUBSCRIPTIONS.lock() {
        Ok(subscriptions) => subscriptions,
        Err(error) => {
            log::error!("hotkey::register_hotkey: cannot lock the registry: {error}");
            return Err(INPUT_PARAMETER_ERROR);
        }
    };

    if subscriptions.find(pre_keys, final_key).is_some() {
        log::debug!("hotkey::register_hotkey: the combination is already subscribed");
        return Ok(());
    }

    let Some(slot) = subscriptions.first_free() else {
        log::error!(
            "hotkey::register_hotkey: all {MAX_SUBSCRIPTIONS} subscription slots are in use"
        );
        return Err(INPUT_PARAMETER_ERROR);
    };

    // SAFETY: every pointer below was created by this call or points at a live
    // local, and `hotkey` is either stored in the registry or destroyed here.
    unsafe {
        let mut hotkey = OH_Input_CreateHotkey();
        if hotkey.is_null() {
            log::error!("hotkey::register_hotkey: OH_Input_CreateHotkey failed");
            return Err(INPUT_PARAMETER_ERROR);
        }

        let mut keys = pre_keys.to_vec();
        OH_Input_SetPreKeys(hotkey, keys.as_mut_ptr(), keys.len() as c_int);
        OH_Input_SetFinalKey(hotkey, final_key);
        // A shortcut fires once per press; the repeat stream is key-level.
        OH_Input_SetRepeat(hotkey, false);

        let add_result = OH_Input_AddHotkeyMonitor(hotkey, TRAMPOLINES[slot]);
        if add_result != INPUT_SUCCESS {
            log::error!("hotkey::register_hotkey: OH_Input_AddHotkeyMonitor failed: {add_result}");
            OH_Input_DestroyHotkey(&mut hotkey);
            return Err(add_result);
        }

        subscriptions.slots[slot] = Some(Subscription {
            pre_keys: keys,
            final_key,
            hotkey: HotkeyPtr(hotkey),
        });
    }

    log::info!("hotkey::register_hotkey: subscribed in slot {slot}");
    Ok(())
}

/// Cancels a subscription created by [`register_hotkey`].
///
/// Unsubscribing a combination that is not subscribed is a no-op and reports
/// success.
///
/// # Errors
///
/// Returns the raw `Input_Result` code when the system rejects the request.
pub fn unregister_hotkey(pre_keys: &[i32], final_key: i32) -> Result<(), i32> {
    log::info!("hotkey::unregister_hotkey: pre_keys={pre_keys:?} final_key={final_key}");

    let mut subscriptions = match SUBSCRIPTIONS.lock() {
        Ok(subscriptions) => subscriptions,
        Err(error) => {
            log::error!("hotkey::unregister_hotkey: cannot lock the registry: {error}");
            return Err(INPUT_PARAMETER_ERROR);
        }
    };

    let Some(slot) = subscriptions.find(pre_keys, final_key) else {
        log::debug!("hotkey::unregister_hotkey: the combination is not subscribed");
        return Ok(());
    };
    let Some(subscription) = subscriptions.slots[slot].take() else {
        // `find` only reports the slots that are occupied.
        log::error!("hotkey::unregister_hotkey: slot {slot} emptied between lookup and take");
        return Err(INPUT_PARAMETER_ERROR);
    };

    // SAFETY: the pointer came from `OH_Input_CreateHotkey`, the subscription
    // is no longer reachable, and `OH_Input_DestroyHotkey` nulls the local.
    let mut hotkey = subscription.hotkey.0;
    let remove_result = unsafe { OH_Input_RemoveHotkeyMonitor(hotkey, TRAMPOLINES[slot]) };
    unsafe { OH_Input_DestroyHotkey(&mut hotkey) };

    if remove_result != INPUT_SUCCESS {
        log::error!(
            "hotkey::unregister_hotkey: OH_Input_RemoveHotkeyMonitor failed: {remove_result}"
        );
        return Err(remove_result);
    }

    log::info!("hotkey::unregister_hotkey: unsubscribed from slot {slot}");
    Ok(())
}
