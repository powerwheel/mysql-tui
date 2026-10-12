# mysql_tui

> 一个快速、纯键盘操作的终端 MySQL 客户端 —— 使用 Rust 和 [ratatui](https://github.com/ratatui/ratatui) 编写。

[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows-lightgrey.svg)](#安装)

[English](README.md)

`mysql_tui` 面向常驻终端的开发者和 DBA：比自带的 `mysql` 命令行强得多，又不必打开笨重的图形客户端。单个静态二进制、无运行时、无 Electron：连接管理、感知表结构的自动补全、多标签编辑器、可滚动结果表格、流式导出、SSH/TLS 连接 —— 全部靠键盘完成。

```
┌ mysql_tui ──────────────────────────────── root@127.0.0.1:3306/shop  MySQL 8.4.3 ─┐
│ 1:SELECT * FROM orders  2:users ⟳            Ctrl+T new  Ctrl+W close  Alt+←/→ switch │
├ Databases ─────┬ Editor ───────────────────────────────────────────────────────────┤
│ ▾ shop         │ SELECT o.id, o.status, u.email, o.total                             │
│   ▸ customers  │ FROM orders o JOIN users u ON u.id = o.user_id                      │
│   ▾ orders     │ WHERE o.created_at > '2026-01-01'                                   │
│       id       │                                                                     │
│       user_id  ├ Results - 1000 rows x 4 cols | n: next 1000 ─────────────────────────┤
│       status   │ id      status     email                   total                    │
│       total    │ 10231   paid       alice@example.com       129.90                   │
│   ▸ users      │ 10232   shipped    bob@example.com         48.00                    │
│ ▸ analytics    │ 10233   pending    carol@example.com       310.25                   │
│ ▸ information… │ …                                                                   │
├────────────────┴─────────────────────────────────────────────────────────────────────┤
│ 1000 rows in 0.041s     Results | n next batch | Enter view cell | y/Y copy | F1 help │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

## 特性

- **连接管理** —— 保存多个服务器，启动自动连接；配置文件使用主密码加密存储（AES-256-GCM，Argon2id 派生密钥）。
- **安全连接** —— TLS 支持 `required` / `verify-ca` / `verify-identity` 三档及自定义 CA；可通过跳板机建立 SSH 隧道（调用系统 `ssh`，支持 agent 与密钥认证）。
- **智能编辑器** —— 多行编辑、撤销/重做、选区与剪贴板、基于表结构的关键字/表名/列名补全，`Ctrl+T` 多标签且状态互相独立。
- **能扛大结果集的表格** —— 列光标、横向滚动、一次执行多结果集、整格查看器（JSON 自动美化），以及 **加载下一批**（`n`）取代硬性行数上限。
- **流式导出** —— CSV / TSV / JSON 分页写入磁盘，导出 5000 万行的表不需要 5000 万行的内存。
- **安全护栏** —— 无 `LIMIT` 的 `SELECT` 自动追加 `LIMIT`；`DELETE` / `UPDATE` / `DROP` / `ALTER` / `TRUNCATE` 执行前二次确认；`Esc` 取消正在运行的查询（服务端 `KILL QUERY`）。
- **结构侧边栏** —— 浏览 数据库 → 表 → 列，`Enter` 切换数据库或插入表名，`F2` 查看 `SHOW CREATE TABLE` 与列详情。
- **查询历史** —— 可搜索，跨会话持久化（`F3`）。

## 安装

### 从源码构建

需要 Rust **1.88 或更高版本**（2024 edition，使用了 let-chains）。

```bash
git clone https://github.com/<you>/mysql_tui.git
cd mysql_tui
cargo install --path .
```

或直接运行：

```bash
cargo run --release
```

### 系统依赖

| 功能 | 要求 |
| --- | --- |
| TLS 连接 | Linux 编译时需要 OpenSSL 头文件（`libssl-dev` / `openssl-devel`）；macOS/Windows 使用系统 TLS 栈 |
| SSH 隧道 | `PATH` 中有 `ssh` 客户端，且认证能无交互完成（ssh-agent 或密钥文件） |
| 剪贴板 | Linux 需要 X11 或 Wayland 会话；macOS/Windows 开箱即用 |

## 快速上手

1. 运行 `mysql_tui`。
2. **首次启动** —— 设置一个主密码。它用于加密 `connections.toml`，之后每次启动都需要输入。（无法跳过：数据库密码永远不会以明文落盘。）
3. 按 `n` 新建连接，填写表单，`Enter` 保存。
4. 选中后按 `Enter` 连接；按 `a` 可将其设为启动时自动连接。
5. 输入 SQL，按 `F9` 执行。任何时候按 `F1` 查看快捷键。

## 快捷键

### 连接列表

| 按键 | 动作 |
| --- | --- |
| `Enter` | 连接 |
| `n` / `e` / `d` | 新建 / 编辑 / 删除连接 |
| `a` | 切换该连接的自动连接 |
| `Ctrl+Q` | 退出 |

### 编辑器

| 按键 | 动作 |
| --- | --- |
| `F9` | 执行编辑器全部内容（支持 `;` 分隔的多条语句） |
| `Enter` | 换行 |
| `Ctrl+Space` | 弹出补全 · `Tab` 接受 · `Ctrl+P`/`Ctrl+N` 上下选择 |
| `Ctrl+P` / `Ctrl+N` | 上一条 / 下一条历史（无弹窗时） |
| `Shift+方向键`, `Ctrl+A` | 选择文本 / 全选 |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | 复制 / 剪切 / 粘贴选区 |
| `Ctrl+Z` / `Ctrl+Y` | 撤销 / 重做 |
| `Tab` | 跳到结果区 |

### 结果区

| 按键 | 动作 |
| --- | --- |
| `j`/`k`, `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End` | 移动行选择 |
| `←` / `→` | 移动列光标（表格横向滚动） · `Ctrl+Home` 回到第一列 |
| `Enter` / `v` | 打开整格查看器 |
| `n` | 加载下一批行 |
| `y` / `Y` | 复制单元格 / 以 TSV 复制整行 |
| `[` / `]` | 切换结果集 |
| `Tab` / `Esc` | 侧边栏 / 返回编辑器 |

### 整格查看器

| 按键 | 动作 |
| --- | --- |
| `j`/`k`, `PgUp`/`PgDn`, `g`/`G` | 滚动 |
| `←`/`→` 或 `h`/`l` | 上一列 / 下一列 |
| `J` / `K` | 下一行 / 上一行 |
| `p` | 切换 JSON 美化显示 |
| `y` | 复制单元格文本 |
| `Esc` / `q` / `Enter` | 关闭 |

### 侧边栏

| 按键 | 动作 |
| --- | --- |
| `j`/`k` | 移动选择 |
| `←` / `→` | 折叠 / 展开 |
| `Enter` | `USE` 数据库 · 向编辑器插入表名 |
| `F2` | 表详情弹窗（`SHOW CREATE TABLE`、列信息） |
| `Tab` / `Esc` | 返回编辑器 |

### 标签与全局

| 按键 | 动作 |
| --- | --- |
| `Ctrl+T` / `Ctrl+W` | 新建 / 关闭查询标签 |
| `Alt+←`/`→`, `Ctrl+PgUp`/`PgDn`, `Alt+1..9` | 切换标签 |
| `F3` | 查询历史面板 —— 直接输入过滤，`Enter` 载入，`Ctrl+D` 删除 |
| `Ctrl+E` | 导出当前结果集 |
| `Ctrl+F` | 刷新表结构缓存 |
| `Ctrl+D` | 断开连接 |
| `Esc` | 取消运行中的查询 / 关闭对话框 |
| `F1` | 帮助 |
| `Ctrl+Q` / `Ctrl+C` | 退出（编辑器有选区时 `Ctrl+C` 为复制） |

## 工作原理

### 行数限制与"加载下一批"

`mysql_tui` 绝不会把无上限的结果集整个拉进内存：

- 没有 `LIMIT` 的 `SELECT` 会自动追加 `LIMIT <default_limit>`（默认 **1000**，见[配置](#配置)）。设 `default_limit = 0` 可关闭。
- 无论 `LIMIT` 多大，每个结果集在表格中最多保留 **10 000** 行。
- 可能还有更多行时，结果区标题显示 `| n: next <页大小>` —— 按 `n` 追加下一页。分页基于你的原语句加 `LIMIT/OFFSET`，支持 `SELECT` 与 `WITH … SELECT`，并会沿用已有的 `LIMIT o, n` / `LIMIT n OFFSET o` 窗口。
- 需要全部数据？`Ctrl+E` → *export all* 把完整结果流式写到磁盘。

### 导出

`Ctrl+E` 打开导出对话框（路径 + CSV/TSV/JSON）。结果完整未截断时直接写出内存中的行；已截断时会重新执行原 `SELECT`，按 10 000 行一页边查边追加写入文件，状态栏实时显示进度。`~` 会被展开，缺少扩展名时自动补上。

### 危险语句确认

以 `DELETE`、`UPDATE`、`DROP`、`ALTER`、`TRUNCATE` 开头的语句在发送前弹出确认框。带 `WHERE` 也不会跳过 —— 规则刻意保持简单。

### 多语句执行

用 `;` 分隔多条语句。每条返回列的语句成为独立结果集（`[` / `]` 切换）；只影响行数的语句汇总显示在状态栏。DDL 和 `USE` 会自动刷新表结构缓存，保证补全始终最新。

### SSH 隧道

填写 **SSH Host**（可选端口、用户、密钥路径）。`mysql_tui` 以子进程方式运行 `ssh -N -L 127.0.0.1:<随机端口>:<db-host>:<db-port> …`，再通过本地端口连接 MySQL。手动 `ssh -L` 能通的场景这里都能通；但不支持密码交互（子进程没有 TTY）。断开连接时隧道自动关闭。

### TLS

| 模式 | 行为 |
| --- | --- |
| `off` | 明文 |
| `required` | 加密；接受任意证书 |
| `verify-ca` | 加密；用系统信任库或 **TLS CA** 校验证书链 |
| `verify-identity` | 在 `verify-ca` 基础上校验主机名（经 SSH 隧道时自动跳过，因为对端永远是 `127.0.0.1`） |

设置了 TLS 模式而服务器未协商 TLS 时，会直接拒绝连接，而不是静默降级。协商到的加密套件显示在顶栏。

## 配置

文件位于平台配置目录（Linux `~/.config/mysql-tui/`，macOS `~/Library/Application Support/mysql-tui/`，Windows `%APPDATA%\mysql-tui\`）：

| 文件 | 内容 |
| --- | --- |
| `connections.toml` | 连接与设置 —— 用主密码**加密**（AES-256-GCM，Argon2id 派生密钥，随机 salt + nonce） |
| `history.json` | 最近 200 条执行过的查询，明文 |

通过界面编辑的设置都保存在加密文件内；解密后结构如下：

```toml
default_limit = 1000      # 自动追加给无 LIMIT 的 SELECT；0 表示关闭
auto_connect = 1          # [[connections]] 的 1-based 序号；省略则不自动连接

[[connections]]
name = "local"
host = "127.0.0.1"
port = 3306
user = "root"
password = "..."
database = "shop"

[connections.tls]
mode = "verify-ca"        # off | required | verify-ca | verify-identity
ca_path = "~/certs/ca.pem"

[connections.ssh]         # 可选
host = "bastion.example.com"
port = 22
user = "deploy"
key_path = "~/.ssh/id_ed25519"
```

忘记主密码？没有找回途径 —— 删除 `connections.toml` 后重新录入连接。

## 架构一段话

一个后台 **DB worker 线程**独占 MySQL 连接，通过 `mpsc` 通道与 UI 通信（`DbCommand` 进、`DbEvent` 出），慢查询永远不会卡住渲染。UI 循环（crossterm + ratatui）轮询事件，维护每个标签的编辑器/结果状态，并把异步结果路由回发起它的标签。取消查询时新开一条临时连接对 worker 的会话执行 `KILL QUERY <id>`。导出与"加载下一批"复用同一 worker 跑 `LIMIT/OFFSET` 分页。全部为纯 Rust，没有 `unsafe`。

```
src/
├── main.rs        入口
├── app.rs         状态机、按键处理、标签、DB 事件路由
├── ui.rs          全部 ratatui 渲染
├── db.rs          worker 线程、连接/TLS/SSH、查询与分页、KILL QUERY
├── editor.rs      带撤销/重做与选区的文本缓冲
├── completer.rs   关键字/表/列补全
├── schema.rs      表结构元数据缓存
├── exporter.rs    CSV/TSV/JSON 写入器（流式）
├── history.rs     查询历史持久化
├── config.rs      connections.toml 模型与序列化
└── crypto.rs      配置文件的 AES-256-GCM + Argon2id
```

## 开发

```bash
cargo test                      # 单元测试（UI 测试使用 ratatui 的 TestBackend，无需终端）
cargo clippy --all-targets      # lint
cargo build --release           # 开启 LTO 的二进制：target/release/mysql_tui
```

测试不依赖数据库。少数 `#[ignore]` 的探针测试可在本机有 MySQL 时运行：

```bash
cargo test -- --ignored
```

## 路线图

- 事务模式（`BEGIN` … `COMMIT`/`ROLLBACK` 开关并显示状态）
- 标签间结果集对比
- 补全器支持 MariaDB 特有语法
- 鼠标支持

欢迎提 issue 反馈想法和问题。

## 参与贡献

1. Fork 并创建分支。
2. `cargo fmt`、`cargo clippy --all-targets`、`cargo test` 必须全部通过。
3. 行为变更请附带测试 —— 应用层测试通过合成的 `KeyEvent` 与 `DbEvent` 驱动 `App`，编写成本很低。
4. 提交 PR 时请说明 *为什么*，而不只是 *改了什么*。

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
