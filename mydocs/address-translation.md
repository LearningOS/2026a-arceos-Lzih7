# 本项目的物理地址与虚拟地址转换

本项目需要区分两类虚拟地址：**内核直接映射区域的地址**和**用户映射区域的地址**。前者可以加减固定偏移转换，后者需要查询所属地址空间的页表。

本文以仓库中的 RISC-V QEMU 平台为例。其他平台的偏移和地址布局可能不同，应以对应平台配置为准。

## 1. 地址计算与建立映射是两件事

- **物理地址（PA）**：标识实际物理内存或设备寄存器的位置。
- **虚拟地址（VA）**：开启分页后，程序发出的内存访问所使用的地址，由 MMU 根据当前页表转换。
- **页表**：记录虚拟页对应哪个物理页，以及读、写、执行、用户态访问等权限。

`phys_to_virt()` 和 `virt_to_phys()` 只是计算地址数值，不会分配内存、建立页表或检查页面是否存在。只有相应映射已经建立，计算出的虚拟地址才可以按权限访问。

```text
软件计算地址：PA + offset → 内核 VA

CPU 实际访问：VA → MMU / TLB / 页表 → PA
```

即使软件能用加减法算出地址，CPU 仍需要有效的地址映射。

## 2. 内核映射区域：固定偏移的直接映射

### 2.1 转换函数

[axhal/src/mem.rs](../arceos/modules/axhal/src/mem.rs) 实现：

```rust
pub const fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    pa!(vaddr.as_usize() - axconfig::PHYS_VIRT_OFFSET)
}

pub const fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    va!(paddr.as_usize() + axconfig::PHYS_VIRT_OFFSET)
}
```

它们依赖一个约定：内核直接映射区域满足以下关系：

```text
内核 VA = PA + PHYS_VIRT_OFFSET
PA      = 内核 VA - PHYS_VIRT_OFFSET
```

**这里的 `virt_to_phys()` 不是通用的虚拟地址翻译器，只适用于满足该固定偏移关系的地址。**

### 2.2 本项目的 RISC-V 配置

[riscv64-qemu-virt.toml](../arceos/platforms/riscv64-qemu-virt.toml) 中声明：

| 配置 | 值 |
| --- | --- |
| 物理内存起点 | `0x8000_0000` |
| 物理内存大小 | `0x0800_0000`，即 128 MiB |
| 内核镜像物理起点 | `0x8020_0000` |
| 内核镜像虚拟起点 | `0xffff_ffc0_8020_0000` |
| 固定偏移 | `0xffff_ffc0_0000_0000` |
| 内核地址空间起点 | `0xffff_ffc0_0000_0000` |

例如：

```text
物理地址：        0x0000_0000_8020_0000
加上偏移：        0xffff_ffc0_0000_0000
得到内核虚拟地址：0xffff_ffc0_8020_0000
```

地址空间范围只是布局约束，不表示其中每个地址都已经映射。

### 2.3 映射在哪里建立

分为启动阶段和后续内存管理阶段。

**启动阶段**：[RISC-V boot.rs](../arceos/modules/axhal/src/platform/riscv64_qemu_virt/boot.rs) 的 `init_boot_page_table()` 建立两段映射：

```text
低地址恒等映射：
VA [0x8000_0000, 0xc000_0000)
 → PA [0x8000_0000, 0xc000_0000)

高地址直接映射：
VA [0xffff_ffc0_8000_0000, 0xffff_ffc0_c000_0000)
 → PA [0x8000_0000, 0xc000_0000)
```

代码随后设置 `satp`，启用 Sv39 分页，把栈地址和跳转目标加上偏移，转到高地址执行。低地址恒等映射用于支持启动过渡；不要把它与高地址直接映射的转换规则混为一谈。

**启用 `paging` 后**：[axmm/src/lib.rs](../arceos/modules/axmm/src/lib.rs) 的 `new_kernel_aspace()` 遍历 `memory_regions()`，重新建立更细粒度的内核映射：

```rust
for r in axhal::mem::memory_regions() {
    aspace.map_linear(
        phys_to_virt(r.paddr),
        r.paddr,
        r.size,
        r.flags.into(),
    )?;
}
```

`map_linear()` 根据起始 VA、起始 PA 和长度向页表写入映射。这里的内存区域还包含代码、数据、空闲内存以及平台提供的设备区域，并使用各区域的权限和属性。

### 2.4 固定偏移映射为什么仍然有用

1. 内核可以放在统一的高地址区域，为用户程序保留低地址空间。
2. 页表可以分别设置代码、数据和设备区域的访问权限与属性；固定偏移不等于所有页面权限相同。
3. 内核可以方便地访问物理页：知道 PA，就能算出其直接映射 VA；需要向页表或硬件提供 PA 时，再反向转换。

当前 [MemRegionFlags 到 MappingFlags 的转换](../arceos/modules/axhal/src/paging.rs) 不添加 `USER` 权限，因此内核映射被复制到用户地址空间的页表中，也不等于用户态可以访问这些页面。

## 3. 用户映射区域：通过所属页表翻译

### 3.1 用户地址空间如何创建

[axmm/src/lib.rs](../arceos/modules/axmm/src/lib.rs) 定义：

```rust
const USER_ASPACE_BASE: usize = 0x0000;
const USER_ASPACE_SIZE: usize = 0x40_0000_0000;
```

