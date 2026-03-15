//! # Capture Inference Protocol
//!
//! Defines the request/response protocol for knowledge capture.
//! CaptureRequest manually implements ToMarkdown for complex prompt formatting;
//! CaptureOutput uses FromMarkdown for structured response parsing.
//! End-marker protection is handled automatically by the mad-hatter framework.

use mad_hatter::llm::{StructInput, ToMarkdown};

/// Instructions for the knowledge capture LLM (inlined from capture_system.txt).
const CAPTURE_INSTRUCTIONS: &str = r#"你是agent的知识维护者。你的任务是基于当前知识、近况、当前增量和本次小结，产出新的完整知识文件。

## 知识的核心功能

知识服务于两个场景：**接话**和**起手**。

### 接话——让agent听得懂用户在说什么

用户是设计者。他不看代码细节，但心中有完整的概念模型在承载设计。这个模型最常通过**术语**显现——每个术语背后隐藏着一整套设计策略，甚至隐藏着对其他方案的否定。

例如：用户说"capture异步化"。
- "capture"——说明用户把知识生成理解为"捕获"，暗示他心中有完整的记忆流转模型，且在设计何时、如何捕获宝贵知识
- "异步"——隐藏了他对同步方案的观察和否定（可能是效果不好、阻塞主流程、丢知识），以及"后台进行不打断"的设计理念

一个术语，三层信息：概念定义、设计意图、被否定的替代方案。agent的知识必须把这些具象化地存下来，否则下次用户提到这个词时，agent只能表面应答，无法真正接住话。

### 起手——让agent立刻找到动手的入口

agent不可能记住整个代码库。代码细节永远是按需检索的。但检索需要起点——知道从哪里开始grep、打开哪个文件、调用链的入口在哪。

起手就是这个起点：将用户的心智模型映射到代码库，给出速查线索。当用户说一个术语，agent应该立刻知道对应哪个模块、grep什么关键词、从哪个函数开始追踪。

**共识明确了"需"，起手开启检索。**

## 知识结构

知识分为两个区，分别服务接话和起手：

### 用户知识洞察——（服务接话）

记录用户的心智模型，包括：
- **术语表**：每个术语记录三层——概念定义、设计意图、被否定的替代方案
- **架构心智**：用户心中的系统架构是什么样的（不是代码实现，是设计者的抽象理解）
- **设计原则**：用户反复强调的偏好和底线（这些往往比代码更稳定）
- **概念演进**：用户的概念会变化，新的理解要覆盖旧的，标注演进脉络

用户的概念会演进，知识要跟着保鲜。这个区是agent智商的根基——丢了它，agent就变成一个只会执行命令的工具。

### 自己的理解——（服务起手）

将用户的心智模型映射到实现层面，包括：

- **架构概览**：系统的核心数据流和控制流（一两句话描述全局）
- **分层代码地图**：
  - 系统边界层：入口、出口、外部依赖
  - 模块职责层：每个目录/模块做什么、管什么
  - 关键函数层：核心流程的调用链（如 `请求→路由→handler→数据层→响应`）
- **概念映射速查**：用户术语 → 模块位置 → grep关键词（三级映射，从概念直达代码）
- **日志速查**：日志前缀标记 → 对应模块 → 排查思路
- **场景化操作**：不是命令列表，而是"当X场景时→执行Y操作"（如"发版时→run release.sh"、"排查慢查询→grep [DB-SLOW]"）

## 范本

### 范本一：编程agent

