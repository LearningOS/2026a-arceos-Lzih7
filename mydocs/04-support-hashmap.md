# 第四题：为 axstd 支持 HashMap

本文说明本次如何让 [support_hashmap 练习](../arceos/exercises/support_hashmap/src/main.rs) 在 ArceOS 裸机环境中直接使用：

```rust
use std::collections::HashMap;
```

实现分为三部分：`axstd` 提供兼容的导出路径，`hashbrown` 提供哈希表算法，ArceOS 全局分配器提供存储空间。

## 1. 这里的 std 实际上是什么

练习使用 `#![no_std]`，没有链接通常的 Rust 标准库。代码通过别名将 `axstd` 当作 `std` 使用：

```rust
#[macro_use]
#[cfg(feature = "axstd")]
extern crate axstd as std;
```

因此，`std::collections::HashMap` 实际查找的是 `axstd::collections::HashMap`。

原来的 [axstd/src/lib.rs](../arceos/ulib/axstd/src/lib.rs) 直接导出：

```rust
pub use alloc::{boxed, collections, format, string, vec};
```

这里的 `collections` 来自 Rust 的 `alloc` 库，它提供 `BTreeMap`、`BTreeSet`、`VecDeque` 等集合，但不提供 `HashMap`。所以仅开启堆分配功能，还不能让原来的导入语句成立。

## 2. 本次修改：自建 collections 模块

现在将 `collections` 从原来的整体重导出中移出，改为独立模块：

```rust
#[cfg(feature = "alloc")]
#[doc(no_inline)]
pub use alloc::{boxed, format, string, vec};

#[cfg(feature = "alloc")]
pub mod collections;
```

新增的 [collections.rs](../arceos/ulib/axstd/src/collections.rs) 内容为：

```rust
pub use alloc::collections::*;
pub use hashbrown::{hash_map, hash_set, HashMap, HashSet};
```

第一行保留原有集合的访问路径，第二行补充哈希集合及其配套模块。例如以下路径都可以使用：

```rust
use std::collections::{BTreeMap, HashMap, HashSet};
use std::collections::hash_map::Entry;
```

这层只是导出接口，实际的哈希表算法由 `hashbrown` 实现，没有在内核中重复编写桶管理和冲突处理逻辑。

## 3. Cargo 依赖和 feature 如何接通

[axstd/Cargo.toml](../arceos/ulib/axstd/Cargo.toml) 增加了可选依赖：

```toml
[dependencies]
hashbrown = { version = "0.15.5", default-features = false, features = ["default-hasher"], optional = true }
```

同时在已有的 `alloc` feature 中加入 `dep:hashbrown`：

```toml
[features]
alloc = ["arceos_api/alloc", "axfeat/alloc", "axio/alloc", "dep:hashbrown"]
```

各项含义如下：

| 配置 | 作用 |
| --- | --- |
| `optional = true` | 不需要堆分配的应用不必通过 axstd 引入该依赖 |
| `dep:hashbrown` | 开启 axstd 的 `alloc` 时激活 hashbrown 依赖 |
| `default-features = false` | 关闭 hashbrown 的默认功能组合，明确选择需要的功能 |
| `default-hasher` | 提供默认哈希构造器，使 `HashMap::new()` 可以直接使用 |

练习原有配置已经请求 `axstd/alloc`，因此应用代码和练习 Cargo 配置均无需修改。传递关系为：

```text
support_hashmap → axstd/alloc
                 ├── dep:hashbrown → default-hasher → foldhash
                 ├── arceos_api/alloc
                 ├── axfeat/alloc → axalloc、axruntime/alloc
                 └── axio/alloc
```

锁文件记录本次解析结果：`hashbrown 0.15.5`、`foldhash 0.1.5`。`collections` 模块本身也受 `alloc` feature 控制。

## 4. 哈希表底层如何存储和查找

`hashbrown` 使用 SwissTable 系列的哈希表设计。可以将其理解为两类信息：

- 存放键值对的桶。
- 描述桶状态、保存部分哈希信息的控制字节。

插入键值对时，主要过程为：

```text
对 key 计算哈希值
    → 根据哈希值确定探测起点
    → 检查控制信息，寻找已有键或可用桶
    → 用键相等比较确认是否为同一个键
    → 更新已有值，或存入新键值对
```

