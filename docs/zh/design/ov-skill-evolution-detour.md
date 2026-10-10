# 在 OpenViking `main` 上接一条 OpenSpace 式 Skill evolution detour

核对基线：`main@144033826` 与本地 OpenSpace。**本文是第一版选定的实现方案**；其中的旁路、worker 和 Harness 协议均为待实现设计，不代表仓库已有 `SkillEvolutionDetour` 接口。

## 结论和切口

**第一版采用可关闭的旁路。** 人工 `SKILL.md + references/scripts` 先作为正式包导入 OV；保留 OV 的 Session 归档、Skill 候选抽取与正式 Skill 安装器，在“候选已抽出、尚未进入 `StreamingPolicyTrainer.submit_gradients()`”处转给独立提案 worker。worker 借鉴 OpenSpace 的证据、决策、暂存、验证、幂等流程，发布时通过 OV 的整包 Skill API 回到共享正式库。Experience 可独立运行，但不作为 Skill 提案或发布的必经步骤。

```mermaid
flowchart LR
    A[任务 Agent + 人工纠正] --> B[OV Session archive]
    B --> C[OV Skill 抽取：轨迹 + 已有 Skill 列表]
    C --> D{维护模式}
    D -->|direct 现有路径| E[OV Skill trainer + SkillPolicyUpdater]
    D -->|detour| F[证据包与归因]
    F --> G[已发布包 + 待处理提案去重]
    G --> H[OpenSpace 式整包暂存/编辑]
    H --> I[引用检查 + 任务验收]
    I --> J[OV 整包 Skill API/安装器]
    E --> K[OV Skill 库]
    J --> K
    K --> L[库版本事件/清单]
    L --> M[各 Agent 在任务边界刷新并回报 revision]
```

