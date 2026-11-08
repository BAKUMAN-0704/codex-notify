<p align="center">
  <img src="assets/icon-circle-512.png" width="112" alt="codex-notify">
</p>

<h1 align="center">codex-notify</h1>

<p align="center">Codex 每完成一个回合，把结果推到你的 QQ。</p>

一个很小的 Rust 程序（约 0.9 MB、单文件、无运行时依赖），挂在 Codex 的 `notify` 钩子上。当 Codex 干完活、等你回话的时候，你手机上会收到一条这样的消息：

```
[Codex] 任务完成 · notify
13:16 完成 · 5h 已用 62%（18:20 重置） · 周 已用 18%（下周日 09:56 重置）
---
已把 notify 钩子接到 QQ 接口，测试通过。
```

比“盯着终端等”省事，也比“跑完忘了回来看”安全。

**[下载最新版本](https://github.com/BAKUMAN-0704/codex-notify/releases)** · [怎么安装](#安装) · [怎么配置](#配置) · [常见问题](#常见问题)

---

## 它是怎么工作的

Codex 的 `config.toml` 里有个 `notify` 配置项：每次回合结束，Codex 会拉起这个程序，并把事件 JSON 作为**最后一个命令行参数**传进来。所以钩子路径是个一次性进程——启动、发一条消息、退出（托盘模式是常驻的例外）。

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
| **Rust 1.82+** | 只有「自己编译」才需要，下载预编译版本可以跳过 | 可选 |

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

单文件、无运行时依赖，约 0.9 MB，Windows 10/11 x64。

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

> 编译时 `build.rs` 会把 `assets/codex-notify.ico` 嵌进 exe，并写入文件描述 `codex-notify`（任务栏、文件属性里显示的就是它们）。这一步需要 Windows SDK 里的 `rc.exe`；如果你的机器上找不到，只会打一条 warning 然后继续，产物正常可用、只是图标是默认的。想手动指定路径就设 `CODEX_NOTIFY_RC_DIR` 指向含 `rc.exe` 的目录。

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

查找顺序（先命中先用）：`CODEX_NOTIFY_DIR` → exe 同目录 → 上一级 → 上两级。最后一条是为了让 `target\release` 里的产物也能找到项目根的配置，方便开发调试。

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

② 会真的往你手机发一条。收到了就说明整条链路通了。没收到就看配置同目录的 `notify.log`，里面会写明失败原因（连接被拒、HTTP 401、retcode 非 0 等）。

最后回到 Codex 随便跑一个回合，确认它也会自动触发。

---

## 配置项

所有字段都有默认值，`notify.toml` 里只写你关心的几项也行。

| 配置项 | 默认值 | 说明 |
|---|---|---|
| `enabled` | `true` | 总开关。`false` 时钩子直接退出，不发消息 |
| `only_main_agent` | `true` | 只通知主 Agent，挡掉子 Agent / 内部审查回合（见[下方专节](#子-agent-也会触发-notify)） |
| `max_chars` | `260` | 摘要截断长度（按**字数**不是字节） |
| `timeout_secs` | `5` | HTTP 超时。这个值决定回合结束后最多卡多久 |
| `prefix` | `"[Codex]"` | 消息标题前缀 |
| `show_limits` | `true` | 是否带 5h / 周额度（只读 Codex 会话日志） |
| `show_reset` | `true` | 额度后面是否带「什么时候重置」 |
| `limit_style` | `"used"` | `"used"` 显示已用百分比，`"remaining"` 显示剩余百分比 |
| `show_footer` | `false` | 是否带末尾的 `(turn N · thread XXXXXXXX)`，多会话并行时才建议开 |
| `chain` | `[]` | 链式转发的原 notify 命令（数组，第一个元素是程序路径） |
| `[qq].base_url` | `http://127.0.0.1:3000` | NapCat 的 HTTP 服务地址 |
| `[qq].access_token` | `""` | OneBot 鉴权 token，空则不带头 |
| `[qq].target` | — | `private:QQ号` 或 `group:群号` |
| `[napcat].command` | `[]` | 托盘要拉起的 NapCat 启动命令，留空则不托管 |
| `[napcat].kill_on_exit` | `true` | 托盘退出时是否一起结束 NapCat |

---

## 命令行

| 命令 | 作用 |
|---|---|
| `codex-notify.exe '<JSON>'` | 由 Codex 调用，正常路径 |
| `--tray` | 常驻托盘；双击 exe 不带参数也会进托盘。右键可看状态 / 发测试 / 改设置 / 管 NapCat（仅 Windows） |
| `--test` | 用一条假事件真发一条，验证整条链路 |
| `--set key=value` | 改一项配置并写回（`--set show_limits=false`），和托盘设置菜单同一条路径 |
| `--classify <thread-id>` | 查这个 thread 会被判为主/子 Agent、会不会被过滤，不发消息 |
| `--dry-run` | 只打印将要发送的内容，不发请求 |
| `--check` | 检查配置解析 + 探测 NapCat 连通性 |
| `--init` | 生成默认配置（已存在不覆盖） |
| `--help` | 帮助 |

运行时日志写在配置同目录的 `notify.log`，超过 256 KB 自动轮转成 `notify.log.1`。

### 关于控制台窗口

程序被编译成 **GUI 子系统**，所以 Codex 每回合拉起它时**不会闪一个黑色控制台窗口**。

代价是：从终端里跑 `--check` 这类命令时，程序会主动去「借」父进程的控制台（`AttachConsole`），所以输出照常能看到。但如果你用某种拿不到控制台的宿主去调用它，那就只能看日志文件了。

### 托盘模式（`--tray`）

想要一个「看得见、关得掉」的入口，就让它常驻托盘。双击 exe（不带任何参数）即可；也可以显式指定：

```
D:\tools\codex-notify\codex-notify.exe --tray
```

托盘右键菜单：

| 菜单项 | 作用 |
|---|---|
| 状态 | 显示 `NapCat 在线` / `NapCat 已启动，等待扫码登录` / `NapCat 未运行`。每 5 秒自动刷新 |
| 启动 / 停止 NapCat | 托管中给「停止」，未运行时给「启动」；别人手动开的会置灰说明 |
| 发送测试消息 | 等同于 `--test` |
| 打开配置目录 | 用资源管理器打开 `notify.toml` 所在目录 |
| **设置** ▸ | 勾选框，点一下立刻生效并写回 `notify.toml` |
| 退出 | 删掉托盘图标并结束进程（不删图标会留下「幽灵图标」） |

「设置」子菜单覆盖了所有能安全用菜单改的配置，点一下立刻生效并写回 `notify.toml`：

| 菜单项 | 配置项 | 说明 |
|---|---|---|
| 启用通知 | `enabled` | 总开关，关掉后钩子静默退出 |
| 只通知主 Agent | `only_main_agent` | 子 Agent / 内部审查回合完成时不再推送 |
| 显示额度 | `show_limits` | 关掉后消息里只剩时间和摘要 |
| 显示重置时间 | `show_reset` | 例如 `（22:11 重置）` |
| 额度显示为「剩余」 | `limit_style` | 关＝已用 6%，开＝剩余 94% |
| 显示会话标记 turn/thread | `show_footer` | 多会话并行时才需要 |
| 摘要长度：N 字 | `max_chars` | 点击循环 80 → 160 → 260 → 400 → 600 |
| HTTP 超时：N 秒 | `timeout_secs` | 点击循环 1 → 3 → 5 → 10 |
| 退出托盘时一起关闭 NapCat | `[napcat].kill_on_exit` | 只作用于托盘自己启动的那个 |

**没进菜单的是文本和路径类**：`prefix`（前缀）、`chain`（转发命令）、`[qq]` 的地址/账号、`[napcat].command`——菜单没法输入这些，用上面的「打开配置目录」编辑文件。改完不用重启，下一次通知就按新配置走。

写回用的是 `toml_edit`，**会保留你手写的注释和排版**，只改被点的那一项，其他行一个字节不动。

命令行也能改，和菜单走同一条写回路径（方便脚本化）：

```bash
codex-notify.exe --set show_limits=false
codex-notify.exe --set napcat.kill_on_exit=false
```

**托盘只是个看板，不在发送路径上。** Codex 每回合仍然是独立进程直接发消息，不经过托盘——所以托盘关掉不影响通知，也不影响回合收尾速度。

### 托管 NapCat（可选）

不想每次开机手动开 NapCat，可以让托盘把它带上：

```toml
[napcat]
# Windows 版 NapCat 的启动脚本（换成你自己的路径）
command = ['D:\tools\NapCat\launcher-user.bat']
# 退出托盘时一起结束
kill_on_exit = true
```

配好之后：

- **托盘启动时**，如果 NapCat 没在跑（端口连不上），就一并拉起
- **托盘退出时**，把 NapCat 一起结束
- **只管自己启动的那个**。你手动开的 NapCat 它不认识，既不接管也不杀——所以不会出现「点一下退出，QQ 突然掉线」的意外

`launcher-user.bat` 不提权，`launcher.bat` 会弹 UAC。如果你的 QQ 装在 `Program Files` 下、用户态注入失败，换成后者。

**实现上没有逐个 kill PID，用的是 Job Object。** NapCat 的进程树是 `cmd → NapCatWinBootMain.exe → QQ.exe（+ 若干渲染子进程）`，逐 PID 清理容易漏、也会被 PID 复用坑到。Job Object 设上 `KILL_ON_JOB_CLOSE` 之后，本进程退出 → 句柄关闭 → 整棵树一起结束，不需要写任何清理逻辑。

---

## 子 Agent 也会触发 notify

Codex 派出去的子 Agent、内部审查回合**结束时也会触发 notify**，但那时主任务还在跑，手机却先收到一条「干完了」。`only_main_agent = true`（默认）把这些挡掉，只在你自己的主会话结束时通知。

难点是 **notify 事件 JSON 里没有父子标记**。查了 Codex 源码确认 `legacy_notify` 只塞了 `type / thread-id / turn-id / cwd / client / input-messages / last-assistant-message`，其中 `thread-id` 取自 `sess.thread_id` —— 即触发回合那个会话自己的 ID。子 Agent 是独立会话，所以可以拿它去查来源。

Codex 会在 `~/.codex/sessions/年/月/日/rollout-...-<thread-id>.jsonl` 的第一行写 `session_meta`，里面有个 `thread_source`：

| `thread_source` | 含义 | 处理 |
|---|---|---|
| `user` | 你发起的主会话 | 通知 |
| `subagent` | 派出去的子 Agent（还带 `agent_path: /root/xxx`、昵称） | 跳过 |
| `guardian_review` | 内部审查回合 | 跳过 |

（`~/.codex/state_5.sqlite` 的 `threads.source` 记的是同一件事：`vscode` 是主会话，`{"subagent":...}` 是子 Agent。为读一张表给这个单文件程序塞进整个 SQLite 驱动不值得，所以走纯文本的会话日志。）

**只拦自己这条消息，不拦转发。** 过滤放在 `chain` 之后 —— 桌宠该收到的事件一个都不能少。

**查不到就照常通知。** `thread_source` 是 Codex 的内部实现细节，改版可能消失或改名；文件找不到、字段不认识、解析失败，一律按主会话放行。宁可多推一条，也不能把真正的「主 Agent 干完了」吞掉。`--test` / `--dry-run` 用假 thread-id，走的也是这条放行路径。

想核对某个事件会被怎么处理：

```bash
codex-notify.exe --classify 01a1ba97-af78-7db1-9dbf-86c1b836aeb1
# thread_source: subagent
# 判定:         子 Agent / 内部回合
# 结果:         会跳过通知
```

钩子每次运行还会在 `notify.log` 里留一行 `thread <id> → 主 Agent / 无法判定`，从事件 JSON 里复制 `thread-id` 就能用 `--classify` 复查。

---

## 常见问题

**收不到消息？**

1. 跑 `--check`，看「连通性」那行。失败的话依次检查：NapCat 是否在运行、HTTP 服务器是否启用、端口/token 是否一致。
2. `--check` 通过但收不到：跑 `--test`，然后看 `notify.log` 里的记录。
3. 都正常但还是没反应：确认 `~/.codex/config.toml` 里的 `notify` 写在**顶层**，并且路径是绝对路径。

**桌宠不动了？** `chain` 没配。把原来那条 `codex-computer-use.exe turn-ended` 加回去。

**想临时关掉？** 把 `enabled` 设为 `false`（托盘里取消勾选「启用通知」），或清空 `[qq].target` 即可，不用改 `config.toml`。

**回合结束后感觉卡了一下？** 因为 Codex 会等这个进程退出。本机 NapCat 通常 5 ms 就返回，但如果你把 `base_url` 指向了远程地址，就会明显变慢。可以调小 `timeout_secs`。

**消息会不会把代码发出去？** 不会。发送的是项目名（工作目录最后一段）、完成时间、额度百分比，以及 Codex 最后一条回复的前 `max_chars` 个字；Codex 没输出文字时会退用你最后一条指令当摘要。不发完整路径，也不发文件内容。

---

## 限制

- 在 Windows 上开发和验证，代码本身是跨平台的，但其他平台未测试。
- 依赖 NapCat 常驻。NapCat 退出、掉线，或者 QQ 自动更新导致注入失效，通知都会中断（`--check` 能看出来）。
- NapCat 是第三方对 QQ 客户端的协议实现，属于灰色地带，账号有被风控的可能。请自行评估，建议用小号。
- `only_main_agent` 依赖 Codex 会话日志里的 `thread_source` —— 这是内部字段，未写进官方文档。字段若变动，程序会自动退回「全部照常通知」（不会漏推，但可能又开始收到子 Agent 的推送），此时升级本程序或关掉该开关即可。

---

## License

[MIT](LICENSE)
