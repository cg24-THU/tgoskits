# qperf 与 StarryOS 集成报告

## 1. 概述

本次工作将 Starry-OS 的 qperf 性能分析工具集成到了 TGOSKits 的 StarryOS 构建流程中。完成后，用户可以通过一条命令完成 qperf plugin 构建、StarryOS 内核构建、QEMU 插件注入、采样数据收集和 folded stack 报告生成。

主要入口命令如下：

```bash
cargo starry perf --arch riscv64
```

当前已经完成并验证的核心能力包括：

- 在 TGOSKits 中新增 `cargo starry perf` 子命令；
- 自动构建内置的 qperf plugin 和 qperf-analyzer；
- 自动构建带调试信息的 StarryOS kernel；
- 自动定位 StarryOS kernel ELF、raw image 和 rootfs；
- 自动生成带 `-plugin libqperf.so,...` 参数的 QEMU 启动配置；
- 自动运行 QEMU 并生成 qperf 原始采样数据；
- 自动调用 analyzer 生成 `stack.folded`；
- 在存在 flamegraph 工具时自动生成 `flamegraph.svg`；
- 在没有 flamegraph 工具时保留 `stack.folded`，主流程不失败；
- 修复 qperf 中无界队列、过深 frame pointer 回溯、写入路径 panic 等稳定性问题。

riscv64 路径已在 Docker 镜像 `b7c4600e825d` 中完成验证。最终生成的 `stack.folded` 非空，并且包含 StarryOS / ArceOS 内核符号，例如 `ax_plat`、`ax_task`、`ax_mm` 等。

## 2. 代码结构调研结果

### 2.1 cargo starry 入口

`cargo starry` 的入口定义在 `.cargo/config.toml`：

```toml
starry = "run -p tg-xtask -- starry"
```

StarryOS CLI 的主要入口位于：

```text
scripts/axbuild/src/starry/mod.rs
```

本次新增的 qperf 集成逻辑位于：

```text
scripts/axbuild/src/starry/perf.rs
```

相关的既有 StarryOS 构建、rootfs 和 QEMU 配置逻辑主要位于：

```text
scripts/axbuild/src/starry/build.rs
scripts/axbuild/src/starry/rootfs.rs
scripts/axbuild/src/starry/mod.rs
```

### 2.2 QEMU 参数组装位置

StarryOS 当前的 QEMU 参数由 StarryOS QEMU 模板加载后进行 patch。qperf 集成中复用了以下逻辑：

```rust
rootfs::load_patched_qemu_config(...)
```

该逻辑负责根据当前架构、rootfs、网络和 SMP 等配置生成最终 QEMU 参数。qperf 集成没有重新实现完整 QEMU 参数组装，而是在已有 QEMU 参数前注入：

```text
-plugin /path/to/libqperf.so,freq=...,out=...,max_depth=...
```

随后将完整参数写入输出目录中的 `qemu.toml`，并用该参数启动 QEMU。

### 2.3 kernel ELF / image 产物路径

riscv64 架构对应 target 为：

```text
riscv64gc-unknown-none-elf
```

调试构建下的关键产物路径为：

```text
target/riscv64gc-unknown-none-elf/debug/starryos
target/riscv64gc-unknown-none-elf/debug/starryos.bin
target/riscv64gc-unknown-none-elf/rootfs-riscv64.img
```

其中：

- `starryos` 是 analyzer 解析符号时使用的 kernel ELF；
- `starryos.bin` 是 QEMU 启动时使用的 raw kernel image；
- `rootfs-riscv64.img` 是 StarryOS 运行所需 rootfs。

### 2.4 host tool 构建逻辑

TGOSKits 已经存在 host-side 工具构建流程，例如 `cargo xtask` 和 `cargo starry` 的子命令体系。

本次新增的 qperf 集成采用最小侵入方式：

- 将 qperf 放入 `tools/qperf`；
- `cargo starry perf` 内部自动执行 qperf 的 release 构建；
- 不要求用户手动 clone qperf；
- 不要求用户手动指定 `libqperf.so`；
- 不要求用户手动指定 analyzer；
- 不要求用户手动传入 kernel ELF。

## 3. qperf 代码结构

qperf 被引入到：

```text
tools/qperf
```

主要文件如下：

```text
tools/qperf/Cargo.toml
tools/qperf/Cargo.lock
tools/qperf/src/lib.rs
tools/qperf/src/profiler.rs
tools/qperf/src/reg.rs
tools/qperf/analyzer/Cargo.toml
tools/qperf/analyzer/src/main.rs
tools/qperf/README.md
```

