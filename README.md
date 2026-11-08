# codex-notify

> Codex 每完成一个回合，把结果推到你的 QQ。

一个很小的 Rust 程序（约 0.7 MB、单文件、无运行时依赖），挂在 Codex 的 `notify` 钩子上。当 Codex 干完活、等你回话的时候，你手机上会收到一条这样的消息：

```
[Codex] 任务完成 · notify
13:16 完成 · 5h 已用 62% · 周 已用 18%
---
已把 notify 钩子接到 QQ 接口，测试通过。
```

比“盯着终端等”省事，也比“跑完忘了回来看”安全。

---

## 它是怎么工作的

Codex 的 `config.toml` 里有个 `notify` 配置项：每次回合结束，Codex 会拉起这个程序，并把事件 JSON 作为**最后一个命令行参数**传进来。所以它是个一次性进程——启动、发一条消息、退出。

```
Codex 回合结束
      │  按 config.toml 里的 notify 数组拉起子进程
      ▼
codex-notify.exe '{"type":"agent-turn-complete","cwd":"...","thread-id":"..."}'
      │
      ├─ chain：把原始 JSON 原样转给「原本占用 notify 槽位」的程序（见下方说明）
      │
      └─ QQ：POST http://127.0.0.1:3000/send_private_msg
                    │
                    ▼
              NapCat（用你登录的 QQ 号发出去）
                    │
                    ▼
                  你的手机
```

