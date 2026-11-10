# MiniVectorDB 学习与证据手册

## 使用边界

这份材料用于帮助你真正理解、复现和质疑 MiniVectorDB 的实现。它不是学生
本人证据，也不能被伪装成学生独立完成的判断、实验或复盘。

可以计入个人学习证据的内容，至少应满足：

1. 你实际读过相关代码或运行过相关命令。
2. 你能用自己的语言说明观察到的现象。
3. 你能解释为什么会出现该现象，而不只是重复 Agent 的结论。
4. 如果做了修改或迁移实验，你参与了验证并理解结果。
5. `conversation.json` 中的归因与事实一致，明确区分学生输入、Agent 输出和
   工具结果。

推荐把学习过程拆成“预测 → 实验 → 结果 → 解释 → 修正 → 复测”的闭环。没有
实际执行和解释时，只记录为计划，不写成已完成事实。

---

## 90 分钟快速学习路径

| 时间 | 实验 | 目标 |
| --- | --- | --- |
| 0–10 分钟 | 阅读协议与入口 | 理解 ready、请求、响应和 shutdown |
| 10–25 分钟 | JSONL 边界实验 | 理解 id 类型、BIGINT、错误后继续运行 |
| 25–40 分钟 | 事务与强杀 | 理解提交持久性和未提交回滚 |
| 40–55 分钟 | 索引回归 | 解释删除后重插为什么曾产生重复行 |
| 55–70 分钟 | 向量验证与批量写入 | 理解 float32、维度错误和 I/O 批处理 |
| 70–85 分钟 | 并发测试隔离 | 解释时钟路径碰撞和原子序号 |
| 85–90 分钟 | 复盘 | 写出取舍、局限和仍不能确认的内容 |

---

## 实验一：为什么正式协议必须是 JSONL

### 先预测

如果程序启动后输出 `READY`，然后等待纯 SQL 输入，官方检查器分别在哪个阶段
失败？

### 实际执行

```powershell
cargo test --offline --test jsonl_protocol default_protocol_emits_ready_then_executes_ascii_sql -- --nocapture
```

观察测试先读取什么，再发送什么。重点看以下事实：

- 程序必须先主动输出 `{"type":"ready"}`。
- 每个请求对应恰好一条响应。
- 响应的 `id` 必须保持原 JSON 类型。
- 每条响应后立即 flush。
- shutdown 响应后不能再有 stdout。

### 需要理解的机制

JSONL 把“如何分隔 SQL、如何区分结果集、如何回传请求编号”从弱约束文本协议
变成了明确对象协议。判题器不需要猜测 `OK`、表头或错误行属于哪个请求。

### 自检问题

1. 为什么不能把 JSON 请求整行直接传给 SQL 解析器？
2. 为什么 `id` 是数字时不能返回字符串？
3. 为什么 SQL 错误不能导致进程退出？
4. 如果没有逐条 flush，检查器会看到什么现象？

### 可迁移场景

设计任意 RPC 或任务队列协议时，检查 request ID、错误后进程生命周期、流式响应
边界和结束帧。你可以用同一套问题审查其他协议设计。

---

## 实验二：BIGINT 与 JSON 数字精度

### 先预测

`9007199254740993` 经过 JavaScript 风格浮点处理后可能变成什么？如果程序把它
序列化成字符串，判题断言会怎样？

### 实际执行

```powershell
cargo test --offline --test jsonl_protocol request_ids_and_bigints_keep_full_json_precision -- --nocapture
```

### 需要理解的机制

- SQL 的 BIGINT 在 Rust 中是 `i64`。
- JSON 响应必须输出整数，不能经过 `f64`。
- `serde_json::Number` 能分别保留整数和浮点表示。
- 请求 `id` 是原始 JSON 值，应克隆后原样输出。

### 自检问题

1. 为什么 `i64 -> f64 -> i64` 会损坏大整数？
2. 请求 `id` 和查询结果中的 BIGINT 是同一个问题吗？
3. 如果把所有数字都转成字符串，哪些断言会失败？

### 迁移实验

