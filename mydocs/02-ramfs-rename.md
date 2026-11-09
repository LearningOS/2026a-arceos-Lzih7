# 第二题：RAMFS 文件重命名解决方案

本文对应 `./scripts/test-ramfs_rename.sh`。本文对应的实现已完成，并通过单元测试和课程 QEMU 测试。

## 1. 题目要求

入口 `arceos/exercises/ramfs_rename/src/main.rs` 中的 `process` 依次执行：

```rust
create_file("/tmp/f1", "hello")?;
rename_file("/tmp/f1", "/tmp/f2")?;
print_file("/tmp/f2")
```

需要让文件在同一目录中改名，文件内容保持为 `hello`。成功后旧名字应无法查到，新名字应指向原文件节点。本题不要求跨目录移动。

核心操作是改变目录中的“名字 → 文件节点”映射，不需要复制文件内容，也不应创建一个空的新文件来代替原文件。

## 2. 从入口追踪到实现

建议依次阅读下列文件，路径均相对仓库根目录：

| 文件 | 作用 |
| --- | --- |
| `arceos/exercises/ramfs_rename/src/main.rs` | 创建文件、改名、读回内容 |
| `arceos/modules/axfs/src/api/mod.rs` | 文件系统公开的 `rename` 接口 |
| `arceos/modules/axfs/src/root.rs` | 查找挂载文件系统，向其根节点转发操作 |
| `arceos/modules/axfs/src/mounts.rs` | 构造 `/tmp` 使用的 RAMFS |
| `arceos/modules/axfs/src/fs/mod.rs` | 将 `axfs_ramfs` 导出为 `fs::ramfs` |
| `arceos/axfs_ramfs/src/dir.rs` | 本地 RAMFS 的目录节点实现 |

练习中的 `src/ramfs.rs` 通过 `MyFileSystemIf` 提供自定义主文件系统，但 `/tmp` 还会通过挂载逻辑使用 RAMFS。因此要确认真正接收 `/tmp/f1` 操作的实现，不能只修改练习入口。

## 3. 逐段理解本次 rename 实现

### 3.1 为什么之前会 panic

应用调用 `fs::rename` 后，最终需要具体目录节点实现 `VfsNodeOps::rename`。此前 RAMFS 的 `DirNode` 没有重写这个方法，所以调用落入 trait 默认实现，返回 `VfsError::Unsupported`。

应用中的 `?` 将这个错误逐层返回给 `main`，`main` 再调用 `panic!` 报告错误。因此 `main.rs` 的 panic 行是错误被报告的位置，缺失功能位于底层 RAMFS。

下面以这次实际运行的调用为例：

```text
应用：std::fs::rename("/tmp/f1", "/tmp/f2")
  ↓ std 是 axstd 的别名
axfs 文件系统 API
  ↓
root::rename(old, new)
  ↓
RootDirectory::rename：解析挂载点
  ↓ 源、目标都进入 /tmp 对应的 RAMFS
DirNode::rename("f1", "f2")
  ↓
修改目录项，保留文件节点
```

### 3.2 文件名和文件内容分别存在哪里

`DirNode` 的核心字段是：

```rust
children: RwLock<BTreeMap<String, VfsNodeRef>>
```

可以从内向外理解：

| 类型 | 在本题中的作用 |
| --- | --- |
| `String` | 目录项名字，例如 `f1` |
| `VfsNodeRef` | 文件系统节点引用，实际是 `Arc<dyn VfsNodeOps>` |
| `BTreeMap` | 保存“名字 → 节点引用”的对应关系 |
| `RwLock` | 允许读者共享访问，修改目录时要求独占访问 |

文件内容放在 `FileNode` 的 `content: RwLock<Vec<u8>>` 中，与目录里的名字分开存储。

```text
改名前：children["f1"] ──→ 文件节点 A ──→ hello
改名后：children["f2"] ──→ 文件节点 A ──→ hello
```

改变的是目录表的键，文件节点 A 没有被重建。因此文件内容保持不变，已经持有节点 A 的打开文件也仍然引用它。

### 3.3 第一处修改：让源路径和目标路径处于相同坐标系