plugin 构建产物为：

```text
tools/qperf/target/release/libqperf.so
```

analyzer 构建产物为：

```text
tools/qperf/target/release/qperf-analyzer
```

qperf 的基本数据流为：

1. QEMU 通过 `-plugin libqperf.so,...` 加载 qperf plugin；
2. plugin 按配置频率对 guest 执行状态采样；
3. plugin 读取寄存器和 frame pointer 链，得到调用栈地址；
4. plugin 将采样记录写入 `qperf.bin`；
5. analyzer 读取 `qperf.bin`；
6. analyzer 使用 kernel ELF 解析符号；
7. analyzer 输出 folded stack 文件 `stack.folded`；
8. 可选使用 `inferno-flamegraph`、`flamegraph` 或 `flamegraph.pl` 生成 `flamegraph.svg`。

## 4. qperf 修复与改进

### 4.1 修复前的主要风险

调研 qperf 后，发现其运行时存在以下稳定性风险：

- 采样写入队列存在无界增长风险；
- QEMU 主执行路径上可能阻塞等待 writer；
- frame pointer 回溯缺少最大深度限制；
- frame pointer 链缺少循环、倒退和坏地址检测；
- 文件写入没有统一使用 buffered writer；
- 采样和写入路径存在 panic 风险；
- analyzer 对坏数据、空栈和重复符号解析不够鲁棒；
- analyzer 在大规模样本下可能反复解析相同地址，性能较差。

### 4.2 已完成的 plugin 改进

本次修改后，qperf plugin 采用 bounded channel，TGOSKits 集成默认队列大小为 4096：

```text
queue=4096
```

采样路径使用非阻塞 `try_send`。当队列满时，当前 sample 会被丢弃，并记录 `dropped_samples`，避免 QEMU 主执行路径被 writer 拖住。

frame pointer unwinding 增加了 `max_depth` 限制，TGOSKits 默认值为 64，qperf 内部默认值为 128。回溯过程中会在以下条件停止：

- frame pointer 为 0；
- frame pointer 未对齐；
- frame pointer 没有前进；
- frame pointer 出现循环；
- 超过最大栈深；
- guest memory 读取失败。

writer 路径改为使用 `BufWriter`，减少每条 sample 的 flush 成本。采样和写入路径避免使用 `expect` 或直接 panic，错误会被计入 `sample_failures` 或写入失败统计。

plugin 正常退出时会输出 summary 信息，包含：

- qperf 格式版本；
- 采样总数；
- dropped sample 数；
- sample failure 数；
- 采样频率；
- 最大栈深；
- 架构；
- 输出文件路径。

### 4.3 已完成的 analyzer 改进

analyzer 现在具备以下改进：

- 使用 buffered input/output；
- 对地址到符号的解析结果做缓存；
- 对空栈输出 `??`；
- 对未知符号输出十六进制地址；
- 对尾部 partial record 更宽容；
- 对少量坏记录不直接导致整个分析流程崩溃；
- 对文件打开、ELF 解析和输出写入返回带上下文的错误。

这些修改解决了 qperf 长时间运行时无界内存增长的主要风险，并提升了 analyzer 在坏数据或不完整数据场景下的可用性。

## 5. TGOSKits 集成方式

### 5.1 新增命令

新增命令：

```bash
cargo starry perf [OPTIONS]
```

当前支持的参数：

```text
--arch <ARCH>          目标架构，支持 riscv64 / loongarch64
--freq <N>             采样频率，默认 99
--out <DIR>            输出目录，默认 target/qperf/<arch>/<timestamp>/
--format <FORMAT>      输出格式，支持 folded / svg / pprof / all，默认 all
--max-depth <N>        最大栈回溯深度，默认 64
--timeout <SECONDS>    QEMU 运行超时时间，默认 20
```

其中 `pprof` 参数已预留，但当前尚未实现。如果指定 `--format pprof`，命令会返回明确的 unsupported-format 错误。

### 5.2 默认输出目录

默认输出目录格式：

```text
target/qperf/<arch>/<timestamp>/
```

输出内容示例：

```text
target/qperf/riscv64/20260516-120000/
  qemu.toml
  qperf.bin
  stack.folded
  flamegraph.svg
  summary.txt
```

其中：

- `qemu.toml` 记录实际用于 qperf 的 QEMU 参数；
- `qperf.bin` 是 qperf plugin 输出的原始采样数据；
- `stack.folded` 是 analyzer 输出的 folded stack；
- `flamegraph.svg` 是可选火焰图，只有安装了 flamegraph 工具时才会生成；
- `summary.txt` 记录本次运行的路径、参数和 folded stack 行数等信息。

