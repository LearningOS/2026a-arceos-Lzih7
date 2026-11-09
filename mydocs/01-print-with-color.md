# 第一题：彩色输出解决方案

本文对应 `./scripts/test-print.sh` 检查的 `print_with_color` 练习，只说明解决方案，不修改练习源码。

## 1. 题目目标

让控制台输出带颜色的 `Hello, Arceos!`。代码入口是：

```text
arceos/exercises/print_with_color/src/main.rs
```

当前程序使用普通字符串：

```rust
println!("[WithColor]: Hello, Arceos!");
```

`[WithColor]` 只是文字标签，本身不会改变颜色。

## 2. 实现方法

在输出字符串中加入 ANSI SGR 转义序列。将入口文件中的 `main` 函数改成：

```rust
#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    println!("\x1b[32m[WithColor]: Hello, Arceos!\x1b[0m");
}
```

保留文件开头的属性和 `use axstd::println;`。这道题只需要修改这一行输出，不需要修改内核组件、Cargo 配置或测试脚本。

| 字符串部分 | 作用 |
| --- | --- |
| `\x1b` | Rust 字符串转义，表示 ESC 字节，数值为十六进制 `1b` |
| `[32m` | 与 ESC 组合，设置前景色为绿色 |
| `[WithColor]: Hello, Arceos!` | 实际显示的文字 |
| `\x1b[0m` | 重置显示属性，避免影响后续输出 |

也可以用 `31` 表示红色、`34` 表示蓝色。这里选择绿色即可。

注意源码中写的是 `\x1b`，不要写成 `\\x1b`；后者会输出可见的反斜杠和文字，无法形成 ESC 字节。

## 3. 为什么这样能生效

启用 `axstd` 时，这个程序通过 `use axstd::println;` 使用 ArceOS 提供的输出宏。可以沿下面的路径阅读实现：

1. `arceos/ulib/axstd/src/macros.rs`：查看 `println!` 如何添加换行并交给输出实现。
2. `arceos/ulib/axstd/src/io/stdio.rs`：查看 `__print_impl` 如何写出格式化内容。
3. `arceos/api/arceos_api/src/imp/mod.rs`：查看控制台 API 如何调用底层控制台输出。

本题的关键是把颜色控制字节和问候语一起写到控制台。支持 ANSI 的终端接收到这些字节后改变文字颜色；不需要在 ArceOS 内核中实现文字绘制或颜色解析。

## 4. 如何验证

在宿主机仓库根目录启动课程容器：

```sh
./scripts/docker.sh
```

进入容器后，在仓库根目录执行：

```sh
./scripts/test-print.sh
```

如果已经通过 VS Code Dev Containers 进入课程环境，直接执行测试命令即可。

测试脚本会创建运行所需的镜像，通过 QEMU 运行本题，并检查捕获输出的最后 20 行：

- 是否包含 ESC 加 `[` 的控制序列前缀。
- 去除 SGR 控制序列后，是否仍包含 `Hello, Arceos!`。

预期结果是终端显示绿色问候语，脚本最后输出：

```text
Has color
Has Hello, Arceos!
print_with_color pass
```

脚本检查的是输出字节和文字；阅读代码时也要确认颜色序列完整，并且有重置序列。

## 5. 常见问题

| 现象 | 检查方法 |
| --- | --- |
| `No color` | 确认使用了 `\x1b[32m`，没有把反斜杠重复转义 |
| `No Hello, Arceos!` | 确认问候语大小写、逗号、空格和感叹号保持一致 |
| 后续日志也变绿 | 确认问候语后包含 `\x1b[0m` |
| 构建阶段报错 | 先确认在课程容器中运行；构建失败不能据此判断颜色代码有误 |

## 6. 完成本题应掌握什么

- 找到练习应用入口，区分应用逻辑与内核组件。
- 理解 `axstd::println!` 是 ArceOS 提供的接口。
- 理解转义序列是输出数据，终端负责解释颜色。
- 阅读测试脚本，依据明确的检查条件验证实现。

本文依据仓库现有源码和测试脚本编写，尚未应用代码修改或运行 QEMU 测试。
