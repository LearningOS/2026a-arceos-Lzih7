# 第三题：替代内存分配器实现与验证

本题对应 README 第 40 行的 `alt_alloc` 练习。实现已完成：5 项分配器单元测试全部通过，课程脚本 `./scripts/test-alt_alloc.sh` 在 RISC-V QEMU 中输出 `alt_alloc pass`。

## 1. 题目要求与原始问题

[练习入口](../arceos/exercises/alt_alloc/src/main.rs) 的核心代码是：

```rust
const N: usize = 3_000_000;
let mut v = Vec::with_capacity(N);
for i in 0..N {
    v.push(i);
}
v.sort();
for i in 0..N - 1 {
    assert!(v[i] <= v[i + 1]);
}
```

这里的元素类型是 `usize`。RISC-V 64 位环境中，一个元素占 8 字节，仅向量元素区就需要 `3_000_000 × 8 = 24_000_000` 字节，约 22.9 MiB。

任务是使练习真正通过替代全局分配器完成内存申请、写入和排序检查，最后输出：

```text
Running bump tests...
Bump tests run OK!
```

原始代码已经通过 `alt_alloc` feature 接通启动初始化和全局分配器注册，但 `EarlyAllocator` 还是一个空结构，其分配、释放和统计方法均为 `todo!()`；外层追加内存接口也为 `unimplemented!()`。因此需要补齐底层分配机制，不能只修改应用的成功输出。

本次仅修改 Rust 实现并补充文档，没有修改练习数据规模、成功标志、Cargo 配置、锁文件或评测脚本。

## 2. 从 Vec 到 EarlyAllocator 的调用路径

```text
exercises/alt_alloc 开启 axstd/alt_alloc
  → axfeat/alt_alloc
  → axruntime/alt_alloc 与 alt_axalloc

启动阶段：
axruntime::init_allocator
  → 查找标记为 FREE 的内存区域
  → 用最大的区域调用 alt_axalloc::global_init
  → 把其他区域交给 global_add_memory（如果存在）

应用申请内存：
Vec::with_capacity(N)
  → Rust alloc 的全局分配接口
  → alt_axalloc::GlobalAllocator::alloc
  → 获取 SpinNoIrq 锁
  → EarlyAllocator::alloc(Layout)
  → 返回向量元素区的指针
```

`#[global_allocator]` 把 `alt_axalloc` 中的静态分配器注册给裸机目标。应用直接使用 `alloc::vec::Vec`，也会走这个全局分配器，不需要显式调用 `EarlyAllocator`。

外层接口使用 `&self`，通过 `SpinNoIrq` 提供内部可变性；取得锁后，才能调用底层要求 `&mut self` 的分配方法。`EarlyAllocator` 本身不额外实现并发锁。

相关文件：

| 文件 | 职责 |
| --- | --- |
| [练习 Cargo.toml](../arceos/exercises/alt_alloc/Cargo.toml) | 已声明使用 `alt_alloc` 功能 |
| [axruntime](../arceos/modules/axruntime/src/lib.rs) | 在应用开始前提供可分配内存 |
| [alt_axalloc](../arceos/modules/alt_axalloc/src/lib.rs) | 全局注册、锁保护和分配接口包装 |
| [bump_allocator](../arceos/modules/bump_allocator/src/lib.rs) | 实际选择地址、推进游标和统计空间 |
| [单元测试](../arceos/modules/bump_allocator/src/tests.rs) | 验证对齐、回收、边界和混合分配 |

## 3. 数据结构：一块内存，两端分配

分配器管理一段连续内存，字节申请从低地址向高地址增长，页申请从高地址向低地址增长：

```text
低地址                                               高地址
start            byte_pos              page_pos         end
  | 已占用字节区域 |        可用区域        | 已保留页区域 |
                   →                    ←
```

核心字段：

```rust
pub struct EarlyAllocator<const SIZE: usize> {
    start: usize,
    end: usize,
    byte_pos: usize,
    page_pos: usize,
    allocations: usize,
}
```

| 字段 | 含义 |
| --- | --- |
| `SIZE` | 页大小；外层分配器使用 4096 字节 |
| `start` / `end` | 管理区域的起止地址，区间右端不包含在内 |
| `byte_pos` | 下次字节申请的搜索起点 |
| `page_pos` | 页分配区域的低地址边界 |
| `allocations` | 尚未释放的字节分配数量，不是字节总数 |