构造 `id` 为字符串、整数、浮点数、布尔值和数组的请求，观察响应中的类型。
不要改测试预期来迁就实现，先判断协议要求是什么。

---

## 实验三：零行修改为什么仍要返回 `affected_rows`

### 先预测

删除一个不存在的条件时，响应只返回 `{"ok":true}` 是否足够？

### 实际执行

```powershell
cargo test --offline --test jsonl_protocol zero_row_mutations_return_integer_affected_rows -- --nocapture
```

### 需要理解的机制

协议要根据 SQL 语义区分：

- 查询：`columns` 和 `rows`
- INSERT/UPDATE/DELETE：整数 `affected_rows`，可以为零
- DDL/事务控制：通常只需要 `ok`

只看 `affected == 0` 无法区分“零行 DML”和“DDL”，因此协议层先解析语句类型，
再选择响应形状。

### 自检问题

1. 是否可以根据结果列是否为空判断 DML？
2. 哪些语句成功但没有任何可见结果？
3. 为什么协议层不应修改数据库执行语义？

---

## 实验四：shutdown、强杀与事务边界

### 先预测

以下两个场景重启后分别应看到哪些行？

1. 自动提交插入 1，开始事务插入 2，然后发送 shutdown。
2. 自动提交插入 1，开始事务插入 2，然后直接强杀进程。

### 实际执行

```powershell
cargo test --offline --test jsonl_protocol shutdown_rolls_back_uncommitted_changes_and_reopens_same_data -- --nocapture
cargo test --offline --test jsonl_protocol forced_kill_recovers_committed_jsonl_writes_and_drops_the_open_transaction -- --nocapture
cargo test --offline --test recovery -- --nocapture
```

两次都应只看到已提交的 1。

### 需要理解的机制

- 自动提交语句在成功响应前追加并同步 WAL。
- 显式事务只在内存中保留修改。
- COMMIT 必须先持久化，再返回成功。
- shutdown 回滚打开的事务，然后完成清理。
- 强杀来不及执行代码路径，因此正确性不能依赖 shutdown。
- 恢复时忽略不完整 WAL 尾部，再重建派生向量文件。

### 自检问题

1. 为什么“进程退出时统一保存”不能满足强杀恢复？
2. 为什么提交响应不能早于 WAL sync？
3. 未提交事务为什么不能写入正式 WAL 后再删除？
4. 向量文件损坏为什么可以重建，而 `state.log` 仍是事实源？

### 迁移实验

画出三种状态机：无事务、显式事务、恢复中。标出每个状态中
`state.log`、`vectors.bin` 和内存表的可见内容。

---

## 实验五：删除后重插的主键索引回归

### 先预测

执行以下操作后，`WHERE category='same'` 应返回几行？

```sql
CREATE TABLE docs (id BIGINT PRIMARY KEY, category VARCHAR(8));
INSERT INTO docs VALUES (1,'same');
DELETE FROM docs WHERE id=1;
INSERT INTO docs VALUES (1,'same');
SELECT id FROM docs WHERE category='same';
```

答案是 1 行。

### 实际执行

```powershell
cargo test --offline --test sql_integration deleting_then_reinserting_same_primary_key_does_not_duplicate_index_matches -- --nocapture
```

### 失败现象

修复前曾返回：

```text
[[1], [1]]
```

原因是 DELETE 直接调用 `BTreeMap::remove`，绕过了 VARCHAR 等值索引的移除逻辑。
旧哈希桶仍保留主键 1，重插后又加入一次，因此候选集合出现重复主键。

### 修正与复测

DELETE 改走 `Table::remove_row`，由它同时删除表行和索引项。复测返回一行，
UPDATE 的受影响行数也从潜在的 2 恢复为 1。

### 自检问题

1. 为什么候选索引中出现失效主键有时不会立刻表现为错误？
2. 为什么同一主键出现两次时，精确谓词过滤仍可能返回重复行？
3. 从“查询结果错误”如何反推到“DELETE 绕过索引维护”？

### 迁移场景