位置：`arceos/modules/axfs/src/root.rs` 的 `impl VfsNodeOps for RootDirectory`。

旧实现只解析源路径的挂载点，将目标路径原样传下去：

```rust
fs.root_dir().rename(rest_path, dst_path)
```

本题中就可能变成 `rename("f1", "/tmp/f2")`。但 RAMFS 的根目录就是挂载在 `/tmp` 的根目录，它不应再把 `tmp` 当成自己的子目录。

现在的实现是：

```rust
fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
    self.lookup_mounted_fs(src_path, |src_fs, src_rel| {
        self.lookup_mounted_fs(dst_path, |dst_fs, dst_rel| {
            if src_rel.is_empty() || dst_rel.is_empty() {
                return ax_err!(PermissionDenied);
            }
            if !Arc::ptr_eq(&src_fs, &dst_fs) {
                return ax_err!(Unsupported);
            }
            src_fs.root_dir().rename(src_rel, dst_rel)
        })
    })
}
```

两层闭包分别拿到源、目标对应的文件系统引用及挂载点内路径。对于本题：

| 输入 | 文件系统 | 交给底层的路径 |
| --- | --- | --- |
| `/tmp/f1` | `/tmp` 对应的 RAMFS 实例 | `f1` |
| `/tmp/f2` | 同一个 RAMFS 实例 | `f2` |

`src_rel` 或 `dst_rel` 为空意味着操作指向挂载根，返回 `PermissionDenied`，保护挂载点。

`Arc::ptr_eq` 检查的是两个引用是否指向同一个文件系统实例。两个文件系统即使类型都是 RAMFS，也可能是两棵独立的目录树，不能仅靠类型相同就允许操作。

最后从该文件系统的根目录进入具体 `rename` 实现。挂载层负责确定“哪一个文件系统”，目录节点负责修改“哪一个目录项”。

### 3.4 第二处修改：分离父目录和文件名

位置：`arceos/axfs_ramfs/src/dir.rs`，新增 `DirNode::rename`。

方法开头是：

```rust
let src = src_path.trim_start_matches('/');
let dst = dst_path.trim_start_matches('/');
let (src_parent, src_name) = src.rsplit_once('/').unwrap_or(("", src));
let (dst_parent, dst_name) = dst.rsplit_once('/').unwrap_or(("", dst));
```

`trim_start_matches` 去掉开头的 `/`；这里的路径已经属于当前文件系统，不再解释为全局根目录。

`rsplit_once('/')` 从最后一个 `/` 分开路径。没有 `/` 时，`unwrap_or` 将父目录设为空字符串：

| 路径 | 父目录 | 文件名 |
| --- | --- | --- |
| `f1` | 空字符串 | `f1` |
| `sub/f1` | `sub` | `f1` |
| `a/b/f1` | `a/b` | `f1` |

接着检查名字和父目录：

```rust
let valid = |name: &str| !name.is_empty() && name != "." && name != "..";
if !valid(src_name) || !valid(dst_name) {
    return Err(VfsError::InvalidInput);
}
if src_parent != dst_parent {
    return Err(VfsError::Unsupported);
}
```

空名字、`.` 和 `..` 不是本题允许改名的普通文件名。两个父目录不同则属于移动操作，本题不支持。

这里比较的是父路径字符串，不是解析后目录节点的身份。因此 `sub/f1 → sub/f2` 受支持，但 `sub/f1 → ./sub/f2` 即使指向相同父目录，也会被拒绝。这是当前实现的明确限制。

### 3.5 父目录非空时，先进入该目录

```rust
if !src_parent.is_empty() {
    return self.this.upgrade().ok_or(VfsError::NotFound)?
        .lookup(src_parent)?.rename(src_name, dst_name);
}
```

`self.this` 是指向当前目录的 `Weak<DirNode>`。使用弱引用可以避免目录通过自己的字段形成强引用环。

`upgrade()` 临时取得 `Arc<DirNode>`，因为 `lookup` 的接收者需要 `Arc<Self>`。`ok_or(...)?` 把无法升级的情况转成错误；后面的 `?` 也会立即返回查找失败的错误。