有效状态保持 `start <= byte_pos <= page_pos <= end`。两种分配都检查中间区域是否足够，因此不能越过对方已经保留的内存。

初始化会检查页大小是否为 2 的幂、地址加法是否溢出，并把高端 `end` 向下对齐到页边界。最后不足一页的尾部不纳入管理；`start` 可以不按页对齐，后续字节申请按各自要求对齐。

## 4. 字节分配：对齐后推进低端游标

`Layout` 给出申请的字节数和地址对齐要求。例如 `Layout::from_size_align(24, 8)` 表示申请 24 字节，返回地址必须是 8 的倍数。

分配过程是：

```text
aligned = align_up(byte_pos, layout.align())
next    = aligned + layout.size()

如果 next > page_pos：返回 NoMemory
否则：byte_pos = next，allocations += 1，返回 aligned
```

向上对齐的计算为：

```rust
(byte_pos + align - 1) & !(align - 1)
```

实现使用 `checked_add` 检查地址运算，防止溢出后错误地返回一块看似可用的内存。所有检查成功后才更新游标，失败申请不会消耗空间。零长度申请在这个底层接口中返回 `InvalidParam`；Rust `GlobalAlloc` 的正常调用要求非零大小。

返回类型 `NonNull<u8>` 表示非空字节指针。分配器只计算和保留地址，`Vec` 随后的 `push` 才会把元素写到这块内存中。

## 5. 字节释放：全部释放后整体复用

这个 early bump allocator 不为每个小块维护空闲链表，因此不能在任意释放后立即复用空洞。

`dealloc` 的策略是：

1. 将尚存字节分配数量减一。
2. 若仍有分配存活，保持 `byte_pos`。
3. 若数量为零，将 `byte_pos` 重置为 `start`，重新使用整个低端字节区域。

例如先申请 A、B，再释放 A，此时新申请 C 仍放在 B 后面，不能覆盖还在使用的 B。等 B、C 也释放后，低端游标才能整体回退。

释放接口不记录或核对每个指针的所有权。调用方必须像使用普通分配器一样，保证传入有效分配并且每块只释放一次。`used_bytes()` 表示当前仍被保留的低端区域，包含对齐填充以及暂不能复用的空洞，不是所有存活对象大小的精确加总。

## 6. 页分配：向下寻找连续页

`alloc_pages(num_pages, align_pow2)` 接收页数和以字节表示的对齐要求。实现首先检查页数非零、对齐为 2 的幂，并使用 `checked_mul` 计算总字节数。

然后进行：

```text
size  = num_pages × SIZE
align = max(align_pow2, SIZE)
next  = align_down(page_pos - size, align)

如果 next < byte_pos：返回 NoMemory
否则：page_pos = next，返回 next
```

对齐额外跳过的空间也一并保留，后续申请不会落入已分配页或其对齐间隙。

按原设计，`dealloc_pages` 不回收页，`page_pos` 不会上移。字节区域整体回收也只重置 `byte_pos`，不会影响高端已保留页。

## 7. 用一组地址观察变化

假设管理 `[0x1000, 0x9000)`，页大小为 `0x1000`：

| 操作 | 返回地址 | `byte_pos` | `page_pos` | 字节分配计数 |
| --- | --- | --- | --- | --- |
| 初始化 | — | `0x1000` | `0x9000` | 0 |
| 申请 24 字节，对齐 8 | `0x1000` | `0x1018` | `0x9000` | 1 |
| 申请 1 页，对齐 4096 | `0x8000` | `0x1018` | `0x8000` | 1 |
| 申请 16 字节，对齐 64 | `0x1040` | `0x1050` | `0x8000` | 2 |
| 释放第一个字节块 | — | `0x1050` | `0x8000` | 1 |
| 释放第二个字节块 | — | `0x1000` | `0x8000` | 0 |

最后一步可复用的范围是 `[0x1000, 0x8000)`；高端页仍然保留。

## 8. 统计与追加内存接口

统计函数分别表示：

| 接口 | 计算方式 |
| --- | --- |
| `total_bytes()` | `end - start` |
| `used_bytes()` | `byte_pos - start` |
| `available_bytes()` | `page_pos - byte_pos` |
| `total_pages()` | `(end - start) / SIZE` |
| `used_pages()` | `(end - page_pos) / SIZE`，包含高端对齐间隙 |
| `available_pages()` | `(page_pos - byte_pos) / SIZE` |

