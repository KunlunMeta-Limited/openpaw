//! 职责：启动可选Host、输出发现地址并响应操作系统停止信号。
//! 输入输出：显式配置路径及端口到退出码。
//! 依赖：Host自身装配和关闭，不复制Provider选择。
//! 禁止职责：不修改全局home或工作目录，不记录凭据。
use std::path::Path;
/// 功能：运行共享Host直至Ctrl-C或终止信号，等待真实清理后退出。
/// 参数：config为本机配置路径，port为可选端口，零表示动态分配。
/// 返回：零为正常停止，二为配置/运行失败。
/// 错误：以无秘密诊断和非零退出码报告。
/// 状态：创建显式Host数据并监听loopback。
/// 并发与取消：信号触发Host关闭，等待工具和存储清理。
/// 权限：授权根与工具仅取自本机配置。
/// 关联：需求/设计REQ-OP-002、REQ-OP-009；DES-OP-006。
pub fn run(config: &Path, port: Option<u16>) -> i32 {
    run_with_env(config, port, None)
}
/// 功能：通过共用.env配置启动Host，保留原run入口。
/// 参数：config为原Host TOML；port为可选端口；env_file为可选显式模型配置。
/// 返回：正常关闭为零，配置或运行失败为二。
/// 错误：使用脱敏错误，不打印.env内容或密钥。
/// 状态：配置只在启动时读取，不修改进程环境。
/// 并发与取消：复用原Host优雅关闭和实际工具清理。
/// 权限：模型字段可回退.env；授权根和工具仍只来自TOML。
/// 关联：需求/设计REQ-OP-003、REQ-OP-009；DES-OP-002、DES-OP-006。
pub fn run_with_env(config: &Path, port: Option<u16>, env_file: Option<&Path>) -> i32 {
    let result = (|| {
        let settings = openpaw_llm::model_config::ModelEnvironment::load(
            &openpaw_core::openpaw_home(),
            env_file,
        )
        .map_err(|_| "模型.env读取失败，文件缺失、不可读或格式错误（内容已隐藏）".to_owned())?;
        let config =
            openpaw_engine_host::HostConfig::load_with_model_environment(config, &settings)
                .map_err(|e| e.to_string())?;
        let (provider, _) = settings
            .assemble(
                Some(&config.provider),
                config.model.as_deref(),
                config.base_url.as_deref(),
                config.api_key_env.as_deref(),
                "echo",
            )
            .map_err(|_| "Provider配置不合法或缺少凭据".to_owned())?;
        let rt = tokio::runtime::Runtime::new().map_err(|_| "无法启动异步运行时".to_string())?;
        rt.block_on(async move {
            let host=openpaw_engine_host::Host::with_provider(config, provider).await.map_err(|e|e.to_string())?;
            let server=match openpaw_engine_host::HostServer::bind(host.clone(),port.unwrap_or(0)).await {
                Ok(server)=>server,
                Err(e)=>{let _=host.shutdown().await;return Err(e.to_string());}
            };
            println!("openpaw engine host listening on http://{}",server.address());
            #[cfg(unix)]
            {
                let signal=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
                match signal {
                    Ok(mut terminate)=>tokio::select!{v=tokio::signal::ctrl_c()=>{if v.is_err(){let _=server.shutdown().await;return Err("停止信号监听失败".into());}},_=terminate.recv()=>{}},
                    Err(_)=>{let _=server.shutdown().await;return Err("停止信号监听失败".into());}
                }
            }
            #[cfg(not(unix))]
            if tokio::signal::ctrl_c().await.is_err(){let _=server.shutdown().await;return Err("停止信号监听失败".into());}
            server.shutdown().await.map_err(|e|e.to_string())
        })
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("engine-host: {e}");
            2
        }
    }
}