### 5.3 指定输出目录示例

```bash
cargo starry perf \
  --arch riscv64 \
  --timeout 20 \
  --format folded \
  --out target/qperf/my-riscv64-run
```

运行完成后检查：

```bash
ls -lh target/qperf/my-riscv64-run
wc -l target/qperf/my-riscv64-run/stack.folded
cat target/qperf/my-riscv64-run/summary.txt
```

## 6. 完整运行示例

### 6.1 推荐 Docker 运行方式

所有关键验证都建议在 Docker 容器中执行。推荐使用镜像：

```text
b7c4600e825d
```

进入交互式容器：

```bash
docker run --rm -it --privileged \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash
```

也可以直接使用非交互式命令：

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format folded --out target/qperf/repro-riscv64'
```

### 6.2 一条命令生成 folded stack

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format folded --out target/qperf/repro-riscv64'
```

预期生成：

```text
target/qperf/repro-riscv64/qemu.toml
target/qperf/repro-riscv64/qperf.bin
target/qperf/repro-riscv64/stack.folded
target/qperf/repro-riscv64/summary.txt
```

检查 folded stack 是否非空：

```bash
wc -l target/qperf/repro-riscv64/stack.folded
```

检查是否包含 StarryOS / ArceOS 相关符号：

```bash
grep -E 'ax_task|ax_mm|ax_plat|syscall|starry|starry_api' \
  target/qperf/repro-riscv64/stack.folded \
  | head
```

查看 summary：

```bash
cat target/qperf/repro-riscv64/summary.txt
```

### 6.3 生成 flamegraph.svg

如果容器或本地环境中安装了 `inferno-flamegraph`、`flamegraph` 或 `flamegraph.pl`，可以使用：

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format all --out target/qperf/repro-riscv64-all'
```

如果 flamegraph 工具存在，预期生成：

```text
target/qperf/repro-riscv64-all/flamegraph.svg
```

如果 flamegraph 工具不存在，命令不会失败，会保留：

```text
target/qperf/repro-riscv64-all/stack.folded
```

并提示安装可选工具。

安装 inferno 的示例：

```bash
cargo install inferno
```

手动从 folded stack 生成 SVG：

```bash
inferno-flamegraph \
  < target/qperf/repro-riscv64-all/stack.folded \
  > target/qperf/repro-riscv64-all/flamegraph.svg
```

### 6.4 调整采样频率和最大栈深

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --freq 100 --max-depth 128 --timeout 30 --format folded --out target/qperf/repro-riscv64-f100-d128'
```

参数说明：

- `--freq 100` 表示按 100Hz 左右频率采样；
- `--max-depth 128` 表示每条 sample 最多回溯 128 层；
- `--timeout 30` 表示 QEMU 最多运行 30 秒。

### 6.5 查看实际 QEMU plugin 参数

每次运行都会在输出目录写入 `qemu.toml`。可以用以下命令查看：

```bash
cat target/qperf/repro-riscv64/qemu.toml
```

其中应能看到类似参数：

```text
-plugin
/work/tools/qperf/target/release/libqperf.so,freq=99,out=target/qperf/repro-riscv64/qperf.bin,max_depth=64,queue=4096
```

## 7. 手工跑通 qperf 的验证过程

在集成前，先完成了手工验证，证明 qperf 可以和 StarryOS 连通。

### 7.1 构建 qperf plugin 和 analyzer

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /qperf \
  b7c4600e825d \
  bash -lc 'cargo build --release && cargo build --release -p qperf-analyzer'
```

预期产物：

```text
/qperf/target/release/libqperf.so
/qperf/target/release/qperf-analyzer
```

### 7.2 构建 StarryOS kernel 和 rootfs

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry build --arch riscv64 --debug && cargo starry rootfs --arch riscv64'
```

预期产物：

```text
target/riscv64gc-unknown-none-elf/debug/starryos
target/riscv64gc-unknown-none-elf/debug/starryos.bin
target/riscv64gc-unknown-none-elf/rootfs-riscv64.img
```

### 7.3 手动向 QEMU 注入 qperf plugin

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /work \
  b7c4600e825d \
  bash -lc 'mkdir -p target/qperf-manual-riscv64 && timeout 20s qemu-system-riscv64 -nographic -cpu rv64 -machine virt -kernel target/riscv64gc-unknown-none-elf/debug/starryos.bin -device virtio-blk-pci,drive=disk0 -drive id=disk0,if=none,format=raw,file=target/riscv64gc-unknown-none-elf/rootfs-riscv64.img -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -plugin /qperf/target/release/libqperf.so,freq=99,out=target/qperf-manual-riscv64/qperf.bin,max_depth=64,queue=4096 || true'
