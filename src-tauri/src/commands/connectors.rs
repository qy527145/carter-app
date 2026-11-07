use crate::prelude::*;

// ---------------------------------------------------------------------------
// IM 连接器
// ---------------------------------------------------------------------------
//
// 每个命令都是「立即返回」，真正干活的那条 `dws` 在后台跑，输出与结果走
// `Connector*` 事件。登录要等最长 15 分钟 —— 一条同步命令挂在那里的话，界面只能
// 干等，连「取消」都点不了（命令还没返回，下一次 invoke 也没法发）。

#[tauri::command]
pub(crate) async fn list_connectors() -> Vec<carter_app_core::ConnectorRow> {
    carter_app_core::connectors::list().await
}

#[tauri::command]
pub(crate) async fn install_connector_cli(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::install_cli(shell.inner(), &vendor).await
}

#[tauri::command]
pub(crate) async fn install_connector_skill(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::install_skill(shell.inner(), &vendor).await
}

/// 扫码登录。`recommend` = 顺带授予服务端推荐权限（官方给 Agent 用的一条路）。
#[tauri::command]
pub(crate) async fn login_connector(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
    recommend: bool,
) -> Result<(), String> {
    carter_app_core::connectors::login(shell.inner(), &vendor, recommend).await
}

/// 取消正在进行的登录。同步命令：它只是杀一个进程，要立刻生效。
#[tauri::command]
pub(crate) fn cancel_connector_login(vendor: String) {
    carter_app_core::connectors::cancel_login(&vendor);
}

#[tauri::command]
pub(crate) async fn logout_connector(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::logout(shell.inner(), &vendor).await
}

/// 配置应用（飞书）：`config init --new`。和登录一样会阻塞到用户在浏览器里完成，
/// 取消走 `cancel_connector_login`。
#[tauri::command]
pub(crate) async fn configure_connector_app(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::configure_app(shell.inner(), &vendor).await
}

/// 添加邮箱账号（邮件）。密码只经环境变量交给 CLI，由它存进系统钥匙串。
#[tauri::command]
pub(crate) async fn add_mail_account(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
    name: String,
    email: String,
    imap_host: String,
    smtp_host: String,
    password: String,
) -> Result<(), String> {
    carter_app_core::connectors::add_mail_account(
        shell.inner(),
        &vendor,
        &name,
        &email,
        &imap_host,
        &smtp_host,
        &password,
    )
    .await
}

#[tauri::command]
pub(crate) async fn remove_mail_account(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
    name: String,
) -> Result<(), String> {
    carter_app_core::connectors::remove_mail_account(shell.inner(), &vendor, &name).await
}
