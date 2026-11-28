# 协议桥接插件

这个目录是一个独立的 Codex Proxy 插件实现，使用官方 `gateway-plugin-sdk` 的 HTTP Middleware 边界，在 Core 保持路由、租约、认证、重试和计费职责的前提下，恢复以下请求路径：

- `POST /v1/messages` → GenAI 上游 → Messages 响应
- `POST /v1/responses` → GenAI 上游 → Responses 响应
- JSON 和 SSE 两种返回形态

协议转换核心位于 `backend/bridge-core`。它从 v3.55 的 `gateway-protocol` 转换模块提取而来，插件本身只负责 HTTP 入口、宿主回调和流事件编排。原始 GenAI 端点不会被插件拦截。

## 构建

需要 Rust 1.99.1 或更高版本，以及可访问当前宿主 fork 的 Git 依赖。

```bash
cargo fmt --manifest-path backend/Cargo.toml
cargo test --manifest-path backend/Cargo.toml
cargo build --release --manifest-path backend/Cargo.toml
./package.sh x86_64-unknown-linux-gnu
```

`package.sh` 会构建目标平台二进制，并调用宿主的 `cpr-plugin` 生成安装包。需要先安装与宿主版本匹配的 `cpr-plugin`，也可以通过 `PLUGIN_CLI` 指定其路径。

## 当前边界

- 插件只处理 `/v1/messages` 和 `/v1/responses` 的 POST 请求。
- `previous_response_id` 暂不跨协议续接。带该字段的请求会被拒绝，避免把客户端会话状态错误地交给转换层。
- 目标上游固定为 `genai`，认证仍通过宿主 `verify_client`。插件兼容 `Authorization`、`x-api-key` 和 `x-goog-api-key` 三种入口头。模型执行仍通过 `host.model.execute_stream`。
- 插件不读取密钥，不实现路由、账号调度、额度、重试或计费。
- 当前版本首先覆盖文本、工具调用、思考载荷和 usage 事件。多候选 GenAI 输出会按转换核心的单候选约束拒绝。

后续可以在不修改 Core 的情况下增加更多目标协议、配置化转换策略和针对不同宿主版本的 SDK 适配。