这里的 detour **并不直接复用 OpenSpace 的默认 committer**：它提交的是 OpenSpace 本地 SkillStore 和 registry；我们需要 `OVPublisher`。也不建议仅把 OpenSpace CLI `fix_skill(skill_dir, direction)` 放进 VikingBot 工具列表：该入口针对已知本地目录、手工给定修改方向，会运行并提交其自身本地流程，缺少 OV 轨迹、版本、共享发布和提案隔离的完整协议。[OpenSpace Engine 的可注入协作者](https://github.com/HKUDS/OpenSpace/blob/38277815ed44a53d757973c2bc4454c3b6426698/openspace/skill_engine/evolution/engine.py#L75)、[默认 committer](https://github.com/HKUDS/OpenSpace/blob/38277815ed44a53d757973c2bc4454c3b6426698/openspace/skill_engine/evolution/engine.py#L718)、[现有 MCP fix_skill](https://github.com/HKUDS/OpenSpace/blob/38277815ed44a53d757973c2bc4454c3b6426698/openspace/entrypoints/mcp/server.py#L1639)。

## 为什么选在梯度之后、写入之前

`main` 的 `extract_session_skills()` 已能从消息生成 `skill_gradients`，然后立即调用 Skill trainer。这里适合换成可插拔 sink：[当前提交点](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/session/compressor_v3.py#L819)。写入端的 `SkillPolicyUpdater` 接受的是单文件正文与元数据计划；上游 schema 只有 `skill_name/description/content`。如果只替换 updater，OpenSpace 已经没有足够信息去决定“为何改”和“reference 文件如何改”，还要先修复 OV 的 Skill PolicySet 重载及候选搜索。前移一个节点可以把完整归档、纠正和已加载版本一起交给提案 worker，之后用 OV 整包 API 发布。

同时不能**只**把已有 `skill_gradients` 当作 detour 唯一入口。OV 模板偏向用户长期规则；Agent 解决了一个新问题但模型没有生成梯度时，旁路也应有机会根据归档证据作 `NO_CHANGE/FIX/NEW` 决策。因此 detour 每次接收一个 `SessionEvidence`，Skill 梯度只是其中一类提示，不是唯一触发条件。

## OV 需要的最小改动

| 位置 | 修改 | 原因 |
| --- | --- | --- |
| 配置 | 保留现有 `session_skill_extraction_enabled` 总开关，增 `skill_maintenance_mode = direct / detour`，默认 `direct` | 不改变现有部署行为；detour 可回退。 |
| `SessionCompressorV3` | 注入 `SkillMaintenanceSink`；抽出 `propose_session_skills(messages, ctx, archive_uri)`，只运行抽取并返回梯度，不写正式库 | 入口稳定，不把 OpenSpace import 进 OV 内核。 |
| commit 分流 | detour 模式对每个允许 self-memory 的归档提交一次 `SessionEvidence`；不依赖 Case 是否存在，也不再逐 Case 抽 Skill | 修复无 Case 跳过和同 Session 多 Case 重复生成。可参考 `feat/dag_exp_mode` 的“一次抽取”调度，但它仍在无 Case 时跳过。 |
| 返回契约 | `skill_proposal_job_ids` 与实际 `session_skill_uris` 分开；前者表示已接收/排队，后者仅记录真正发布成功的包 | 避免把“已提案”误报为“已安装”；绕开当前共享批次 Skill URI 归属缺陷。 |
| 安全边界 | detour 模式禁止内部 Skill trainer 同时落盘；配置恢复 direct 时才走旧路径 | 单一正式写入者，避免两个维护器对同一包竞争。 |

概念接口（**拟新增，不是现有 API**）：

```python
class SkillMaintenanceSink(Protocol):
    async def submit(self, event: SessionEvidence) -> ProposalReceipt: ...

@dataclass
class SessionEvidence:
    event_id: str                 # 幂等键：session/archive/batch
    library_id: str               # 共同任务 Skill 库
    archive_uri: str
    run_id: str
    agent_id: str
    skill_hints: list             # OV skill_gradients，可为空
    loaded_skills: list           # 真正读取/启用的 URI + revision
    corrections: list            # 人工纠正及其来源消息 ID
    outcome_refs: list           # 工具结果、验收结果的 OV URI
```

`loaded_skills` 和 `corrections` 需要 Harness 显式记录；仅凭当前梯度或“Skill 出现在可见列表”不能判断 Agent 是否实际加载并遵守。原始消息/文件仍留在 OV，旁路持久化引用、摘要、作业状态即可。

## Detour worker 做哪些事

1. **归因与准入**：参考 OpenSpace `EvidencePacket`、`DecisionEngine`、`EvolutionAdmission`。将漏读、已读未遵守、原规则错误、已验证的新流程、外部故障、用户长期规则分开；可输出 `NO_CHANGE` 或 `ROUTING_CHANGE`，不必每次改包。
2. **两层去重**：先按 `event_id/proposal_id` 幂等，合并同一时间待处理提案；再对 OV 已发布包和候选包做任务范围、触发条件、前提、步骤、验收条件的语义对比。OpenSpace 的名称/描述相似度只作廉价初筛，不能决定跨 Agent 同义合并。
3. **整包暂存**：从 OV `GET /api/v1/skills/{name}?include_integrity=true` 获取 revision、文件路径和哈希，再下载 `SKILL.md` 及全部 references/scripts/assets，复制到隔离候选目录。借鉴 `StagedSkillEdit`、`stage_fix_skill()` 的 before/proposed/diff，但扩展到二进制附件：OpenSpace 自带的文本 snapshot 会跳过二进制。[OpenSpace 暂存](https://github.com/HKUDS/OpenSpace/blob/38277815ed44a53d757973c2bc4454c3b6426698/openspace/skill_engine/patch.py#L207)、[文本快照限制](https://github.com/HKUDS/OpenSpace/blob/38277815ed44a53d757973c2bc4454c3b6426698/openspace/skill_engine/patch.py#L1065)、[OV 完整性清单](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/server/routers/skills.py#L355)。
4. **验证**：先检查 Skill 格式、相对引用、附件完整性和 `intent_spec` 中的正/负触发；再用原失败用例和正常用例比较基线包与候选包。OpenSpace `SkillBehaviorEvaluator` 有接口分层，但实际回放效果依赖我们自己的任务 runner。[行为验证器](https://github.com/HKUDS/OpenSpace/blob/38277815ed44a53d757973c2bc4454c3b6426698/openspace/skill_engine/evolution/behavior_eval.py#L232)。
5. **串行发布**：单发布者在写锁内重新读取 OV 包 revision、确认未被更改，然后上传整个候选包，通过 `POST /api/v1/skills` 新建或 `PUT /api/v1/skills/{name}` 替换，显式指定 `target_uri=viking://agent/skills`，等待处理，再读完整性清单核对新 revision。删除单独走 `DELETE`，首期只处理明确授权的删除。OV PUT 有包备份与失败恢复，但 API 当前没有 `expected_revision` 条件参数；若允许旁路之外的写入者，应给 PUT/DELETE 增加局部 CAS。[新增接口](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/server/routers/resources.py#L364)、[整包替换](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/server/routers/skills.py#L747)、[删除接口](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/server/routers/skills.py#L1034)。
6. **通知与采用**：发布者推进 `library_revision` 并发出事件；Agent Harness 在下一任务/工具决策边界查询清单，重载受影响的 Skill，按 `agent_id/run_id` 记录实际采用的包 revision。发布者维护目标 Agent 的采用回执或任务结束状态；只有收到目标 Agent 的回执，才能声明这些 Agent 已采用。OV/VikingBot 的请求范围远端 Skill 加载可复用，但共享存储可见并不等于对所有在途 Agent 的广播确认。

第一版限制一个 release 只改一个包，单发布者用“持锁复查 + OV PUT + 读回 + 推进清单”建立成功语义。若包已替换但清单更新失败，发布日志保留待恢复状态；消费者发现清单 revision 与实包不符时暂停加载该包并重试，不把半完成发布当成已采用。作业状态依次记录 `received → classified → staged → validated → published`；随后分别记录各目标 Agent 的 `adopted` 回执或 `run_ended`。`NO_CHANGE`、验证失败和待重试是单独的终止/暂停结果。`received` 只表示提案已入队，`published` 必须以 OV 写入、读回和库清单推进为准，`adopted` 必须有该 Agent 的 Harness 版本回执。若必须一次原子切换多个包，还需不可变包版本和原子切换的库清单指针；OV 现有逐包 API 不提供多包事务。

## 与“仅替换 SkillPolicyUpdater”的区别

直接做 `StagedSkillPolicyUpdater` 也可行：让 `PatchMergePolicyOptimizer` 产出计划后暂存、验证、再发布。它最贴近现有类结构，适合**只改 `SKILL.md` 正文**的受控实验。要满足人工包 + references + 多 Agent 去重，至少还需扩展计划数据结构、修复 `PolicySet.reload()` 固定 Experience loader、核实 Skill 语义候选搜索、解决 Skill 提交结果归属，以及补 no-case 触发。因此其真实工程量未必更小。见[重载调用](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/session/train/engine.py#L80)、[错误的固定 loader](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/session/train/domain.py#L90)、[SkillPolicyUpdater 输入](https://github.com/volcengine/OpenViking/blob/144033826eb3d612de1a64b46efa7cb5f6073a41/openviking/session/train/components/skill_policy_updater.py#L235)。

第一版的完成条件是以下闭环：两名 Agent 在同类任务中遇到同一问题；只形成一个修订提案；保留原 references 并增加必要的新 reference；通过失败与正常用例；发布到共享根；第三名运行中 Agent 在下一个决策边界加载新版并回报 revision。新建包与明确授权的删除也使用同一提案、验证、单发布者协议。完整 OpenSpace runtime、多包原子事务、自动信任晋升和多个 Harness 的统一适配不在第一版范围；完成闭环后再决定是否把外围能力内收进 OV trainer。
