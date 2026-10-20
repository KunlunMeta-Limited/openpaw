# 治理测试入口

`python -m unittest discover -s governance/tests -v` 自动递归发现带__init__.py的两个专用目录：

- features：验证真实命令退出码、缺命令、超时、依赖树约束与证据不可覆盖。
- regressions：临时Git仓中的中文/空格路径、提交与脏工作区分离、越界/必改/条件、重命名、删除CHG、重复/坏表格、假批准、保护/风险降级、来源/追踪/命令错误及生成文件漂移。

Rust注释AST与scrub私有不变量测试分别在xtask/src/comments.rs和scrub.rs的cfg(test)，随workspace测试执行；不复制实现、不为目录形式暴露私有API。业务新增功能/bug采用所属crate tests/features.rs、tests/regressions.rs显式入口挂载子目录；只有实际需要时创建，禁止空壳测试。