```

这里使用 `timeout 20s` 是为了让 StarryOS 在最小场景中运行一段时间并产生采样数据，然后由外部超时停止 QEMU。

### 7.4 手动调用 analyzer

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /work \
  b7c4600e825d \
  bash -lc '/qperf/target/release/qperf-analyzer -e target/riscv64gc-unknown-none-elf/debug/starryos target/qperf-manual-riscv64/qperf.bin target/qperf-manual-riscv64/stack.folded'
```

检查输出：

```bash
wc -l target/qperf-manual-riscv64/stack.folded
grep -E 'ax_task|ax_mm|ax_plat|syscall|starry|starry_api' \
  target/qperf-manual-riscv64/stack.folded \
  | head
```

该阶段的目标是证明 qperf plugin、QEMU、StarryOS kernel ELF 和 analyzer 之间的链路是可用的。完成该验证后，才继续做 TGOSKits 集成。

## 8. 实际验证命令与结果

以下命令均在 Docker 镜像 `b7c4600e825d` 中执行。

### 8.1 TGOSKits 格式检查

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo fmt --check'
```

结果：通过。

### 8.2 axbuild clippy

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo xtask clippy --package axbuild'
```

结果：通过，4 项检查通过。

### 8.3 axbuild targeted tests

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo test -p axbuild'
```

结果：通过，207 个测试通过。

### 8.4 qperf 格式检查与 clippy

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work/tools/qperf \
  b7c4600e825d \
  bash -lc 'cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings'
```

结果：通过。

### 8.5 cargo starry perf help

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --help'
```

结果：通过。帮助信息中包含：

```text
--arch
--freq
--out
--format
--max-depth
--timeout
```

### 8.6 riscv64 集成运行

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format folded --out target/qperf/integration-riscv64-final'
```

结果：通过。QEMU 在运行 20 秒后由 timeout 停止，期间已经产生 qperf 原始采样数据，analyzer 成功生成 folded stack。

生成文件：

```text
target/qperf/integration-riscv64-final/qemu.toml
target/qperf/integration-riscv64-final/qperf.bin
target/qperf/integration-riscv64-final/stack.folded
target/qperf/integration-riscv64-final/summary.txt
```

`stack.folded` 中包含 1630 行，并能看到 StarryOS / ArceOS 相关符号，例如：

```text
ax_plat::call_main
ax_task::api::current_may_uninit
ax_mm::backend::Backend
ax_page_table_multiarch
```