不同键可能具有相同哈希值，因此哈希值相同不代表键相同，还需要 `Eq` 比较。发生冲突时继续探测，不会直接覆盖另一个不同键。

表容量不足时，需要申请更大的存储区域，把已有元素重新安置，再释放旧存储。探测与分组匹配由库处理，具体优化取决于目标平台；应用不需要为此调用操作系统接口。

本次启用的默认哈希构造器是 `foldhash::fast::RandomState`，无需 ArceOS 额外提供系统随机数接口。它不提供 Rust 标准库默认哈希器同等的 HashDoS 防护；处理恶意可控键时，应选择适合该场景的哈希器与种子来源。

## 5. 桶数组和 String 的内存从哪里来

本题有两种主要的堆内存申请：

```rust
let key = format!("key_{value}"); // 为 String 内容申请内存
m.insert(key, value);            // 表首次分配或扩容时申请桶存储
```

`HashMap::new()` 通常先创建空表，不立即申请桶存储。并非每次插入都会扩容；只有当前容量不足等需要调整存储时才会申请新的表空间。

主要分配路径为：

```text
format! / String              hashbrown 的桶与控制信息存储
        └─────────────┬─────────────┘
                      ↓
             Rust 全局分配接口
                      ↓
          axalloc::GlobalAllocator（GlobalAlloc）
                      ↓
              加锁后的字节分配器
                      ↓ 堆空间不足时
               页分配器提供更多内存
```

[axalloc/src/lib.rs](../arceos/modules/axalloc/src/lib.rs) 使用两级分配：优先向字节分配器申请；不足时通过页分配器取得一块更大的区域，加入字节堆后重试。默认字节分配算法由 `axalloc` 的默认 `tlsf` feature 选择。

启动时，`axruntime` 查找空闲物理内存区域，将起点通过 `phys_to_virt()` 转成内核直接映射地址，再初始化全局分配器。因此哈希表拿到的是可以用于指针访问的内核虚拟地址，背后有实际的物理内存。

这里使用普通的 `axalloc`，不是前一题的 `alt_axalloc / EarlyAllocator`。哈希表也不会直接操作页表或调用 `alloc_pages()`，这些职责由底层分配层完成。

## 6. 所有权、释放与并发

执行 `m.insert(key, value)` 后，`String` 键的所有权移入哈希表，键的字符缓冲区无需仅因所有权转移而复制一份。

哈希表离开作用域时，Rust 的析构流程释放各个键值对象，并释放表自身的存储。这些释放最终进入全局分配器的 `dealloc()`。

分配器的锁保护的是堆内部状态，不会自动让 `HashMap` 支持多个线程同时修改。若多个任务共享并修改同一个表，仍需在表外使用合适的同步机制。本题只有一个执行流程，不需要额外为表加锁。

本练习不依赖文件系统、网络或用户地址空间，也不要求开启多任务。`println!` 另外使用控制台输出支持，用于显示测试进度和结果。

## 7. 修改文件与验证结果

| 文件 | 修改内容 |
| --- | --- |
| [axstd/Cargo.toml](../arceos/ulib/axstd/Cargo.toml) | 增加 hashbrown 可选依赖，由 alloc feature 激活 |
| [axstd/src/lib.rs](../arceos/ulib/axstd/src/lib.rs) | 将 collections 改为本地模块 |
| [axstd/src/collections.rs](../arceos/ulib/axstd/src/collections.rs) | 保留原有集合，补充 HashMap、HashSet 和配套模块 |
| [Cargo.lock](../arceos/Cargo.lock) | 记录 hashbrown 的启用依赖及新增 foldhash |

已在课程容器内运行仓库脚本：

```sh
./scripts/test-support_hashmap.sh
```

RISC-V QEMU 测试插入 50,000 个 `String → u32` 键值对，再遍历验证键中的数字与值一致。测试完成析构释放后输出：

```text
Running memory tests...
test_hashmap() OK!
Memory tests run OK!
support_hashmap pass
```

脚本退出状态为 0，验证了导出路径、裸机构建、堆分配、插入、遍历与退出释放的集成。该练习没有单独覆盖删除、Entry API 或多线程共享等行为。

另外尝试的宿主机 `cargo check -p axstd --features alloc` 未通过：现有 `x86_64 0.14.13` 依赖的 `Step::steps_between` 签名与工具链不兼容。本次未修改该依赖；上述 RISC-V 目标构建和运行均已通过。