例如 `sub/f1 → sub/f2`：先 `lookup("sub")` 找到子目录，再在该目录上调用 `rename("f1", "f2")`。进入第二次调用后父目录为空，继续执行目录项更新。递归之前没有持有 `children` 写锁。

### 3.6 获取写锁，并在修改前完成检查

```rust
let mut children = self.children.write();
let node = children.get(src_name).ok_or(VfsError::NotFound)?;
if !node.get_attr()?.is_file() {
    return Err(VfsError::Unsupported);
}
if src_name == dst_name {
    return Ok(());
}
if let Some(target) = children.get(dst_name) {
    if !target.get_attr()?.is_file() {
        return Err(VfsError::IsADirectory);
    }
}
```

逐句作用如下：

1. `write()` 获取目录项表的独占访问权。其他遵守该锁的线程不能在检查后、更新前插入或删除目录项。
2. `get(src_name)` 只借用源节点；源不存在时返回 `NotFound`，此时没有删除任何东西。
3. `get_attr()?.is_file()` 限定源必须是普通文件，当前不支持目录改名。
4. 源、目标名字相同且源文件存在时，直接成功。
5. 若目标已存在，要求目标也是普通文件，避免用文件覆盖目录。

这些检查都在真正更新前进行。通过 `?` 或 `return` 离开作用域时，锁守卫自动释放，不需要手动解锁。

### 3.7 核心两行：移出旧键，插入新键

```rust
let node = children.remove(src_name).ok_or(VfsError::NotFound)?;
children.insert(dst_name.into(), node);
Ok(())
```

`remove` 返回旧目录项持有的节点引用，将其所有权交给局部变量 `node`。随后 `insert` 把同一个引用移到新名字下。

`dst_name` 是借用的 `&str`，而目录表的键要求拥有字符串的 `String`，所以用 `.into()` 转换。这里复制了名字，不复制文件内容。

若目标普通文件已存在，`BTreeMap::insert` 会替换其节点引用。旧目标节点若仍被其他打开文件持有，就继续存在；当最后一个强引用释放时才能销毁。

移除和插入都位于同一把写锁内，所以其他通过该锁查询目录的线程看不到两步之间的中间状态。这不等于实现了所有文件系统层面的原子性：跨目录、跨挂载点以及完整 POSIX rename 语义均不在当前实现范围内。

### 3.8 第三处修改：移除上层提前删除目标

`root.rs` 中公开给内部使用的辅助函数现在只转发：

```rust
pub(crate) fn rename(old: &str, new: &str) -> AxResult {
    parent_node_of(None, old).rename(old, new)
}
```

旧版本会先查找并删除目标，再执行改名。这有两种明显问题：

| 调用 | 提前删除目标的后果 |
| --- | --- |
| `rename("missing", "target")` | 源不存在，改名失败，但目标已经丢失 |
| `rename("f1", "f1")` | 目标就是源，提前删除后再也找不到源 |

现在把目标替换交给具体文件系统处理。RAMFS 会先检查源和目标，再在同一目录写锁内更新，避免以上问题。其他文件系统是否允许覆盖目标，则取决于各自实现；本次课程测试验证的是 RAMFS。

### 3.9 第四处修改：让本地 RAMFS 参与构建

这一步不改变改名算法，但决定运行时是否使用新增的代码。

`arceos/modules/axfs/Cargo.toml` 修改前：

```toml
axfs_ramfs = { version = "0.1", optional = true }
```

它独立声明使用 crates.io 版本，没有继承工作区的本地路径。即使本地目录里有同名 crate，Cargo 也不会自动用它替代远程包。

修改后：

```toml
axfs_ramfs = { workspace = true, optional = true }
```

`workspace = true` 继承 `arceos/Cargo.toml` 的 `[workspace.dependencies]`：

```toml
axfs_ramfs = { path = "./axfs_ramfs" }
```

该路径相对工作区清单，因此指向 `arceos/axfs_ramfs/`。`optional = true` 表示此依赖仍由相关 feature 启用；它不表示 Cargo 可以任意在本地和远程版本之间选择。