任何“主表 + 辅助索引”系统都要保证主表修改和索引修改经过同一个入口，否则重插、
回滚或恢复时很容易出现残留项。可以检查其他索引写路径是否存在同类绕过。

---

## 实验六：非法向量和 float32

### 先预测

以下输入应成功还是失败？

- `[1]` 写入 `VECTOR(2)`
- `[NaN,0]`
- `[1e100,0]`

前三个都应失败。合法的 `[1,0]` 应成功，且失败语句不能留下部分行。

### 实际执行

```powershell
cargo test --offline --test jsonl_protocol invalid_vectors_fail_atomically_and_do_not_stop_the_protocol -- --nocapture
cargo test --offline --test value_contract -- --nocapture
```

### 需要理解的机制

- 输入先解析成 `f32`。
- 维度必须严格匹配。
- 非有限值和 f32 溢出必须拒绝。
- 距离使用存储后的 float32 值。
- SQL 错误响应后进程继续接收请求。

### 自检问题

1. 为什么用 f64 存储向量会违反题目语义？
2. 为什么非法向量拒绝应发生在写 WAL 之前？
3. 为什么距离计算可能用 f64 累加，但输入仍必须是 f32？

### 迁移实验

比较 `16777216` 和 `16777217` 在 f32 中的表示，再解释公开用例
`vector_float32` 为什么两者距离都为 0。

---

## 实验七：批量写入与失败污染

### 先预测

重复失败的多行向量 INSERT 是否可能不断增大 `vectors.bin`？如果主键检查发生在
向量物化之后，答案是“可能”。

### 实际执行

```powershell
cargo test --offline --test storage_delta_contract failed_duplicate_vector_inserts_do_not_append_orphan_payloads -- --nocapture
cargo test --offline --release --test benchmark -- --ignored --nocapture
```

### 失败现象

修复前，十次重复主键插入使 `vectors.bin` 从 12 字节增长到 132 字节。失败语句
没有改变表数据，却追加了不会被 WAL 引用的孤立向量。

### 修正

1. 在向量物化前预检查主键是否存在或是否在批次内重复。
2. 把一次 INSERT/UPDATE 的多个向量合并成一次批量写入。
3. 恢复 WAL 时也按批次物化向量。

### 观察结果

本机 10,000 行 × 128 维、1,000 次查询的回归基准：

```text
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=445.7862ms query=1.3043352s cache_bytes=6151232
```

同一机器上较早的非批量运行约为：

```text
write=484.4624ms query=1.2972755s
```

### 自检问题

1. 批量写入减少了逻辑 I/O 还是字节数？
2. 为什么失败语句留下的孤立向量通常不影响查询结果，却仍值得修复？
3. 为什么不能为优化性能而跳过主键预检查？
4. 本地时间能否直接换算成官方性能分？

---

## 实验八：并发测试为什么不稳定

### 出现过的现象

Linux Docker 构建曾失败一次：

```text
EOF while parsing a value
```

随后单独查看日志，失败发生在读取 ready 行时，子进程已经退出。

### 根因分析

多个并行测试使用“进程 ID + 当前纳秒”生成临时目录。容器或虚拟机的时钟分辨率
可能让不同线程拿到相同时间值，于是两个数据库进程竞争同一数据目录和向量文件。

### 修正

测试路径增加原子递增序号，并抽成共享的 `unique_temp_path`。随后执行：

```powershell
1..20 | ForEach-Object {
    cargo test --offline --test jsonl_protocol -- --test-threads=8
    if ($LASTEXITCODE -ne 0) { throw "repetition $_ failed" }
}
```

二十轮并行回归全部通过。

### 自检问题

1. 为什么“纳秒时间戳”不保证并发唯一？
2. 为什么这类问题更像测试隔离问题，而不是数据库恢复问题？
3. 如果真实生产路径也共享目录，应增加什么约束？

---

## 五个评分维度对应的真实练习

### 1. 问题理解与自主拆解

练习：不看现成总结，自己把实现拆成协议、SQL、存储、事务、向量五部分，并写出
每部分的输入、输出和失败模式。

可记录的本人证据：

