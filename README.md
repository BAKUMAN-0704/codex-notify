<p align="center">
  <img src="assets/icon-circle-512.png" width="112" alt="codex-notify">
</p>

<h1 align="center">codex-notify</h1>

<p align="center">Codex 每完成一个回合，把结果推到你的 QQ。</p>

一个很小的 Rust 程序（约 0.85 MB、单文件、无运行时依赖），挂在 Codex 的 `notify` 钩子上。当 Codex 干完活、等你回话的时候，你手机上会收到一条这样的消息：

```
[Codex] 任务完成 · notify
13:16 完成 · 5h 已用 62% · 周 已用 18%
---
已把 notify 钩子接到 QQ 接口，测试通过。
```

比“盯着终端等”省事，也比“跑完忘了回来看”安全。

**[下载最新版本](https://github.com/BAKUMAN-0704/codex-notify/releases)** · [怎么安装](#安装) · [怎么配置](#配置) · [常见问题](#常见问题)

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

| 需要 | 说明 | 什么时候需要 |
|---|---|---|
| **Codex** | CLI 或桌面版均可，需要能改 `~/.codex/config.toml` | 必需 |
| **NapCat**（或任何 OneBot v11 实现） | 提供 HTTP 接口，且已登录一个 QQ 号 | 必需 |
| **Rust 1.70+** | 只有「自己编译」才需要，下载预编译版本可以跳过 | 可选 |

---

## 安装

两条路，任选一条。产物完全一样，区别只在于要不要装 Rust。

### 方式一：下载预编译版本（推荐）

1. 打开 [**Releases**](https://github.com/BAKUMAN-0704/codex-notify/releases)，在最新版本的 **Assets** 里下载 `codex-notify.exe`
2. 放到**任意一个你喜欢的固定目录**。目录位置完全随你，常见的选择有：

   ```
   C:\Users\<你>\.codex\bin\codex-notify\     # 跟 Codex 配置放一起
   D:\tools\codex-notify\                      # 独立工具目录
   D:\repo\codex-notify\                       # 直接从源码目录跑
   ```

   唯一的要求：**后面 `config.toml` 里写的路径要和它一致**。

   > 为什么强调“固定”：那个路径会被写死在 Codex 的配置里。如果放在下载文件夹或桌面，哪天被清理掉，钩子就静默失效了——Codex 不会报错，只是再也不通知你。

3. 可选：核对文件完整性。Release 说明里有 SHA256，用 PowerShell 算一遍对比：

   ```powershell
   Get-FileHash .\codex-notify.exe -Algorithm SHA256
   ```

单文件、无运行时依赖，约 730 KB，Windows 10/11 x64。

### 方式二：自己编译

需要先装 [Rust](https://rustup.rs/)，装完 `rustc --version` 能输出就行。

```bash
git clone https://github.com/BAKUMAN-0704/codex-notify.git
cd codex-notify
cargo build --release
```

产物在 `target/release/codex-notify.exe`。**别直接在 `target\release\` 里跑**——`cargo clean` 会把它删掉，钩子就断了。复制到你选定的目录：

```powershell
copy target\release\codex-notify.exe D:\tools\codex-notify\      # 换成你的目录
```

> 编译时 `build.rs` 会把 `assets/codex-notify.ico` 嵌进 exe（任务栏和资源管理器里显示的就是它）。这一步需要 Windows SDK 里的 `rc.exe`；如果你的机器上找不到，只会打一条 warning 然后继续，产物正常可用、只是图标是默认的。想手动指定路径就设 `CODEX_NOTIFY_RC_DIR` 指向含 `rc.exe` 的目录。

> 下面的配置步骤，两种方式完全相同。

---

## 配置

分三步，建议按顺序做。每一步都能单独验证，出了问题容易定位。

### 第一步：让 NapCat 能发消息

本程序不直接连腾讯服务器，它只是往本机的一个 HTTP 接口发请求。所以得先把这个接口跑起来。

1. 安装并启动 [NapCat](https://github.com/NapNeko/NapCatQQ)，扫码登录一个 QQ 号
2. 打开 NapCat 的 WebUI（默认 `http://127.0.0.1:6099`）→ **网络配置 → HTTP 服务器** → 新增一个
3. 记下两样东西：

   - **端口**（默认 `3000`）
   - **token**（可以为空，但强烈建议设一个——这个接口能以你的 QQ 身份发消息，裸奔等于本机任何程序都能冒充你）

**验证这一步：** 浏览器访问 <http://127.0.0.1:3000/get_login_info>，能返回 JSON 且里面有你登录的 QQ 号，就说明服务通了。

```json
{"status":"ok","retcode":0,"data":{"user_id":123456789,"nickname":"..."}}
```

打不开的话，先确认 NapCat 在运行、HTTP 服务器是「启用」状态、端口没被占用。

### 第二步：写 notify.toml

在 exe 所在目录执行：

```bash
codex-notify.exe --init
```

会在 exe 旁边生成一份 `notify.toml`（已存在则不覆盖）。改这三个字段，其余保持默认：

| 字段 | 填什么 | 值从哪来 |
|---|---|---|
| `[qq].base_url` | `http://127.0.0.1:3000` | 第一步记下的端口 |
| `[qq].access_token` | 第一步记下的 token | 没设 token 就留空 `""` |
| `[qq].target` | `private:你的QQ号` | 发到群就写 `group:群号` |

改完大概长这样：

```toml
[qq]
base_url = "http://127.0.0.1:3000"
access_token = "你设的token"
target = "private:123456789"
```

> **配置文件放在哪？** 默认在 exe 同目录，日志（`notify.log`）也写在这里。想换位置见下一节。

其余字段的含义见下方[配置项](#配置项)表，一开始不用动。

#### 配置文件与日志放在哪

默认都在 **exe 所在目录**：`notify.toml` 和 `notify.log`。

如果不想让配置文件散落在工具目录里，可以把它挪走：设一个环境变量 `CODEX_NOTIFY_DIR` 指向目标目录，配置和日志会一起跟过去。

```powershell
setx CODEX_NOTIFY_DIR "D:\my-config\codex-notify"
```

三个注意点：

- `setx` 写的是**用户级环境变量**，设完要**新开终端**才生效。Codex 拉起钩子时继承的就是这个用户环境变量。
- 只在某个终端窗口里 `$env:CODEX_NOTIFY_DIR = ...` **不够**——那个变量只存在于当前窗口，Codex 的进程看不到。
- 换目录后 `notify.toml` 要自己搬过去，程序不会自动迁移；找不到配置时它会用内置默认值，表现是「不发消息、也不报错」，容易误判。

查找顺序（先命中先用）：`CODEX_NOTIFY_DIR` → exe 同目录 → exe 的上两级。最后一条是为了让 `target\release` 里的产物也能找到项目根的配置，方便开发调试。

### 第三步：接管 Codex 的 notify 槽位

编辑 `~/.codex/config.toml`，**在顶层**（不是 `[tui]` 下面）加一行，路径指向你实际存放 exe 的位置：

```toml
notify = ['D:\tools\codex-notify\codex-notify.exe']
```

两个容易踩的点：

- **必须是绝对路径**，`~` 这类写法 Codex 不会展开
- **必须在顶层**。写进 `[tui]` 段落里不会生效，而且不报错，很难发现

**如果这里原来已经有内容**（比如桌面版的桌宠），分两种情况，先看一眼你现在这行长什么样：

**情况 A：看到 `--previous-notify`**

```toml
notify = [ "C:\\...\\codex-computer-use.exe", "turn-ended",
           "--previous-notify", "[\"D:\\\\...\\\\codex-notify.exe\"]" ]
```

这说明 **Codex 桌面版已经自动接管了**：它把桌宠放在前面，再把你的程序挂在 `--previous-notify` 参数里，由桌宠转发过来。实际调用是：

```
Codex → codex-computer-use.exe → codex-notify.exe
```

这种情况下 **`notify.toml` 里的 `chain` 要留空**（`chain = []`）。因为桌宠已经在外面被调用过一次了，再配 chain 会让它每回合跑两次。

**情况 B：原来那行没有 `--previous-notify`**

那你就是自己直接接管了槽位，需要手动把原命令挪到 `chain` 里，否则桌宠不再响应：

```toml
# 原来可能是：notify = ["C:\\...\\codex-computer-use.exe", "turn-ended"]
# 改成 notify = ['你的 codex-notify.exe']，然后把原命令写到这里：
chain = ['C:\Users\<你>\AppData\Local\OpenAI\Codex\runtimes\...\codex-computer-use.exe', 'turn-ended']
```

这样我们收到事件后会先把原始 JSON 转发给它，再发自己的通知，两边共存。

### 验证

```bash
codex-notify.exe --check     # ① 检查配置 + 探测 NapCat
codex-notify.exe --test      # ② 真发一条到手机
```

① 应该看到：

```
== QQ（走本机 NapCat）==
base_url: http://127.0.0.1:3000
target:   private:123456789
token:    (已设置)
连通性:   OK -> {"status":"ok","retcode":0,"data":{"user_id":...}}
```

「连通性 OK」说明配置读对了、NapCat 也在线。这一步不验证「能不能真的发出去」，只验证「连得上、登录着」。

② 会真的往你手机发一条。收到了就说明整条链路通了。没收到就看 exe 同目录的 `notify.log`，里面会写明失败原因（连接被拒、HTTP 401、retcode 非 0 等）。

最后回到 Codex 随便跑一个回合，确认它也会自动触发。

---

## 配置项

所有字段都有默认值，`notify.toml` 里只写你关心的几项也行。

| 配置项 | 默认值 | 说明 |
|---|---|---|
| `enabled` | `true` | 总开关。`false` 时钩子直接退出，不发消息 |
| `max_chars` | `260` | 摘要截断长度（按**字数**不是字节） |
| `timeout_secs` | `5` | HTTP 超时。这个值决定回合结束后最多卡多久 |
| `prefix` | `"[Codex]"` | 消息标题前缀 |
| `show_limits` | `true` | 是否带 5h / 周额度 |
| `show_footer` | `false` | 是否带末尾的 `(turn N · thread XXXXXXXX)`，多会话并行时才建议开 |
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

## 图标

`assets/` 里放着图标源文件：

| 文件 | 用途 |
|---|---|
| `icon-1024.png` | 主图标（圆角方形） |
| `icon-circle-512.png` | 圆形版本，README 顶部用的就是它 |
| `codex-notify.ico` | 多尺寸 Windows 图标，由 `build.rs` 嵌进 exe |
| `chevron-mono.svg` | 单色 chevron，给托盘/浅色背景用 |
| `icon-sheet.png` | 设计说明：各种尺寸和配色的效果预览 |
| `_build_icon.py` / `_build_ico.py` | 生成脚本，改完图形重新跑一遍即可（需要 Pillow） |

图形语言很简单：深色圆角底 + 白色气泡（你收到的消息）+ 深色 chevron（终端提示符）+ 青色圆点（有新消息）。配色 `#1B2938` / `#F5FAFF` / `#2BD2FF`。

---

## License

[MIT](LICENSE)