### 8.7 format all 验证

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format all --out target/qperf/integration-riscv64-all'
```

结果：主流程通过。当前容器内没有 flamegraph 生成工具，因此没有生成 `flamegraph.svg`，但命令没有失败，仍然保留了 `stack.folded` 和 `summary.txt`。

## 9. 用户使用说明

### 9.1 最小使用方式

```bash
cargo starry perf --arch riscv64
```

该命令会：

1. 构建 qperf plugin；
2. 构建 qperf-analyzer；
3. 构建 StarryOS debug kernel；
4. 准备 rootfs；
5. 加载并 patch QEMU 配置；
6. 注入 qperf plugin；
7. 启动 QEMU 运行 StarryOS；
8. 收集 qperf 原始采样；
9. 生成 `stack.folded`；
10. 在可能时生成 `flamegraph.svg`；
11. 打印最终报告路径。

### 9.2 常用命令

生成 folded stack：

```bash
cargo starry perf --arch riscv64 --format folded
```

指定输出目录：

```bash
cargo starry perf --arch riscv64 --out target/qperf/my-run
```

延长运行时间：

```bash
cargo starry perf --arch riscv64 --timeout 60
```

调整采样频率：

```bash
cargo starry perf --arch riscv64 --freq 100
```

调整最大回溯深度：

```bash
cargo starry perf --arch riscv64 --max-depth 128
```

生成 folded stack 并尝试生成 SVG：

```bash
cargo starry perf --arch riscv64 --format all
```

## 10. 当前未完成事项与限制

### 10.1 cargo starry qemu --perf

`cargo starry qemu --perf` 尚未实现。

原因是当前已经通过独立的 `cargo starry perf` 完成 P0 目标，并且 `qemu` 子命令涉及既有运行路径和调试参数行为。为了避免破坏原有 `cargo starry qemu` 行为，本次没有把 qperf 语法糖合并到 `qemu` 子命令中。

后续可以在 `qemu` 子命令中增加 `--perf`，内部复用 `perf.rs` 中的 qperf 工具构建、输出目录和 plugin 参数生成逻辑。

### 10.2 pprof 输出

`--format pprof` 已预留，但还未实现。当前如果用户请求 pprof，会收到明确错误，而不是静默失败。

### 10.3 loongarch64 验证

`--arch loongarch64` 在 CLI 和 QEMU 配置选择层面已经接入，但当前环境中未完成完整 loongarch64 qperf 运行验证。

后续验证需要确认：

- 容器内 QEMU 是否支持 loongarch64 TCG plugin；
- StarryOS loongarch64 kernel 是否可在当前镜像中完整构建和启动；
- analyzer 能否正确解析 loongarch64 采样栈。

### 10.4 QEMU 被 timeout 停止时的 plugin summary

如果 QEMU 是被外部 `timeout` 终止，plugin 的正常 shutdown callback 可能没有机会完整执行。因此 plugin 内部 dropped sample 总数可能无法写入 plugin-side summary。

TGOSKits 集成侧仍会写入 `summary.txt`，其中包含：

- qperf 原始数据路径；
- folded stack 路径；
- flamegraph 路径；
- kernel ELF 路径；
- QEMU 配置路径；
- folded stack 行数；
- 本次运行参数。

### 10.5 workspace 全量测试

由于 workspace 规模较大，本次没有执行全量 `cargo test`。已执行并通过的验证包括：

- `cargo fmt --check`；
- `cargo xtask clippy --package axbuild`；
- `cargo test -p axbuild`；
- qperf workspace clippy；
- `cargo starry perf --help`；
- riscv64 qperf 集成运行。

## 11. 修改文件清单

本次主要修改和新增文件如下：

```text
docs/qperf-starryos-integration-report.md
scripts/axbuild/src/starry/mod.rs
scripts/axbuild/src/starry/perf.rs
tools/qperf/.gitignore
tools/qperf/Cargo.lock
tools/qperf/Cargo.toml
tools/qperf/README.md
tools/qperf/analyzer/Cargo.toml
tools/qperf/analyzer/src/main.rs
tools/qperf/src/lib.rs
tools/qperf/src/profiler.rs
tools/qperf/src/reg.rs
```

本中文报告新增为：

```text
qperf-starryos-integration-report.zh.md
```

## 12. 复现步骤汇总

从干净工作区复现 riscv64 folded stack：

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format folded --out target/qperf/repro-riscv64'
```

检查输出文件：

```bash
ls -lh target/qperf/repro-riscv64
```

检查 folded stack 行数：

```bash
wc -l target/qperf/repro-riscv64/stack.folded
```

检查内核符号：

```bash
grep -E 'ax_task|ax_mm|ax_plat|syscall|starry|starry_api' \
  target/qperf/repro-riscv64/stack.folded \
  | head
```

查看 summary：

```bash
cat target/qperf/repro-riscv64/summary.txt
```

安装 flamegraph 工具后生成 SVG：

```bash
cargo install inferno
inferno-flamegraph \
  < target/qperf/repro-riscv64/stack.folded \
  > target/qperf/repro-riscv64/flamegraph.svg
```

或者直接运行：

```bash
cargo starry perf \
  --arch riscv64 \
  --timeout 20 \
  --format all \
  --out target/qperf/repro-riscv64-all
```

## 13. 结论

本次工作完成了 P0 目标：

- qperf 已经可以在 StarryOS riscv64 场景中实际跑通；
- qperf 无界内存增长风险已经通过 bounded channel、非阻塞丢样和 dropped sample 统计处理；
- frame pointer unwinding 已增加最大深度和基础合法性检查；
- `cargo starry perf --arch riscv64` 已实现；
- 能生成非空 `stack.folded`，并能看到 StarryOS / ArceOS 内核符号。

同时完成了多个 P1 项：

- 支持 `--freq`；
- 支持 `--out`；
- 支持 `--max-depth`；
- 支持 `--timeout`；
- 支持 `--format folded/svg/all`；
- 在 flamegraph 工具缺失时优雅降级；
- 输出 `summary.txt`；
- analyzer 增强了坏数据容忍度和符号解析缓存。

暂未完成的 P2 项包括：

- `cargo starry qemu --perf`；
- pprof 输出；
- loongarch64 完整运行验证；
- 更完整的 CI 覆盖。
