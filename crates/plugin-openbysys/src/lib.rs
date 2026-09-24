//! System file open capability plugin facade.
//!
//! Two ways of handing an absolute local path to the system:
//!
//! - `open-file` dispatches an implicit want (`ohos.want.action.viewData`) so the app that
//!   registered for the file's type opens it;
//! - `reveal-file` opens the `filemanager://openDirectory` link the system file manager
//!   declares, so the path is shown inside it.
//!
//! Both are resolved in ArkTS, which also owns the path-to-URI conversion, because the
//! platform only accepts the `file://` URI form. Which of the two an application wants is
//! application policy: this facade exposes them separately and never picks on its own.

use std::{future::Future, path::Path, pin::Pin};

use napi_derive_ohos::napi;
use napi_ohos::{Error, Result};
use openharmony_ability::{
    impl_bridge_napi_type, AsyncBridge, BridgeCallOptions, BridgeContextRequirement, BridgePlugin,
    OpenHarmonyApp,
};

pub struct OpenBySysBridgePlugin;

impl BridgePlugin for OpenBySysBridgePlugin {
    type Mode = AsyncBridge;

    const ID: &'static str = "ohos.openbysys";
    const REQUIRED_CONTEXTS: &'static [BridgeContextRequirement] =
        &[BridgeContextRequirement::Ability];
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct OpenRequest {
    /// Absolute local path (not a URI) of the file to open.
    pub path: String,
}

impl_bridge_napi_type!(OpenRequest, "ohos.openbysys.OpenRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct OpenResponse {
    pub accepted: bool,
}

impl_bridge_napi_type!(OpenResponse, "ohos.openbysys.OpenResponse");

impl OpenResponse {
    fn ensure(self) -> Result<()> {
        if self.accepted {
            Ok(())
        } else {
            Err(Error::from_reason(
                "open-by-sys plugin rejected the open request",
            ))
        }
    }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct RevealRequest {
    /// Absolute local path (not a URI) of the file or directory to reveal.
    pub path: String,
}

impl_bridge_napi_type!(RevealRequest, "ohos.openbysys.RevealRequest");

#[napi(object)]
#[derive(Clone, Debug)]
pub struct RevealResponse {
    pub accepted: bool,
}

impl_bridge_napi_type!(RevealResponse, "ohos.openbysys.RevealResponse");

impl RevealResponse {
    fn ensure(self) -> Result<()> {
        if self.accepted {
            Ok(())
        } else {
            Err(Error::from_reason(
                "open-by-sys plugin rejected the reveal request",
            ))
        }
    }
}

fn validate_path(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(Error::from_reason("path must not be empty"));
    }
    if !Path::new(path).is_absolute() {
        return Err(Error::from_reason(
            "path must be an absolute path (e.g. /storage/Users/...)",
        ));
    }
    Ok(())
}

/// Extension trait supplied by the capability package, never by `openharmony-ability` core.
pub trait OpenBySysExt {
    /// Opens an absolute local file path with the application registered for its type.
    ///
    /// The path becomes its `file://` URI form in ArkTS and travels as an implicit want; no
    /// file type is named, so the system resolves it from the suffix.
    fn open_file(&self, path: impl Into<String>) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    /// Reveals an absolute local path in the system file manager.
    fn reveal_in_file_manager(
        &self,
        path: impl Into<String>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;
}

impl OpenBySysExt for OpenHarmonyApp {
    fn open_file(&self, path: impl Into<String>) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        let path = path.into();
        if let Err(error) = validate_path(&path) {
            return Box::pin(async move { Err(error) });
        }
        let bridge = self.bridge();
        Box::pin(async move {
            let response = bridge?
                .call_async::<OpenBySysBridgePlugin, OpenRequest, OpenResponse>(
                    "open-file",
                    OpenRequest { path },
                    BridgeCallOptions::default(),
                )
                .await?;
            response.ensure()
        })
    }

    fn reveal_in_file_manager(
        &self,
        path: impl Into<String>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        let path = path.into();
        if let Err(error) = validate_path(&path) {
            return Box::pin(async move { Err(error) });
        }
        let bridge = self.bridge();
        Box::pin(async move {
            let response = bridge?
                .call_async::<OpenBySysBridgePlugin, RevealRequest, RevealResponse>(
                    "reveal-file",
                    RevealRequest { path },
                    BridgeCallOptions::default(),
                )
                .await?;
            response.ensure()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_path, OpenRequest, OpenResponse, RevealRequest, RevealResponse};
    use openharmony_ability::BridgeNapiType;

    #[test]
    fn openbysys_uses_stable_named_napi_contracts() {
        assert_eq!(
            <OpenRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.openbysys.OpenRequest"
        );
        assert_eq!(
            <OpenResponse as BridgeNapiType>::TYPE_NAME,
            "ohos.openbysys.OpenResponse"
        );
        assert_eq!(
            <RevealRequest as BridgeNapiType>::TYPE_NAME,
            "ohos.openbysys.RevealRequest"
        );
        assert_eq!(
            <RevealResponse as BridgeNapiType>::TYPE_NAME,
            "ohos.openbysys.RevealResponse"
        );
    }

    #[test]
    fn path_must_be_absolute_and_non_empty() {
        assert!(validate_path("/storage/Users/currentUser/Documents").is_ok());
        assert!(validate_path("").is_err());
        assert!(validate_path("   ").is_err());
        assert!(validate_path("relative/dir").is_err());
    }

    #[test]
    fn responses_reject_a_refusal() {
        assert!(OpenResponse { accepted: true }.ensure().is_ok());
        assert!(OpenResponse { accepted: false }.ensure().is_err());
        assert!(RevealResponse { accepted: true }.ensure().is_ok());
        assert!(RevealResponse { accepted: false }.ensure().is_err());
    }
}
