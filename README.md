![](./assets/Primordial_Human.png)

# AliveBot

AliveBot（灰眸）是一个 Rust 编写的基于 [nagisa](https://github.com/djkcyl/nagisa) 的 QQ **群聊**机器人。它通过 OneBot WebSocket 接收群消息，并通过 Codex CLI 的 app-server 生成回复和执行工具。Codex 服务、配置、认证和日志都位于 AliveBot 内，可独立运行；LLMServer 控制台只是可选的管理界面。

**Bot 一定要 Alive。**AliveBot 支持丰富的消息形式，除普通消息外，还支持图片 URL、QQ 内置表情、回复、戳一戳、贴表情、合并转发、独立群文件收发及语音转录，这使得只要模型足够强大，机器人的行为可以接近真人。

## 准备

- Cargo 和 Visual Studio C++ 构建工具、CMake、LLVM/libclang。

- 一个提供 WebSocket 地址的 OneBot 实现，推荐 [NapCatQQ](https://github.com/NapNeko/NapCatQQ)。

- Codex CLI 0.160.0 或更新版本，以及 Node.js 22 或更新版本。机器人使用自己的 `codex-home`，不读取桌面 Codex 的个人配置。

- （可选）一个可被编译器找到的 CUDA Toolkit。



## 入门

1. 启动 NapCatQQ（或其他 OneBot 实现）并登录 QQ 账号；
   配置一个无认证 token 的 WebSocket 服务器；
   建议端口：8080。（若选择其他端口，需要同时在 AliveBot 中配置 url）

2. 首次安装先将 `codex-service/config.example.json` 复制为 `codex-service/config.json`；其中 `executable` 可填写 PATH 中的 `codex` 或 CLI 的完整路径。实例配置、`codex-home` 和群工作区均不提交 Git。准备 `codex-home/config.toml`（示例见 `config/codex_runtime_example.toml`），运行 `Codex登录.ps1` 登录机器人专用账号环境，再运行 `启动Codex服务.ps1`。已有机器人登录状态时无需重复登录。

3. 在 AliveBot 目录构建并运行主程序，使用原有的 `config/config.toml`。启动前先确认 Codex 服务已运行。每个群会创建 `memes` 并复制 `faces.csv`。

   ```powershell
   cargo build --release --target-dir target/codex
   ./target/codex/release/AliveBot.exe --config config/config.toml
   ```

现在，您已经创建了一个最基础的~~可用~~的群聊机器人！它会接收群中的消息，默认在被明确 @ 时调用模型生成回复。

#### [命令]

使用 `/` 开头即可发送命令。命令不会传给 LLM，而是执行一些固定操作。

- `/ping`：检查机器人是否在线
- `/new`：停止旧会话的执行并新建 Codex 会话，保留工作目录文件
- `/stop`：停止当前任务，保留会话历史及工作目录
- `/face`：发送表情
- `/faceid`：查询表情 ID
- `/react`：给被回复的消息添加回应



## 配置

AliveBot 有如下参数可以进行配置。

| 参数名                | 类型         | 默认值                                          | 说明                                                         |
| --------------------- | ------------ | ----------------------------------------------- | ------------------------------------------------------------ |
| `--config`            | `'PathBuf'`  | `'config/config.toml'`                          | 指定配置文件路径                                             |
| `--ws-url`            | `'String'`   | `'ws://127.0.0.1:8080'`                         | OneBot WebSocket 地址                                        |
| `--codex-url` | `String` | `http://127.0.0.1:4097` | AliveBot 自有 Codex 服务地址 |
| `--codex-token-file` | `PathBuf` | `codex-home/service-token.txt` | 本机服务认证；环境变量 `ALIVEBOT_CODEX_TOKEN` 优先 |
| `--workspace-dir` | `PathBuf` | `workspace` | 群工作目录根目录 |
| `--model`, `-m` | `String` | `gpt-6.1-sol` | Codex 模型 ID |
| `--reasoning-effort` | `String` | `high` | 推理强度；须由所选模型支持 |
| `--fast-mode` | `bool` | `false` | 每个新 turn 是否使用 Fast；false 明确使用普通速度；修改后重启 AliveBot |
| `--group-whitelist`   | `'Vec<i64>'` | `'[593883760]'`                                 | 群聊白名单；多个群号需要重复传入该参数                       |
| `--self-accounts`     | `'Vec<i64>'` | `'[1787552039, 3550036364]'`                    | 只加入上下文而不触发模型回复的账号                           |
| `--mention-only`      | `'bool'`     | `'true'`                                       | 仅被明确 @ 当前机器人账号时触发模型回复；false 恢复自动回复 |
| `--system-prompt`     | `'String'`   | `'你正在参加一个真实、持续运作的熟人QQ群聊...'` | 模型的系统提示词                                             |
| `--enable-transcript` | `'bool'`     | `'false'`                                       | 是否接收语音消息并转录。在启动时自动准备 FFmpeg 和 Whisper 模型 |

配置有两种方法：

1. 在启动程序时直接使用参数，优先级最高

2. 在 `config/config.toml` 中配置，参数名前缀横杠去掉，中间横杠变成下划线。

`mention_only` 默认为 `true`。空闲时未被 @ 的消息、贴表情和戳一戳仅记录；执行期间其他群友的新消息统一以 steer 注入。@全体不算明确 @ 机器人。自身判断仅依据 `self_accounts` 列表（默认 `[]`），不检查当前登录账号或平台 `is_self` 标记。列表内账号的消息和互动空闲时只记录，工作中与其他有效消息一样直接 steer；列表外账号均按普通群友处理。`mention_only = false` 时恢复普通消息及互动触发。已移除随机跳过、普通新消息导致的过期取消和 debug 控制台确认。



## 进阶

上述的机器人是一个**非常机器人的**机器人，一点也不 **Alive**，于是，我们可以开始搞点更高级的玩法。

#### [人格]

**可以用系统提示词为机器人设定人格**。相信你对此并不陌生！

不过默认提示词中存在很多重要的规则说明，因此建议在后面追加自己的提示词而不是替换。

#### [认识]

用户识别统一使用 QQ 号（uin），无需配置姓名映射表。普通消息同时提供昵称/群名片和 uin，转发消息保留原作者昵称与 uin；收到的 @、贴表情和戳一戳中的用户目标使用数字 uin。

模型发送个人提及时使用 `@123456789 正文`（uin 后接空格或换行），戳一戳使用独立标签 `<nudge receiver:123456789>`。uin 必须是正整数；昵称不再解析为动作目标。`@全体成员` 保持原有行为。程序不再创建或读取旧用户映射文件；已有文件留在本地，不再生效。

#### [自主权]

传统机器人只是一个有问必答的助手，但真正的人类既可以选择不说话，也可以选择连续说好几句话。

保持 NapCatQQ 的自身消息上报开启。AliveBot 会按群、发送者和发送接口返回的 QQ 消息 ID，过滤模型发送的普通消息的重复上报，不再次加入上下文或唤醒模型；原 assistant 回复保留。同账号手动发送、其他程序发送的消息照常处理，并继续遵守 `mention_only` 和 `self_accounts`。每群持久化最近 4096 个模型发送 ID，重启及 `/new` 后仍保留；不按正文或时间猜测来源。贴表情、戳一戳无独立消息 ID，仍按原规则处理。发送超时、进程在保存 ID 前退出或没有返回可用 ID 时，无法保证过滤该条上报。

与此同时，AliveBot 把查看合并转发消息的自主权也交给了机器人，当它看到一个合并转发消息，可以自行选择是否展开消息。

#### [多模态聊天内容]

`config/faces.csv` 提供 QQ 内置表情的名称。群工作区初始化时会将它复制到工作区根目录的 `faces.csv`，供 Agent 直接读取；已有副本会保留。群图片以 `<img:原始URL>` 文本标记传给 Agent，保留完整地址与查询参数。AliveBot 不再预下载、压缩或转成 Base64 视觉附件；Agent 按需用命令行下载原文件，再用图片查看工具读取。URL 可能过期，下载失败时应如实说明。

每群使用 `workspace/<群号>/` 作为工作目录，并自动创建其中的 `memes/` 素材文件夹。目录中的图片由 Agent 通过命令行管理，名称和简介可记录到知识库的“QQ 表情库”。发送本地图片时使用 `<img:相对路径>`，例如 `<img:memes/图片.png>`；路径基于当前群的 Agent 工作目录，文件必须真实存在且位于该目录内。它按普通图片发送，可以与文字混排。输入 URL 引用需先下载为本地文件，再通过相对路径发送。其他输出格式包括普通文字、QQ 内置表情、引用、@、贴表情、戳一戳和消息查询。

群文件以独立的 `<file path:原始URL, name:原文件名, size:字节数>` 消息传给 Agent；文件名和大小写在标签内，上传者 UIN 位于消息头。文件 ID 仅在程序内部使用；收到上传通知但没有 URL 时，通过 OneBot 查询群文件下载链接。Agent 按需下载并读取，失效链接的 path 标记为 unavailable，仍保留 name 和 size；文件名中的特殊字符使用 HTML 实体转义。文件内容不直接作为视觉附件注入。

发送文件时独立输出 `<file path:相对路径>`，例如 `<file path:files/报告.pdf>`，路径基于当前群的 Agent 工作目录。程序调用群文件上传接口，使用本地文件名；这不是图片消息段，不能与文字、图片、引用或其他动作混排。上传结果只写入日志，发送结果不明时不自动重试。返回文件 ID 时按文件 ID 和上传者过滤模型文件的重复上报，近期记录持久化并在 `/new` 后保留；接口没有返回文件 ID 时无法保证该过滤，不按文件名或时间猜测。

同时，AliveBot 重点支持了”贴表情“这一现代化的聊天模式，双击头像拍一拍也进行了支持，极大提高聊天的表现力。

这些还原真实人类的点睛之笔，如果准备训练模型，可以重点关注。

#### [语音转录]

此外，还可以配置并开启语音转录，以接收语音消息。

设置 `enable_transcript = true` 后，程序会在启动时自动下载 FFmpeg 和 Whisper 模型 `ggml-small-q5_1.bin`。模型保存在 `models/`，首次启动需要网络连接。

默认使用 CPU：

```bash
cargo run --release --bin AliveBot
```

使用 CUDA：

```bash
cargo run --release --features cuda --bin AliveBot
```

Cargo 只负责启用 CUDA feature，不会安装 CUDA Toolkit。**需要自行安装兼容的 CUDA 环境**。



## 项目状态

早期项目，功能不完善，仅为将创意落地。

## Codex 会话与运行状态

机器人专用目录 `codex-home` 保存配置、认证和 Codex 会话；`codex-service/config.json` 设置 CLI 路径、服务端口、工作区根目录。服务仅监听 127.0.0.1，需要随机生成的本机令牌。适配层只依赖 Node 标准库，启动 Codex app-server 的 stdio JSON-RPC，不依赖 LLMServer、OpenCode 服务、它们的密钥或数据库。

每群 `.alivebot/session.json` 保存 transport session ID、真实 `thread_id`、待投递消息、已处理消息和 QQ 发送去重状态；`.alivebot/codex` 保存用于恢复投递的本地日志，实际模型上下文由 Codex 自身持久保存。首次切换会备份旧状态为 `.alivebot/opencode-session.backup.json`，新建 Codex 会话并保留工作区素材与发送去重记录。不会再生成 `.opencode/opencode.json`。

空闲时，触发回复的消息启动 turn；其他消息通过 `thread/inject_items` 记录且不调用模型。工作中的所有有效消息，包括自身非模型消息，均以 `turn/steer` 追加当前轮。模型生成消息的 QQ 回传直接过滤。steer 和当前轮结束发生竞争时，重新核对状态，选择启动或只记录，不使用下一轮 queue。

过程说明与最终答复均可发送，思考和工具输出不发送。工具日志只有名称、状态，不保存参数和结果。输入、主提示词和公开输出日志位于 `codex-home/alivebot/trace-日期.jsonl`；Codex 原始诊断另存 `app-server.stderr.log`，不展示到工具过程窗口。

修改配置文件中的 `system_prompt` 无需重启。下次空闲投递前，会卸载并恢复该 thread，重新应用主提示词。工作中修改会在下一轮生效，不中断正在执行的工具。模型、推理强度、运行目录等其他配置修改仍需重启相应服务。主提示词仍只有这一份，插件使用说明不追加成新的全局人格提示词。

输入图片仅保留 `<img:原始URL>` 文本；Agent 自行下载和查看。图片、文件发送仍限定当前群工作区内的路径。memory 和 gcsim 保留其现有独立 CLI 和使用说明，不需要运行 OpenCode。

确定已接受的消息不会重复提交；连接中断导致接受结果不明时，保留待投递状态并报错，避免盲目重复启动工具或发送。`/stop` 中断当前轮；`/new` 建立新会话，保留素材和回传过滤记录。

验证命令：

```powershell
cargo test --locked --bin AliveBot
node --test codex-service/bridge.test.mjs
python codex-service/check_protocol.py
```

最后一项使用真实 CLI 和完全本地的模型替身，验证记录、steer、工具执行、提示词刷新及重启恢复，不调用付费模型，不连接 QQ。

## 协议端切换

AliveBot 通过 OneBot 11 正向 WebSocket 连接协议端，Codex、memory、gcsim 和 Yunzai 宿主相互独立。LLBot 提供对应的 OneBot 11 接口，但本部署仍待 LLBot Auth Token 审核及端到端验证，不能将接口存在等同于全部功能已验证。

LLBot 连接应使用数组消息格式并开启 `reportSelfMessage`，以保留同账号非模型消息和模型回复去重。更换协议端后旧消息 ID 不保证仍可用于引用、撤回或表情回应；切换应在在途发送完成后进行。普通图片和文件目前使用协议端可读取的本地路径，同一 Windows 主机可继续使用，跨系统部署需要内容上传或路径映射。Yunzai 使用另一条连接，关闭自身消息上报以避免触发插件循环。
