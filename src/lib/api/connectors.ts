import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// IM 连接器
// ---------------------------------------------------------------------------
//
// 连接器底下是「官方 CLI + 配套 skill」两层。三层状态分开给：CLI 装了没有、
// skill 装了没有、登录态还有效没有 —— 只有一个「连不上」的话，用户猜错的每一次
// 都是一轮无效操作。镜像 `connectors.rs` 的同名结构体。

export type ConnectorCli = {
  name: string;
  /** 找到的可执行文件绝对路径。`null` = 没装（或装了但不在找得到的地方）。 */
  path: string | null;
  version: string | null;
};

export type ConnectorSkill = {
  name: string;
  /** 技能目录（`~/.carter/skills/<name>`）。 */
  dir: string;
  installed: boolean;
  /** 在 `skills.disabled/` 里：装了但被停用，技能页能把它打开。 */
  disabled: boolean;
  /** `SKILL.md` 里写的 CLI 版本要求。只用来提示，不做版本比较。 */
  requires_cli: string | null;
};

export type ConnectorAuth = {
  logged_in: boolean;
  user_name: string | null;
  corp_name: string | null;
  corp_id: string | null;
  /** access token 过期时间（RFC3339）。刷新由 CLI 自己做。 */
  expires_at: string | null;
  /** 没登录 / 有问题时的一句话。 */
  message: string | null;
  hint: string | null;
};

export type ConnectorRow = {
  id: string;
  name: string;
  blurb: string;
  docs: string;
  /** 「装不上」时给人看的那条手动命令。 */
  install_hint: string;
  cli: ConnectorCli;
  skill: ConnectorSkill;
  auth: ConnectorAuth;
  /** 三层都齐了。 */
  ready: boolean;
  /** 登录方式：扫码 / 先配应用再扫码 / 填邮箱账号。 */
  auth_kind: "oauth" | "app_then_oauth" | "account";
};

export function listConnectors(): Promise<ConnectorRow[]> {
  return invoke<ConnectorRow[]>("list_connectors");
}

/** 跑官方安装脚本（下载几十 MB）。输出走 `Connector*` 事件。 */
export function installConnectorCli(vendor: string): Promise<void> {
  return invoke<void>("install_connector_cli", { vendor });
}

/** 把官方 skill 装进 `~/.carter/skills`，会话里立刻可用。 */
export function installConnectorSkill(vendor: string): Promise<void> {
  return invoke<void>("install_connector_skill", { vendor });
}

/**
 * 弹出浏览器扫码登录。`recommend` = 顺带授予服务端推荐权限（官方给 Agent 用的一条路）。
 *
 * 最长可能等 15 分钟（组织没开 CLI 权限时 `dws` 还会等管理员审批），期间输出与
 * 授权页地址都走事件。**取消**用 `cancelConnectorLogin` —— 它是一条独立的同步命令，
 * 因为这条 invoke 还没返回时也得能叫停。
 */
export function loginConnector(vendor: string, recommend: boolean): Promise<void> {
  return invoke<void>("login_connector", { vendor, recommend });
}

export function cancelConnectorLogin(vendor: string): Promise<void> {
  return invoke<void>("cancel_connector_login", { vendor });
}

export function logoutConnector(vendor: string): Promise<void> {
  return invoke<void>("logout_connector", { vendor });
}

/**
 * 配置应用（飞书）：`config init --new`，一次性在浏览器里创建应用，之后才能登录。
 * 和登录一样会阻塞到用户完成，输出与授权页地址走事件，取消用 `cancelConnectorLogin`。
 */
export function configureConnectorApp(vendor: string): Promise<void> {
  return invoke<void>("configure_connector_app", { vendor });
}

export type MailAccountDraft = {
  name: string;
  email: string;
  imapHost: string;
  smtpHost: string;
  /** 密码 / 授权码。只经环境变量交给 CLI，由它存进系统钥匙串。 */
  password: string;
};

/** 添加邮箱账号（邮件）。 */
export function addMailAccount(vendor: string, d: MailAccountDraft): Promise<void> {
  return invoke<void>("add_mail_account", {
    vendor,
    name: d.name,
    email: d.email,
    imapHost: d.imapHost,
    smtpHost: d.smtpHost,
    password: d.password,
  });
}

/** 移除邮箱账号（连同钥匙串里的密码）。 */
export function removeMailAccount(vendor: string, name: string): Promise<void> {
  return invoke<void>("remove_mail_account", { vendor, name });
}