字节和页共享一块内存，因此有页分配时，`total_bytes()` 不等于 `used_bytes() + available_bytes()`，还需计入高端保留区。

本次同时补齐外层 `add_memory`、`global_add_memory` 的转发。底层只有一对游标，不支持任意不连续的多块内存，只允许安全扩展现有区域：

- 分配器尚未初始化时，可用新增区域完成初始化。
- 尚未分配页时，可在高地址端追加连续区域。
- 没有存活字节块时，可在低地址端追加连续区域。
- 重叠区域返回 `MemoryOverlap`；不连续区域或会越过已有分配的扩展返回 `InvalidParam`。

本次 RISC-V 平台的默认空闲内存是内核结束地址之后的一段连续区域，可以直接初始化。这个实现保留了 early allocator 的简单模型，没有把它扩展成通用多区域分配器。

## 9. 验证结果

### 9.1 分配器单元测试

在课程容器的 `arceos/` 目录执行：

```sh
cargo test -p bump_allocator --lib --offline
```

结果：**5 passed，0 failed**。测试覆盖：

1. 字节块满足对齐，部分释放后存活内容保持不变，全部释放后可复用。
2. 字节块和页可以恰好用完区域，后续失败申请不越界；字节回收不覆盖页。
3. 页地址满足更大对齐要求，对齐间隙保留，释放页不导致复用。
4. 未初始化、零长度、无效对齐、页数乘法溢出和过大申请均被拒绝，状态保持不变。
5. 连续内存安全扩展成功，重叠、不连续和跨越已有分配的扩展被拒绝。

### 9.2 课程 QEMU 测例

在课程容器的仓库根目录执行：

```sh
./scripts/test-alt_alloc.sh
```

脚本会重新生成测试镜像，并执行 `make A=exercises/alt_alloc/ run`。本次使用课程容器，在 `riscv64-qemu-virt`、release、单核配置下完成测试，退出状态为 0，末尾输出：

```text
Running bump tests...
Bump tests run OK!
alt_alloc pass
```

这验证了替代分配器与启动流程、Rust 全局分配接口和大向量工作负载的集成。练习的输入本身按升序写入，因此单靠这个应用不能充分验证复杂的分配交错；对齐、回收与耗尽行为由前述单元测试补充验证。

## 10. global_allocator：让 Rust 容器使用这个分配器

`EarlyAllocator` 实现了分配算法，但 `Vec`、`Box` 等类型不会自动知道它的存在。[alt_axalloc/src/lib.rs](../arceos/modules/alt_axalloc/src/lib.rs) 通过以下三层把两者连接起来：

```text
Rust 的 Vec、Box 等堆分配请求
    ↓ 全局分配接口
GlobalAllocator：实现 GlobalAlloc，负责接口转换和加锁
    ↓
EarlyAllocator：计算地址、检查空间并移动游标
```

### 10.1 类型、静态对象、注册属性和访问函数

这几个名字相近，但作用不同：

| 名称 | 作用 |
| --- | --- |
| `GlobalAllocator` | 分配器包装类型，内部保存锁和 `EarlyAllocator` |
| `GLOBAL_ALLOCATOR` | 这个类型的一个静态实例，供整个程序共享 |
| `#[global_allocator]` | 告诉 Rust 使用指定的静态实例处理全局堆分配 |
| `global_allocator()` | 返回该静态实例的引用，供内核显式调用 |
| `global_init(start, size)` | 为这个已存在的实例设置可管理内存 |

实际声明为：

```rust
#[cfg_attr(all(target_os = "none", not(test)), global_allocator)]
static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator::new();

pub fn global_allocator() -> &'static GlobalAllocator {
    &GLOBAL_ALLOCATOR
}
```

`cfg_attr` 表示：仅在 `target_os = "none"` 且不是测试构建时，应用 `#[global_allocator]`。因此裸机程序使用它接管全局分配；普通宿主机测试构建不会通过这里替换测试程序的全局分配器。

`global_allocator()` 每次返回的都是同一个对象，不会创建新分配器。返回值中的 `'static` 表示这个引用指向的对象在整个程序运行期间一直存在。

### 10.2 创建对象不等于提供堆内存

`GlobalAllocator::new()` 内部调用 `EarlyAllocator::new()`，此时边界和游标都是零。静态实例已经存在，但尚无可用内存，也不需要先申请堆内存来创建它。

启动阶段由 `axruntime::init_allocator()` 找到空闲内存，将物理地址转换为虚拟地址，再调用：

