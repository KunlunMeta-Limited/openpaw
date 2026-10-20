//! 职责：预留Office和固定OpenCode协议兼容层所属crate；T0尚无运行实现。
//! 输入输出：当前不提供HTTP、Run或查询API；T3建立共享宿主，T4—T7接适配器。
//! 依赖：CLI通过默认关闭的engine-host feature可选引用；核心不依赖Office DTO。
//! 禁止职责：不复制AgentLoop、Session、工具或存储，不将占位构建宣称为产品可用。
//!
//! 当前方案与里程碑见docs/engine-host/master.md。历史M5b实验检查点已由
//! 当前T0—T8计划替代；不删除历史ADR，也不由本crate文档授权修改Office。