`Cargo.lock` 则记录最终解析的包。此次 Cargo 自动更新了以下记录：

| 使用方或记录 | 修改结果 |
| --- | --- |
| 本地 RAMFS | 新增版本 `0.1.1` 的本地包记录，无 registry 来源 |
| `axfs` 的依赖 | 指向本地 RAMFS `0.1.1` |
| 练习直接依赖的 RAMFS | 保持 crates.io 的 `0.1.2`，用于自定义主文件系统 |

所以锁文件中同时存在两个 RAMFS，`/tmp` 挂载使用的是 `axfs` 选择的本地实现。构建日志中的 `axfs_ramfs v0.1.1 (/workspace/arceos/axfs_ramfs)` 可以佐证这一点。

`Cargo.toml` 声明依赖规则，`Cargo.lock` 记录具体解析结果。仅修改锁文件不会替代清单中的依赖规则；只在本机改缓存也无法把实现提交到评测环境。

这项依赖配置和锁文件修正与做题指南“只改 .rs”的要求存在冲突，提交课程作业前应向维护者说明原有本地依赖未接入的问题。

### 3.10 用实际输入串起整个过程

1. 创建 `/tmp/f1`，挂载路由使 RAMFS 的目录表出现 `f1 → 节点 A`。
2. 写入 `hello`，内容存入节点 A。
3. 调用 `rename("/tmp/f1", "/tmp/f2")`。
4. 挂载层确认两个路径属于同一个 RAMFS，转换为 `f1` 和 `f2`。
5. RAMFS 确认两个文件名合法、源存在且为普通文件。
6. 持有写锁，将 `f1 → A` 改为 `f2 → A`。
7. 打开 `/tmp/f2` 找到节点 A，读回 `hello`；再查找旧名字 `f1` 则得到 `NotFound`。

### 3.11 已验证的行为与边界

此前已经执行并通过：

- `cargo test -p axfs_ramfs@0.1.1 --lib`：两个单元测试通过。
- `./scripts/test-ramfs_rename.sh`：QEMU 读回 `hello`，输出 `ramfs_rename pass`。

新增测试覆盖内容保留、节点引用身份不变、旧名字消失、同名操作、覆盖目标普通文件、源不存在时目标保留、特殊名字拒绝、覆盖目录拒绝、跨目录移动拒绝和嵌套同目录改名。

当前没有实现目录改名、跨目录移动、跨文件系统移动或完整路径规范化，也没有验证所有其他文件系统的替换语义。

## 4. 测试与观察

在宿主机仓库根目录启动课程容器：

```sh
./scripts/docker.sh
```

在容器的仓库根目录运行：

```sh
./scripts/test-ramfs_rename.sh
```

已经进入 Dev Container 时可直接运行测试。脚本会创建镜像，并执行：

```sh
make run A=exercises/ramfs_rename/ BLK=y
```

正常输出应包含：

```text
Create '/tmp/f1' and write [hello] ...
Rename '/tmp/f1' to '/tmp/f2' ...
Read '/tmp/f2' content: [hello] ok!

[Ramfs-Rename]: ok!
```

测试脚本检查捕获输出的最后一行是否含有 `[Ramfs-Rename]: ok!`，最终显示 `ramfs_rename pass`。

练习程序会读出内容，但没有断言内容一定等于 `hello`，也没有检查旧名字消失。自己验证时还应确认：新文件读回 `hello`，旧名字打开失败，源文件不存在时返回错误且不改变目录项。

## 5. 排查顺序

1. 返回 `Unsupported`：确认调用进入了实现了 `rename` 的 RAMFS，而不是 trait 默认实现。
2. 修改本地文件却毫无变化：通过 Cargo metadata 确认实际依赖来源。
3. 返回路径相关错误：检查 RAMFS 收到的是 `f1`、`f2`，还是仍夹带 `/tmp/`。
4. 改名后内容消失：检查是否移动了原节点，而不是创建了空节点。
5. 读回成功但脚本仍失败：查看输出最后一行是否满足脚本检查条件。

本题应掌握的核心是：应用接口通过挂载层路由到具体文件系统，而文件改名最终改变的是目录项映射。
