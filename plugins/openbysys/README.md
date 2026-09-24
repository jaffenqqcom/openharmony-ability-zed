# @ohos-rs/ability-plugin-openbysys

这是「把本地路径交给系统」能力的 ArkTS HAR，对应 Rust crate
`openharmony-ability-plugin-openbysys`。它在 Ability context 上把本地路径转成 `file://` URI，然后：

- `open-file` 用隐式 Want（`ohos.want.action.viewData` + 读写授权 flag）拉起注册了该文件类型的
  应用；
- `reveal-file` 用 `context.openLink` 打开系统文件管理器声明的 `filemanager://openDirectory`
  链接。

## Install

```bash
ohpm install @ohos-rs/ability-plugin-openbysys
```

## 装配

```json5
{
  "dependencies": {
    "@ohos-rs/ability": "1.0.0-beta.2",
    "@ohos-rs/ability-plugin-openbysys": "1.0.0-beta.1"
  }
}
```

```ts
import { LazyPlugin, NativeAbility } from "@ohos-rs/ability";
import { OpenBySysPlugin } from "@ohos-rs/ability-plugin-openbysys";

export default class EntryAbility extends NativeAbility {
  public bridgePlugins = [new LazyPlugin(() => new OpenBySysPlugin())];
}
```

Rust 侧还需注册 `OpenBySysBridgePlugin`，并通过 `OpenBySysExt::open_file` /
`OpenBySysExt::reveal_in_file_manager` 发起调用。使用示例见
[Rust facade README](../../crates/plugin-openbysys/README.md)。

## Plugin 契约

| 字段 | 值 |
| --- | --- |
| `id` | `ohos.openbysys` |
| `execution` | `async` |
| `requires` | `["ability"]` |
| 支持 action | `open-file`、`reveal-file` |
| `open-file` | `ohos.openbysys.OpenRequest { path }` → `ohos.openbysys.OpenResponse { accepted }` |
| `reveal-file` | `ohos.openbysys.RevealRequest { path }` → `ohos.openbysys.RevealResponse { accepted }` |

## 行为

- `path` 必须是绝对本地路径；空字符串或相对路径直接抛错。
- `open-file` 不携带 `type`：由系统按 URI 后缀推断文件类型并匹配应用（显式传 type 必须与文件类型
  一致，否则匹配不到）。
- path 到 URI 的转换通过 `@ohos.file.fileuri` 的 `getUriFromPath` 完成；系统没有可处理该文件的
  应用、`openLink` 失败或 Ability 在调用期间销毁时，Promise 以明确错误结束。
- request/response 是具名 N-API object，禁止 JSON transport。

完整线程、生命周期与契约变更规则见
[插件开发规范](../../docs/plugin-development-standard.md)。