`new_user_aspace()` 创建用户地址空间，并复制内核映射：

```rust
let mut aspace = AddrSpace::new_empty(
    VirtAddr::from(USER_ASPACE_BASE),
    USER_ASPACE_SIZE,
)?;
aspace.copy_mappings_from(&kernel_aspace().lock())?;
```

这意味着页表中可以同时包含低地址用户映射和高地址内核映射。用户区域的代码、数据、栈等仍需要另外映射，创建地址空间不会自动给整个用户范围分配物理内存。

### 3.2 用户页如何获得物理内存

例如 [tour/m_1_0 的加载器](../arceos/tour/m_1_0/src/loader.rs) 使用 `map_alloc()`，并传入 `MappingFlags::USER` 等权限。

主要调用关系是：

```text
AddrSpace::map_alloc(用户 VA, 长度, 权限, populate)
    → Backend::Alloc
    → 从全局分配器申请物理页对应的内核 VA
    → virt_to_phys(内核 VA)，得到 PA
    → pt.map(用户 VA, PA, 页大小, 权限)
```

[backend/alloc.rs](../arceos/modules/axmm/src/backend/alloc.rs) 的 `alloc_frame()` 使用 `axalloc::global_allocator()`，不是 `alt_axalloc`：

```rust
let vaddr = VirtAddr::from(
    global_allocator().alloc_pages(1, PAGE_SIZE_4K).ok()?
);
let paddr = virt_to_phys(vaddr);
```

这里能减固定偏移，是因为分配器返回的 `vaddr` 是内核直接映射地址，**不是用户 VA**。

`populate = true` 时在建立映射时申请物理页；`populate = false` 时先建立未就绪的映射项，随后由缺页处理路径申请物理页并更新页表。

### 3.3 用户 VA 不能直接减内核偏移

假设某个用户页映射如下，数值仅用于演示：

```text
用户 VA：0x0000_0000_0001_0000
    ↓ 用户地址空间的页表
物理 PA：0x0000_0000_8100_0000
    ↑ 内核直接映射
内核 VA：0xffff_ffc0_8100_0000
```

用户 VA 和内核 VA 是同一块物理内存的两个访问入口。它们的转换方式不同：

```text
用户 VA → PA：查询对应用户页表
内核 VA → PA：减 PHYS_VIRT_OFFSET
PA → 内核 VA：加 PHYS_VIRT_OFFSET
```

相邻用户虚拟页可以对应不连续的物理页；不同地址空间中的同一个用户 VA，也可以对应不同 PA。因此不能对用户地址统一使用 `virt_to_phys(user_vaddr)`。

### 3.4 内核如何读写用户内存

[axmm/src/aspace.rs](../arceos/modules/axmm/src/aspace.rs) 的 `process_area_data()` 展示了典型过程：

```rust
let (paddr, _, _) = self.pt.query(vaddr)?;
// 处理页内偏移等细节后：
f(phys_to_virt(paddr), cnt, copy_size);
```

也就是：

```text
用户 VA
    ↓ 查询该用户地址空间的页表
PA
    ↓ phys_to_virt
内核直接映射 VA
    ↓
内核通过指针读写这块物理内存
```

这样内核可以通过自己的直接映射访问目标用户页，不必先切换到目标用户页表。跨页缓冲区需要逐页处理，不能假设用户虚拟地址连续就意味着物理地址也连续；尚未建立有效物理映射的页面也不能直接读取。

反方向“PA → 用户 VA”没有统一的加减法公式：一块物理页可以未映射到用户空间，也可以被多个用户 VA 或多个地址空间共享。

## 4. 与前面 EarlyAllocator 练习的关系

[axruntime 的分配器初始化](../arceos/modules/axruntime/src/lib.rs) 会把空闲区域的 PA 转成内核 VA，再交给 `axalloc` 或 `alt_axalloc` 管理。因此 [EarlyAllocator::alloc_pages()](../arceos/modules/bump_allocator/src/lib.rs) 只需在已有内存范围中划出页并返回地址，不负责建立用户页表。

要区分两步：

1. **分配内存**：从空闲物理内存中保留若干页，返回可供内核访问的直接映射 VA。
2. **建立用户映射**：取得这些页的 PA，将指定用户 VA 映射到它们。

当前 `axmm` 和 `axhal` 的分页实现明确依赖普通 `axalloc`；不能因为 `alt_axalloc` 也有 `alloc_pages()`，就认为本项目已经自动把这些分页路径改成了替代分配器。

## 5. 使用时如何选择转换方法

| 已知地址与目标 | 使用方法 | 前提 |
| --- | --- | --- |
| PA → 内核直接映射 VA | `phys_to_virt(pa)` | 相应内核映射存在，且允许目标访问 |
| 内核直接映射 VA → PA | `virt_to_phys(va)` | VA 符合固定偏移关系 |
| 用户 VA → PA | 对应地址空间的 `page_table().query(va)` | 查询正确页表，页面已有有效映射 |
| 用户 VA → 内核可访问 VA | 先查用户页表得到 PA，再 `phys_to_virt(pa)` | 两侧映射与访问权限符合要求 |
| PA → 用户 VA | 根据具体映射关系查找或记录 | 没有通用固定偏移，也不保证唯一 |

`VirtAddr` 类型本身不区分用户 VA 和内核 VA。调用转换函数前，需要先确认这个地址属于哪个区域，以及它对应哪一种映射关系。
