# 1.0.0-beta.1

- Initial release: typed `ohos.openbysys` plugin handing an absolute local path to the system,
  with two actions:
  - `open-file` dispatches an `ohos.want.action.viewData` want so the application registered
    for the file's type opens it (`startAbility` with read/write URI grants, no explicit type);
  - `reveal-file` opens the `filemanager://openDirectory` link the system file manager
    declares (`context.openLink`).
- The ArkTS side owns the path-to-`file://`-URI conversion; Rust carries the absolute path
  and an acknowledgement.
- Requires `ability` context; both actions are async and callable from any Rust thread.

---
