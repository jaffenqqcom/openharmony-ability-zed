# openharmony-ability-plugin-openbysys

`openharmony-ability-plugin-openbysys` 提供把绝对本地路径交给系统的 Rust facade：`open-file` 用
注册了该文件类型的应用打开，`reveal-file` 在系统文件管理器中定位。它与 ArkTS HAR
`@ohos-rs/ability-plugin-openbysys` 成对使用，是异步插件，可从 Rust worker 调用。

## 契约

| 项目 | 值 |
| --- | --- |
| Rust crate | `openharmony-ability-plugin-openbysys` |
| ArkTS HAR | `@ohos-rs/ability-plugin-openbysys` |
| 插件 ID | `ohos.openbysys` |
| 执行模式 | 异步：`AsyncBridge` / `invokeAsync` |
| 前置 context | `ability` |
| action | `open-file`、`reveal-file` |
| `open-file` | `ohos.openbysys.OpenRequest { path }` → `ohos.openbysys.OpenResponse { accepted }` |
| `reveal-file` | `ohos.openbysys.RevealRequest { path }` → `ohos.openbysys.RevealResponse { accepted }` |

## 接入

Rust facade 和 ArkTS factory 都必须在应用启动期装配：

```rust
use openharmony_ability::OpenHarmonyApp;
use openharmony_ability_derive::ability;
use openharmony_ability_plugin_openbysys::OpenBySysBridgePlugin;

#[ability]
fn configure_ability(app: OpenHarmonyApp) {
    app.register_plugin(OpenBySysBridgePlugin)
        .expect("openbysys facade must be registered once");
}
```

```ts
import { LazyPlugin, NativeAbility } from "@ohos-rs/ability";
import { OpenBySysPlugin } from "@ohos-rs/ability-plugin-openbysys";

export default class EntryAbility extends NativeAbility {
  public bridgePlugins = [new LazyPlugin(() => new OpenBySysPlugin())];
}
```

同时在应用 `oh-package.json5` 添加 `@ohos-rs/ability-plugin-openbysys`。HAR 的具体依赖和 plugin
说明见 [ArkTS README](../../plugins/openbysys/README.md)。

## Rust 使用方式

两个 facade 都是异步的，可在 Rust worker 中调用；`path` 必须是绝对本地路径，URI 形式会被拒绝：

```rust
use openharmony_ability_plugin_openbysys::OpenBySysExt;

app.open_file("/storage/Users/currentUser/Documents/notes.txt").await?;
app.reveal_in_file_manager("/storage/Users/currentUser/Documents").await?;
```

用哪个 action 属于应用策略：facade 分开暴露，不替调用方决定。

## 线程与生命周期限制

- 异步 action：request/response 必须是 `Send + 'static` 的 Rust 所有权数据，经 TSFN 传输；不保存
  `Env`、`napi_value` 或 ArkTS 对象到 worker。
- path 到 `file://` URI 的转换由 ArkTS 插件完成；系统找不到可处理该文件的应用、转换失败或
  Ability 销毁时返回明确错误。
- 参数与结果都是具名 N-API object，不存在 JSON transport。

完整规范见 [插件开发规范](../../docs/plugin-development-standard.md)。