```rust
alt_axalloc::global_init(start_vaddr, size);
```

调用关系为：

```text
global_init(start_vaddr, size)
    → GLOBAL_ALLOCATOR.init(start_vaddr, size)
    → self.inner.lock().init(start_vaddr, size)
    → 设置 EarlyAllocator 的边界和游标
```

因此，注册决定“堆申请交给谁”，初始化决定“它可以分配哪段内存”。后续 `global_add_memory()` 同样操作这个实例，只是尝试扩展管理范围。

### 10.3 GlobalAlloc 是 Rust 与底层算法之间的适配层

Rust 要求被注册的类型实现 `core::alloc::GlobalAlloc`。本项目的核心实现是：

```rust
unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Ok(ptr) = GlobalAllocator::alloc(self, layout) {
            ptr.as_ptr()
        } else {
            alloc::alloc::handle_alloc_error(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        GlobalAllocator::dealloc(
            self,
            NonNull::new(ptr).expect("dealloc null ptr"),
            layout,
        )
    }
}
```

这里有两组同名方法：trait 的 `alloc` 返回 `*mut u8`，包装类型自身的 `alloc` 返回 `AllocResult<NonNull<u8>>`。`GlobalAllocator::alloc(self, layout)` 调用后者，完成加锁和底层分配，不是在递归调用 trait 方法。

成功时把 `NonNull<u8>` 转换成原始指针交给 Rust；失败时，本实现直接调用 `handle_alloc_error` 进入分配失败处理，不向调用者返回可恢复的分配错误。释放则进行相反方向的指针转换，再交给底层的 `dealloc`。

`unsafe impl` 表示实现者负责满足分配接口的安全约定，例如返回足够大、正确对齐且不与存活分配重叠的区域。调用者也必须保证释放的指针和布局有效，不能重复释放。

### 10.4 为什么方法接收 &self 也能修改游标

全局实例被共享访问，而分配需要修改 `byte_pos`、`page_pos` 等字段，因此包装层使用：

```rust
pub struct GlobalAllocator {
    inner: SpinNoIrq<EarlyAllocator<PAGE_SIZE>>,
}

pub fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
    self.inner.lock().alloc(layout)
}
```

`SpinNoIrq` 提供内部可变性：通过共享引用获取锁后，才能独占访问内部的 `EarlyAllocator`，调用其要求 `&mut self` 的方法。锁同时避免并发修改游标；关中断保护用于防止本 CPU 的中断处理在持锁期间再次进入分配器而造成死锁。

### 10.5 Vec 的申请和释放如何落到游标上

对于本题的非零容量向量，申请和释放的主要路径是：

```text
Vec::with_capacity(N)
    → 根据元素大小、数量和对齐要求形成 Layout
    → GlobalAlloc::alloc
    → GlobalAllocator::alloc（加锁）
    → EarlyAllocator::alloc（推进 byte_pos）

Vec 离开作用域并释放缓冲区
    → GlobalAlloc::dealloc
    → GlobalAllocator::dealloc（加锁）
    → EarlyAllocator::dealloc（减少 allocations）
```

`Vec` 的元素缓冲区走字节分配接口，不会因为申请很大就自动调用本项目的 `alloc_pages()`。页分配是内核通过 `global_allocator().alloc_pages(...)` 显式使用的另一套接口。

向量释放后，底层仍遵循 bump allocator 的规则：只有所有字节分配都释放，才把 `byte_pos` 整体重置。注册成全局分配器不会改变底层的回收策略。

最终程序只能注册一个全局分配器。这里的 `alt_alloc` 和普通 `alloc` 分别接入 `alt_axalloc`、`axalloc`；裸机构建应选择其中一套，不能把同时启用两者理解为自动切换或分配失败后的备用关系。

## 11. 本次修改清单

- [bump_allocator/src/lib.rs](../arceos/modules/bump_allocator/src/lib.rs)：实现状态、初始化、双端分配、整体字节回收、统计和受限连续扩展。
- [alt_axalloc/src/lib.rs](../arceos/modules/alt_axalloc/src/lib.rs)：将追加内存接口转发到锁保护的底层分配器。
- [bump_allocator/src/tests.rs](../arceos/modules/bump_allocator/src/tests.rs)：补充分配器行为测试。
- 本文和[架构文档](architecture_zh.md)：记录实现逻辑、适用范围和验证结果。