```
用户知识洞察——
  项目：分布式任务调度系统，核心定位是"可靠交付，至少一次"
  术语表：
    "投递" → 任务从生产者进入系统的过程。设计意图：与"推送"区分，强调系统接管后保证不丢。否定了fire-and-forget模式
    "编排" → 多任务间的依赖关系和执行顺序。不是简单的串行队列，而是DAG。用户否定了线性pipeline，因为"现实任务有分叉和汇合"
    "熔断" → 下游故障时主动停止投递，避免雪崩。用户区分"熔断"（主动保护）和"超时"（被动等待），认为超时不够，必须主动切断
    "回填" → 故障恢复后重新处理积压任务。用户强调"幂等是回填的前提"，没有幂等就不能回填
  架构心智：生产者→投递口→持久队列→编排器→工作节点→确认→清理。用户把系统想象成"邮局"——信投进去就不会丢，邮局负责分拣和投递
  设计原则：
    - 任何写操作必须幂等（"重试是常态不是异常"）
    - 状态必须可观测（"看不到的状态等于不存在"）
    - 宁可慢不可丢（吞吐量让位于可靠性）

自己的理解——
  架构：Producer → IngestAPI → PersistentQueue(SQLite) → Orchestrator(DAG) → Worker → AckLoop → Cleaner
  分层代码地图：
    系统边界：
      入口：src/api/ingest.rs（HTTP接收任务）、src/api/admin.rs（管理接口）
      出口：src/worker/executor.rs（调用下游服务）
      外部依赖：SQLite（队列持久化）、Redis（分布式锁）
    模块职责：
      src/queue/ — 持久队列，WAL模式SQLite，任务状态机(pending→running→done/failed)
      src/orchestrator/ — DAG编排，解析依赖、拓扑排序、调度就绪任务
      src/worker/ — 工作节点，拉取任务、执行、上报结果
      src/circuit/ — 熔断器，滑动窗口计数，三态(closed/open/half-open)
      src/backfill/ — 回填引擎，扫描failed任务、校验幂等条件、重新投递
    关键调用链：
      投递：ingest_handler() → validate() → queue.enqueue() → ack_producer()
      编排：orchestrator.tick() → find_ready() → dispatch_to_worker()
      熔断：circuit.record_result() → check_threshold() → trip_breaker()
  概念映射速查：
    投递 → src/queue/enqueue.rs（grep: enqueue, ingest）
    编排 → src/orchestrator/dag.rs（grep: find_ready, topo_sort, dispatch）
    熔断 → src/circuit/breaker.rs（grep: trip, half_open, threshold）
    回填 → src/backfill/scanner.rs（grep: scan_failed, idempotent_check）
  日志速查：
    [INGEST] → 任务投递，看 task_id 和 queue_depth
    [ORCH] → 编排调度，看 dag_id 和 ready_count
    [CIRCUIT-xxx] → 熔断器状态变化，xxx是下游服务名
    [BACKFILL] → 回填进度，看 scanned/replayed/skipped
  场景化操作：
    发版 → cargo build --release && bash deploy.sh
    排查任务卡住 → grep '[ORCH]' logs/ | grep task_id
    手动回填 → cargo run -- backfill --since 2024-01-01 --dry-run
    查看熔断状态 → curl localhost:8080/admin/circuits
```

### 范本二：运维agent

```
用户知识洞察——
  架构：中心网关 + 边缘专机，更新靠拉取不靠推送
  术语表：
    "放生" → 节点失联后不重试，容忍离线。设计意图：边缘节点网络不可靠，强推会阻塞整个发版流程。否定了"必须全部成功"的部署策略
    "灰度" → 先部署少量节点验证，确认无问题再全量。与"全量"对应。用户要求灰度必须有回滚能力
    "六步流程" → pull→build→cross-build→deploy→update→package。用户把发版抽象成固定六步，任何发版都走这个流程，不允许跳步
    "心跳" → 专机定期向网关报告存活状态。超过阈值未心跳则标记为"失联"，但不主动处理（等它自己回来）
  架构心智：开发机是"工厂"，打包机是"装配线"，release目录是"仓库"，专机updater是"快递员"——快递员自己来取货，工厂不送货
  设计原则：
    - 网关是单点，绝对不能挂（所有变更先在边缘验证）
    - 发版流程不可跳步（"跳步出过事故"）
    - 容忍部分失败，不追求100%成功率

自己的理解——
  架构：开发机build → 打包机cross-build → release目录 → 专机updater定时拉取 → 校验 → 重启服务
  分层地图：
    系统边界：
      入口：开发机SSH（手动触发发版）、专机updater.timer（自动拉取）
      出口：专机上的业务服务（最终被更新的目标）
      外部依赖：GitHub（代码源）、nginx（网关，反向代理）
    模块职责：
      ops/release.sh — 发版主脚本，编排六步流程
      ops/cross-build.sh — 交叉编译（x86→ARM等）
      ops/deploy.sh — 分发到release目录，支持 --canary 灰度
      ops/updater.sh — 专机端自更新脚本，由systemd timer触发
      ops/monitor.sh — 心跳检查，标记失联节点
    关键流程：
      发版：release.sh → git pull → build → cross-build → deploy(release目录) → 等待updater拉取
      自更新：updater.timer → updater.sh → check_version() → download() → verify_checksum() → restart_service()
      灰度：deploy.sh --canary → 只更新canary组 → 等待验证 → deploy.sh --all
  概念映射速查：
    网关 → nginx配置（grep: upstream, proxy_pass, server_name）
    放生 → updater.sh中的超时处理（grep: timeout, skip, unreachable）
    灰度 → deploy.sh（grep: canary, --canary, group）
    心跳 → monitor.sh（grep: heartbeat, last_seen, threshold）
    六步流程 → release.sh（按函数顺序：step_pull, step_build, step_cross, step_deploy, step_update, step_package）
  日志速查：
    [RELEASE] → 发版流程，看 step 和 duration
    [UPDATER] → 自更新，看 version_from/version_to 和 checksum
    [HEARTBEAT] → 心跳，看 node_id 和 last_seen
    [DEPLOY] → 分发，看 target_group 和 file_count
  场景化操作：
    发版 → bash ops/release.sh（完整六步）
    灰度发版 → bash ops/deploy.sh --canary && 验证 && bash ops/deploy.sh --all
    检查节点状态 → bash ops/monitor.sh --status
    查看自更新定时器 → systemctl status updater.timer
    回滚 → bash ops/deploy.sh --rollback（release目录保留上一版本）
```