- 你亲手画出的模块边界图。
- 你识别出的一个 Agent 初始遗漏。
- 你对“正确性准入优先于 ANN 性能”的独立取舍。

### 2. 技术理解与迁移

练习：把“主表与索引必须同步修改”迁移到一个你熟悉的场景，例如文档与倒排索引、
用户表与缓存、对象存储与元数据表。

可记录的本人证据：

- 对新场景中“残留索引”的具体描述。
- 你预测的复现步骤。
- 实际观察到的结果。

### 3. 独立验证与质疑

练习：不要只跑 `cargo test`。选一个通过的结果，构造测试来挑战它。例如：

- 删除主键后以相同主键重插。
- 重复失败插入后检查文件大小。
- 用不同 JSON 类型作为请求 `id`。
- 强制杀死包含未提交修改的进程。

可记录的本人证据：

- 你提出的反例。
- 失败现象。
- 你是否接受或推翻 Agent 的解释。

### 4. 实验与反馈迭代

练习：至少完成一条完整的“预测 → 失败 → 修正 → 复测”链。不要写“我让 Agent
修好了”；应写清你观察到了什么，修改改变了哪条数据路径，复测指标如何变化。

可记录的本人证据：

- 修复前后的实际输出。
- 为什么选择这个修复。
- 复测是否只修复了表面现象。

### 5. 取舍与反思

练习：针对以下取舍写出自己的判断：

- exact 模式与 ANN 召回率、查询速度的取舍。
- 向量分页存储与实现复杂度的取舍。
- 事务全量快照与未来大规模数据内存占用的取舍。
- 批量写性能与失败原子性的取舍。

可记录的本人证据：

- 你认为当前设计在哪个数据规模会失效。
- 你如何验证这个预测。
- 为什么暂时不实现更复杂的方案。

---

## 推荐真实实验记录模板

完成实验后，用下面结构记录。只在括号处写你实际做过和观察到的事实。

```text
问题：
我原先的预测：
我实际运行的命令：
实际输出：
我观察到的关键差异：
我认为的根因：
我参与的修正或验证：
复测结果：
能够迁移到的新场景：
当前局限：
```

示例只应参考结构，不应照抄内容：

```text
问题：删除后重插同一主键，VARCHAR 查询是否仍只返回一行？
我的预测：应该只返回一行。
实验：运行回归测试前，我先手工执行 INSERT/DELETE/INSERT/SELECT。
结果：修复前出现两行相同主键。
解释：删除路径没有清除等值索引项，重插后又加入一次。
复测：改走统一的 remove_row 后只返回一行。
局限：这个测试只覆盖 VARCHAR 等值索引，没有覆盖其他未来新增索引。
```

---

## `conversation.json` 的安全整理方法

允许写入：

- 用户的真实原始 prompt。
- Agent 或工具的真实执行结果，并明确标注来源。
- 学生亲自完成的实验、解释、质疑、修正和反思。
- 没有完成的计划，并明确写为“计划”。

不要写入：

- 把 Agent 的代码、判断或复盘改写成学生第一人称。
- 没运行过却写成“我验证了”。
- 把同一个证据重复包装成多个独立 unit。
- 用模板生成不存在的失败、取舍或迁移经历。

推荐每个证据 unit 只包含一条清晰链条：

```text
student_observation -> student_prediction -> actual_command/output
-> student_explanation -> retest -> limitation
```

同一实验如果再次运行，不自动成为新 unit。只有更换问题、改变假设或迁移到新场景，
才形成新的独立证据。

---

## 提交前的诚实性检查

- `conversation.json` 是否明确说明哪些内容由 Agent/tool 产生？
- 每条“我运行了”是否能对应真实命令和输出？
- 每条“我解释了”是否是你本人能当场复述的内容？
- 是否把公开用例写成隐藏用例已验证？
- 是否把 Recall@10=1.0 写成官方性能满分？
- 是否区分了本地毫秒数与官方 cgroup 指标？
- 是否承认隐藏用例、正式基线和 Agent 评审仍不可见？

如果任何一项不确定，保留为限制，不补写不存在的经历。
