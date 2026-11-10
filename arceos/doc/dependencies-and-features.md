# ArceOS 中依赖与 Feature 的传递

本文依据本仓库的 Cargo 配置，简述 ArceOS 如何通过依赖（`dependencies`）和功能开关（`features`）组合内核。

## 1. dependencies：建立组件之间的关系

`[dependencies]` 声明当前 crate 使用哪些组件；这些组件还可以依赖其他 crate，形成传递依赖。例如：

```text
应用 → axstd → arceos_api → axhal → 更底层组件
             └→ axfeat → axruntime → axhal
```

这是一张依赖图，同一个组件可以被多条路径引用。

根目录 [Cargo.toml](../Cargo.toml) 的 `[workspace.dependencies]` 统一声明路径、版本和部分 feature；成员通过 `workspace = true` 继承：

```toml
# 工作空间中统一声明
[workspace.dependencies]
axfs = { path = "modules/axfs" }

# 某个成员中使用
[dependencies]
axfs = { workspace = true, optional = true }
```

仅列入 `workspace.members` 或 `workspace.dependencies`，不会自动让所有应用依赖该组件。成员仍需声明依赖；`optional = true` 表示需要通过 feature 激活该依赖边。

依赖来源包括本地路径（如 `axfs`）、crates.io 版本（如 `axio = "0.1"`）和 Git 仓库（如 `axtask` 使用的 `scheduler`）。根配置还通过 `[patch.crates-io]` 将 `kernel_guard` 替换为仓库内的实现。

## 2. features：沿依赖边传递功能选择

Feature 属于各自的 crate，不会因为名称相同而自动传播，必须显式转发。常见写法如下：

| 写法 | 含义 |
| --- | --- |
| `paging = ["alloc"]` | 开启当前 crate 的 `paging` 时，同时开启本 crate 的 `alloc` |
| `fs = ["axfeat/fs"]` | 开启依赖 `axfeat` 的 `fs` feature；普通的 `依赖/feature` 写法也会激活对应可选依赖 |
| `fs = ["dep:axfs"]` | 激活可选依赖 `axfs`；使用 `dep:` 可避免为该依赖自动生成同名隐式 feature |
| `myfs = ["axfs?/myfs"]` | 仅当 `axfs` 已被其他路径激活时，开启它的 `myfs`，本项不会主动引入 `axfs` |
| `default = ["tlsf"]` | 默认启用当前 crate 的 `tlsf` |

未使用 `dep:` 隐藏隐式 feature 时，可选依赖也能通过同名 feature 激活。例如 `axruntime` 中的 `alloc = ["axalloc"]`。

Feature 通常是累加的：在同一次解析中，同一包的 feature 会按适用的构建上下文合并。一条依赖路径启用的功能，不会被另一条路径的空 feature 列表关闭。本项目使用 `resolver = "2"`，可减少构建依赖、开发依赖和未启用目标平台等上下文之间不必要的 feature 合并，但不意味着各条普通依赖路径互相隔离。

`default-features = false` 只关闭该依赖声明默认请求的 feature，不能撤销其他路径已请求的默认功能。例如本仓库 `axalloc` 默认启用 `tlsf`，额外请求 `alloc-slab` 不代表自动关闭 `tlsf`。

## 3. ArceOS 的主要传递层次

| 层次 | 主要职责 |
| --- | --- |
| 应用 | 在依赖声明或构建参数中选择需要的功能 |
| `axstd` / `axlibc` | 提供 Rust 类标准库接口或 C 库接口，并转发功能选择 |
| `arceos_api` / `arceos_posix_api` | 按功能引入模块、提供对应 API，并向 `axfeat` 转发 |
| `axfeat` | 集中描述内核功能之间的组合关系，将 feature 分发给模块 |
| `axruntime` 与各模块 | 按 feature 编译实现、选择底层组件，并完成启动初始化 |

Rust 应用的主要入口是 [axstd](../ulib/axstd/Cargo.toml)，它同时依赖 [arceos_api](../api/arceos_api/Cargo.toml) 和 [axfeat](../api/axfeat/Cargo.toml)，因此 API 与内核配置可以沿多条路径汇合。C 应用主要通过 `axlibc → arceos_posix_api` 进入，并同样使用 `axfeat`。

本仓库还有一项需要注意的统一配置：`workspace.dependencies` 中的 `arceos_posix_api` 已请求 `features = ["fs", "fd"]`。继承该依赖的成员也会获得这两个功能，不能只看 `axlibc` 自身的空默认 feature 就判断文件系统未启用。

## 4. 示例：开启文件系统 fs

假设应用通过以下依赖请求文件系统：

```toml
[dependencies]
axstd = { workspace = true, features = ["fs"] }
```

根据当前配置，主要传递过程是：

```text
axstd/fs
├── arceos_api/fs
│   ├── 激活 axfs、axdriver
│   └── axfeat/fs
└── axfeat/fs
    ├── alloc → axalloc、axruntime/alloc
    ├── paging → alloc、axhal/paging、axruntime/paging
    ├── axdriver/virtio-blk
    ├── 激活 axfs
    └── axruntime/fs → 激活 axdriver、axfs
```

继续向下，`axruntime/paging` 引入 `axmm`；`axfs` 的依赖声明请求 `axdriver/block`，且默认开启 `devfs`、`ramfs`、`fatfs`、`procfs`、`sysfs`。因此，顶层一个 `fs` 会带来内存分配、分页、块设备和具体文件系统实现等一组传递依赖。

最终，[axruntime/src/lib.rs](../modules/axruntime/src/lib.rs) 使用 `#[cfg(feature = "...")]` 控制启动代码：例如 `alloc` 控制分配器初始化，`paging` 控制内存管理初始化，`fs` 控制文件系统初始化。依赖决定“哪些组件参与构建”，feature 决定“组件的哪些实现和初始化路径生效”。

另一个例子是 `axstd/sched_rr → axfeat/sched_rr`：它会启用 `axtask/sched_rr` 和中断支持，前者继续开启 `multitask` 与 `preempt`。因此，选择调度策略也会带出所需的任务管理和抢占功能。

## 5. 构建入口与检查方法

[scripts/make/features.mk](../scripts/make/features.mk) 将 `FEATURES` 转换为 Cargo feature 参数。Rust 应用主要使用 `axstd/` 前缀；C 应用按功能分别转发到 `axfeat/` 或 `axlibc/`，并可读取应用的 `features.txt`。脚本还根据 `LOG`、`BUS`、`SMP` 补充功能；`APP_FEATURES` 用于应用自身的 feature。

查看某个功能时，建议按“应用声明 → 用户库 → API / axfeat → 模块 → 底层组件”的顺序追踪，同时检查各层的默认 feature 和可选依赖。在 `arceos/` 目录可使用：

```sh
# 查看 axstd 开启 fs 后的依赖及 feature 关系
cargo tree -p axstd --features fs -e features

# 反向查看哪些依赖路径请求了 axfs 及其 feature
cargo tree -p axstd --features fs -e features -i axfs
```

检查实际应用时，应改用对应应用包、目标平台和构建 feature，以匹配实际构建上下文；上述命令需要本机已安装 Cargo 且相关依赖可用。