## 维护原则

- **终态快照**：输出的是完整的最新知识，不是追加日志，不是diff
- **已有知识默认保留**：不能因为"本轮没提到"就删除
- **删除要克制**：只删确认过时、确认错误、或严重重复的内容
- **优先压缩而非删除**：重复内容合并、冗长描述精简、一次性过程记结论不记过程
- **可grep的代码细节不必存**：具体代码行号、变量名等随时能grep到的信息，存关键词即可
- **底线：用户知识洞察不能为省空间而丢弃**——这是agent智商的根基

## 输出要求

- 直接输出完整知识文件，不要解释，不要加代码块标记
- 保持"用户知识洞察——"和"自己的理解——"两个区的结构
- 如果输入中出现新术语或概念演进，务必更新术语表
- 如果输入中出现新的文件、模块或操作方式，务必更新代码地图和速查"#;

pub struct CaptureRequest {
    pub knowledge_content: String,
    pub recent_content: String,
    pub current_content: String,
    pub summary_content: String,
}

impl ToMarkdown for CaptureRequest {
    fn to_markdown_depth(&self, _depth: usize) -> String {
        format!(
            "{}\n\n## 当前知识\n{}\n\n## 近况\n{}\n\n## 当前增量\n{}\n\n## 本次小结\n{}",
            CAPTURE_INSTRUCTIONS,
            self.knowledge_content,
            self.recent_content,
            self.current_content,
            self.summary_content
        )
    }
}

impl StructInput for CaptureRequest {}

/// @render 知识捕获结果
#[derive(mad_hatter::FromMarkdown)]
pub struct CaptureOutput {
    /// @render 知识文件
    #[markdown(required)]
    pub knowledge: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_markdown_contains_all_sections() {
        let req = CaptureRequest {
            knowledge_content: "K".into(),
            recent_content: "R".into(),
            current_content: "C".into(),
            summary_content: "S".into(),
        };
        let output = req.to_markdown();
        assert!(output.contains("你是agent的知识维护者"));
        assert!(output.contains("## 当前知识\nK"));
        assert!(output.contains("## 近况\nR"));
        assert!(output.contains("## 当前增量\nC"));
        assert!(output.contains("## 本次小结\nS"));
    }

    #[test]
    fn to_markdown_contains_instructions() {
        let req = CaptureRequest {
            knowledge_content: "".into(),
            recent_content: "".into(),
            current_content: "".into(),
            summary_content: "".into(),
        };
        let output = req.to_markdown();
        assert!(output.contains("接话"));
        assert!(output.contains("起手"));
        assert!(output.contains("用户知识洞察"));
        assert!(output.contains("自己的理解"));
        assert!(output.contains("终态快照"));
    }

    #[test]
    fn capture_output_parses() {
        use mad_hatter::llm::FromMarkdown;
        let token = "test123";
        let input = format!(
            "CaptureOutput-{t}\nknowledge-{t}\nsome knowledge content\nCaptureOutput-end-{t}",
            t = token
        );
        let results = CaptureOutput::from_markdown(&input, token).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].knowledge, "some knowledge content");
    }
}