**它不实现 QQ 协议，也不直接连腾讯服务器。** 真正的发送方是 [NapCat](https://github.com/NapNeko/NapCatQQ)（或 LLOneBot 等任何实现了 OneBot v11 HTTP 接口的程序）。本程序只负责「解析事件 → 拼消息 → 发一个本地 HTTP 请求」。

### 关于 chain：为什么需要它

Codex 的 `notify` **全局只有一个槽位**。而 Codex 桌面版默认把这个位置用于桌宠动画（`codex-computer-use.exe turn-ended`）。直接覆盖会让桌宠不再响应。

所以本程序支持链式转发：把原命令配到 `chain` 里，收到事件后先原样转发过去，再发我们自己的通知。这样两边共存，互不影响。

如果你的 Codex 没占用这个槽位（比如纯 CLI 环境），`chain = []` 即可。

---

## 前置条件

| 需要 | 说明 |
|---|---|
| **Codex** | CLI 或桌面版均可，需要能改 `~/.codex/config.toml` |
| **Rust** | 编译用，1.70+ 应该都行（开发时用的是 1.99） |
| **NapCat**（或同类） | 提供 OneBot v11 的 HTTP 接口，且已登录一个 QQ 号 |

NapCat 的安装不在本项目范围内，官方文档见 <https://napneko.github.io/>。要点是：装好后在 WebUI 的「网络配置 → HTTP 服务器」里启用一个 HTTP 服务，记下端口和 token。

---

## 安装

**1. 编译**

```bash
git clone https://github.com/<you>/codex-notify.git
cd codex-notify
cargo build --release
```

产物在 `target/release/codex-notify.exe`，可以拷到任意稳定目录（示例里放在 `~/.codex/bin/`）。

**2. 写配置**

```bash
codex-notify.exe --init     # 在 exe 旁边生成 notify.toml
```

然后改三处：

```toml
[qq]
base_url = "http://127.0.0.1:3000"   # NapCat 的 HTTP 地址
access_token = ""                     # 和 NapCat 里配的一致
target = "private:123456789"          # 你的 QQ 号
```

**3. 接管 Codex 的 notify 槽位**

编辑 `~/.codex/config.toml`，加一行（**注意是顶层配置，不是 `[tui]` 下面**）：

```toml
notify = ['C:\Users\you\.codex\bin\codex-notify.exe']
```

如果原来这里已经有别的程序，把它移到 `notify.toml` 的 `chain` 里，别直接删掉：

```toml
chain = ['C:\...\codex-computer-use.exe', 'turn-ended']
```

**4. 验证**

```bash
codex-notify.exe --check     # 检查配置 + 探测 NapCat 是否在线
codex-notify.exe --test      # 真发一条到手机
```

`--check` 会打印当前登录的 QQ 号（通过 NapCat 的 `get_login_info` 接口），能对上就说明链路通了。

---

## 配置项

配置文件默认放在 exe 旁边。也支持用环境变量 `CODEX_NOTIFY_DIR` 指定目录；从 `target/release` 里直接运行时，会自动往上两级找。

| 配置项 | 默认值 | 说明 |
|---|---|---|
| `enabled` | `true` | 总开关。`false` 时钩子直接退出，不发消息 |
| `max_chars` | `260` | 摘要截断长度（按**字数**不是字节） |
| `timeout_secs` | `5` | HTTP 超时。这个值决定回合结束后最多卡多久 |
| `prefix` | `"[Codex]"` | 消息标题前缀 |
| `show_limits` | `true` | 是否带 5h / 周额度 |
| `show_footer` | `true` | 是否带末尾的 `(turn N · thread XXXXXXXX)` |
| `chain` | `[]` | 链式转发的原 notify 命令（数组，第一个元素是程序路径） |
| `[qq].base_url` | `http://127.0.0.1:3000` | NapCat 的 HTTP 服务地址 |
| `[qq].access_token` | `""` | OneBot 鉴权 token，空则不带头 |
| `[qq].target` | — | `private:QQ号` 或 `group:群号` |

---

## 命令行

| 命令 | 作用 |
|---|---|
| `codex-notify.exe '<JSON>'` | 由 Codex 调用，正常路径 |
| `--test` | 用一条假事件真发一条，验证整条链路 |
| `--dry-run` | 只打印将要发送的内容，不发请求 |
| `--check` | 检查配置解析 + 探测 NapCat 连通性 |
| `--init` | 生成默认配置（已存在不覆盖） |
| `--help` | 帮助 |

运行时日志写在配置同目录的 `notify.log`，超过 256 KB 自动轮转成 `notify.log.1`。

---

## 额度是从哪来的

事件 JSON 里没有额度信息。Codex 每次收尾会把快照写进会话日志：

```
~/.codex/sessions/2026/11/08/rollout-...-<thread-id>.jsonl
```

里面有一条 `payload.rate_limits`：

```json
{"primary":  {"used_percent":62.0,"window_minutes":300},
 "secondary":{"used_percent":18.0,"window_minutes":10080}}
```

`300` 分钟 = 5 小时窗口，`10080` 分钟 = 7 天。本程序按 `thread-id` 匹配到对应文件，**只读文件末尾 512 KB** 找最后一条记录，耗时 5–10 ms，不需要任何网络请求或凭据。

注意：这里**只读** `~/.codex`，不会写入任何东西。不想要这块就设 `show_limits = false`。

---

## 常见问题

**收不到消息？**

1. 跑 `--check`，看「连通性」那行。失败的话依次检查：NapCat 是否在运行、HTTP 服务器是否启用、端口/token 是否一致。
2. `--check` 通过但收不到：跑 `--test`，然后看 `notify.log` 里的记录。
3. 都正常但还是没反应：确认 `~/.codex/config.toml` 里的 `notify` 写在**顶层**，并且路径是绝对路径。

**桌宠不动了？** `chain` 没配。把原来那条 `codex-computer-use.exe turn-ended` 加回去。

**想临时关掉？** 把 `[qq].target` 清空即可，不用删配置也不用改 `config.toml`。

**回合结束后感觉卡了一下？** 因为 Codex 会等这个进程退出。本机 NapCat 通常 5 ms 就返回，但如果你把 `base_url` 指向了远程地址，就会明显变慢。可以调小 `timeout_secs`。

**消息会不会把代码发出去？** 不会。只发三样：工作目录的**最后一段**目录名、Codex 最后一条回复的前 `max_chars` 个字、以及额度百分比。不发完整路径、不发文件内容、不发你的输入。

---

## 设计取舍

- **不用异步运行时**：这是一次性进程，`ureq` 阻塞式发一个请求就够了，引入 tokio 只会让启动变慢、二进制变大。
- **所有失败都静默**：配置解析失败、网络不通、NapCat 没开，一律只写日志并正常退出。钩子跑在 Codex 的收尾流程里，这里 panic 会让 Codex 报错，代价远大于少发一条通知。
- **不往 stdout 写东西**：运行时输出会混进 Codex 的流里。除了人工命令，其余信息全部落文件。
- **不碰 `~/.codex` 的写操作**：只读会话日志取额度。Codex 的配置目录里只留 `config.toml` 那一行 `notify`，那是它唯一的入口。

---

## 限制

- 在 Windows 上开发和验证，代码本身是跨平台的，但其他平台未测试。
- 依赖 NapCat 常驻。NapCat 退出、掉线，或者 QQ 自动更新导致注入失效，通知都会中断（`--check` 能看出来）。
- NapCat 是第三方对 QQ 客户端的协议实现，属于灰色地带，账号有被风控的可能。请自行评估，建议用小号。

---

## License

[MIT](LICENSE)
