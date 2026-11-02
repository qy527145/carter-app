// 连接器页：把 IM（钉钉，将来飞书…）接进本应用的统一入口。
//
// ## 一张卡片，三层状态
//
// 连接器底下是「官方 CLI + 配套 skill + 登录态」三层。三层**分开显示**：
// 只知道「连不上」时，用户要么重装 CLI、要么重新登录、要么去技能页把停用的技能
// 打开，猜错的每一次都是一轮无效操作。
//
// ## 为什么这些动作用弹窗而不是就地显示进度
//
// 装 CLI 要下载几十 MB、装 skill 要解包、登录要等人扫码（最长 15 分钟，组织没开
// CLI 权限时还要等管理员审批）。过程里每一行输出都推进弹窗里的日志框 ——
// 它同时是「正在干活」的证据，和出问题时唯一能拿给人看的东西。就地显示的话，
// 用户滚一下设置页就找不着进度了。
//
// 扫码登录那条路：后端带 `--no-browser` 跑 `dws`，把授权页地址解析出来推到事件里，
// 这里用系统浏览器打开它（`openUrl`）。同时那个地址也留在弹窗上 ——
// 浏览器没被拉起来（默认浏览器没配、被拦了）时，那颗按钮是唯一的出路。
//
// ## 厂商表在 Rust 那边
//
// 加飞书只需要往 `connectors.rs` 的 `VENDORS` 里再加一项，这个页面一行不用改。

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  CheckCircle2,
  CircleAlert,
  Download,
  ExternalLink,
  LogIn,
  LogOut,
  ScanLine,
  Sparkles,
  X,
} from "lucide-react";

import { EVENT_NAME, type AppEvent } from "../../lib/api";
import {
  cancelConnectorLogin,
  installConnectorCli,
  installConnectorSkill,
  listConnectors,
  loginConnector,
  logoutConnector,
  type ConnectorRow,
} from "../../lib/api";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { Button } from "../../components/ui/Button";
import { Switch } from "../../components/ui/Input";
import { Badge, Banner, Card, Modal, Spinner } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

type Job = "install-cli" | "install-skill" | "login" | "logout";

const JOB_TITLE: Record<Job, string> = {
  "install-cli": "安装 CLI",
  "install-skill": "安装技能",
  login: "扫码登录",
  logout: "退出登录",
};

/**
 * 一条日志最多留多少行。安装脚本能吐几百行（每个文件一行），全留着最后只会让
 * 真正有用的那几行滚出视野。**留尾部**：出错的永远是最后几行。
 */
const MAX_LINES = 400;

type Progress = {
  job: Job;
  vendor: string;
  lines: string[];
  /** 扫码授权页地址（只有登录会有）。 */
  url: string | null;
  running: boolean;
  error: string | null;
};

function StatusLine({
  ok,
  children,
}: {
  ok: boolean;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-1.5 text-xs leading-relaxed">
      {ok ? (
        <CheckCircle2 size={13} className="mt-0.5 shrink-0 text-tone-green" />
      ) : (
        <CircleAlert size={13} className="mt-0.5 shrink-0 text-tone-orange" />
      )}
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function ConnectorCard({ row, onStart }: { row: ConnectorRow; onStart: (job: Job) => void }) {
  const { cli, skill, auth } = row;
  const [showHint, setShowHint] = useState(false);

  return (
    <Card className="space-y-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">{row.name}</span>
            <Badge tone={row.ready ? "green" : "orange"}>
              {row.ready ? "已连接" : "未就绪"}
            </Badge>
          </div>
          <p className="mt-1 text-xs leading-relaxed text-dim">{row.blurb}</p>
        </div>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => openUrl(row.docs).catch(toastError)}
          title="官方仓库"
        >
          <ExternalLink size={12} />
        </Button>
      </div>

      {/* 这一排按钮是「下一步该干嘛」的答案：还没装的才给装，装了才给登。 */}
      <div className="flex flex-wrap gap-2">
        {cli.path === null ? (
          <Button size="sm" variant="primary" onClick={() => onStart("install-cli")}>
            <Download size={12} />
            安装 CLI
          </Button>
        ) : (
          <Button size="sm" onClick={() => onStart("install-cli")} title="重新安装 / 升级">
            <Download size={12} />
            重装 CLI
          </Button>
        )}
        {cli.path !== null && (
          <Button size="sm" onClick={() => onStart("install-skill")}>
            <Sparkles size={12} />
            {skill.installed || skill.disabled ? "重装技能" : "安装技能"}
          </Button>
        )}
        {cli.path !== null && auth.logged_in && (
          <Button size="sm" onClick={() => onStart("logout")}>
            <LogOut size={12} />
            退出登录
          </Button>
        )}
        {cli.path !== null && !auth.logged_in && (
          <Button size="sm" variant="primary" onClick={() => onStart("login")}>
            <LogIn size={12} />
            扫码登录
          </Button>
        )}
      </div>

      <div className="space-y-1.5 border-t border-edge pt-3">
        <StatusLine ok={cli.path !== null}>
          <span className="text-dim">CLI</span>{" "}
          {cli.path ? (
            <>
              <span className="mono">{cli.name}</span>
              {cli.version && <span className="text-dim"> {cli.version}</span>}
              <div className="mono truncate text-[10px] text-dim" title={cli.path}>
                {cli.path}
              </div>
            </>
          ) : (
            <span className="text-dim">
              没找到 <span className="mono">{cli.name}</span>。装完会自动认出它，不用重启应用。
            </span>
          )}
        </StatusLine>

        <StatusLine ok={skill.installed}>
          <span className="text-dim">技能</span>{" "}
          {skill.installed ? (
            <span className="mono">{skill.name}</span>
          ) : skill.disabled ? (
            <>
              <span className="mono">{skill.name}</span>{" "}
              <span className="text-dim">被停用了 —— 在「技能」页打开它即可。</span>
            </>
          ) : (
            <span className="text-dim">
              还没装。装完模型就能用{row.name}的能力，会话里立刻生效。
            </span>
          )}
          {skill.requires_cli && (
            <div className="text-[10px] text-dim">要求 CLI {skill.requires_cli}</div>
          )}
        </StatusLine>

        <StatusLine ok={auth.logged_in}>
          <span className="text-dim">登录态</span>{" "}
          {auth.logged_in ? (
            <>
              {auth.user_name ?? "已登录"}
              {auth.corp_name && <span className="text-dim"> · {auth.corp_name}</span>}
              {auth.expires_at && (
                <div className="text-[10px] text-dim">
                  有效期至 {auth.expires_at.slice(0, 19).replace("T", " ")}
                </div>
              )}
            </>
          ) : (
            <span className="text-dim">{auth.message ?? "还没登录"}</span>
          )}
          {auth.hint && <div className="text-[10px] text-dim">{auth.hint}</div>}
        </StatusLine>
      </div>

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={() => setShowHint(!showHint)}
          className="text-[10px] text-dim underline decoration-dotted underline-offset-2 hover:text-ink"
        >
          {showHint ? "收起手动安装命令" : "装不上？看手动安装命令"}
        </button>
        <span className="ml-auto text-[10px] text-dim">
          {row.id}
        </span>
      </div>
      {showHint && (
        <div className="space-y-2 rounded-lg bg-base p-2.5">
          <p className="text-[11px] leading-relaxed text-dim">
            自动安装只是替你跑下面这条命令。在终端里跑完、确认{" "}
            <span className="mono">{cli.name}</span> 能用了，再回来点「重装 CLI」以外
            的任何按钮 —— 界面会自己认出它。
          </p>
          <pre className="mono max-h-32 overflow-auto whitespace-pre-wrap break-all rounded-md bg-card p-2 text-[10px] leading-relaxed text-dim">
            {row.install_hint}
          </pre>
        </div>
      )}
    </Card>
  );
}

/** 进程跑着的时候那块弹窗：日志 + （登录时）授权页 + 取消。 */
function ProgressDialog({ p, onCancel, onClose }: {
  p: Progress;
  onCancel: () => void;
  onClose: () => void;
}) {
  const log = useRef<HTMLDivElement>(null);
  const url = p.url;

  // 一直在滚到底：安装 / 登录都可能刷几百行，不跟着滚的话用户看到的永远是开头。
  useEffect(() => {
    const el = log.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [p.lines.length]);

  return (
    <Modal
      title={JOB_TITLE[p.job]}
      width={560}
      onClose={p.running ? () => {} : onClose}
      footer={
        p.running ? (
          <Button onClick={onCancel}>
            <X size={12} />
            取消
          </Button>
        ) : (
          <Button variant="primary" onClick={onClose}>
            关闭
          </Button>
        )
      }
    >
      <div className="space-y-3">
        {url && (
          <Banner kind={p.error ? "warn" : "info"}>
            <div className="mb-2">
              浏览器没有自动弹出、或扫到一半关了，可以点下面这个按钮重新打开授权页。
            </div>
            <Button
              size="sm"
              variant="primary"
              onClick={() => openUrl(url).catch(toastError)}
            >
              <ScanLine size={12} />
              打开授权页扫码
            </Button>
          </Banner>
        )}

        {p.error && <Banner kind="error">{p.error}</Banner>}

        <div className="flex items-center gap-2 text-xs text-dim">
          {p.running ? <Spinner size={12} /> : null}
          <span>
            {p.running
              ? p.job === "login"
                ? "等待扫码授权…（组织没开 CLI 权限时，可能还要等管理员审批）"
                : "执行中…"
              : p.error
                ? "没有完成"
                : "完成"}
          </span>
        </div>

        <div
          ref={log}
          className="mono h-64 overflow-y-auto rounded-lg bg-base p-2.5 text-[10px] leading-relaxed whitespace-pre-wrap break-all text-dim"
        >
          {p.lines.length === 0 ? <span>（还没有输出）</span> : p.lines.join("\n")}
        </div>
      </div>
    </Modal>
  );
}

export function ConnectorsPage() {
  const state = useAsync(listConnectors, []);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [recommend, setRecommend] = useState(true);
  // 哪一个 vendor 正在登录。取消要同步拿到它，所以用 ref 而不是 state。
  const loginVendor = useRef<string | null>(null);

  // 订阅事件：日志逐行进弹窗，地址单独收一条，结束时重拉卡片。
  //
  // 用 ref 拿最新的 setProgress / reload：事件监听只挂一次，回调里闭包捕获的
  // 旧值会让一串日志里只有第一行进弹窗（典型的「怎么只显示一行」）。
  const settle = useRef<(() => void) | null>(null);
  settle.current = () => state.reload();

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listenConnectorEvents((e) => {
      if (e.type === "connector_output") {
        setProgress((p) => {
          if (!p || p.vendor !== e.vendor) return p;
          const lines = [...p.lines, e.line];
          if (lines.length > MAX_LINES) lines.splice(0, lines.length - MAX_LINES);
          return { ...p, lines };
        });
        return;
      }
      if (e.type === "connector_auth_url") {
        setProgress((p) => (p && p.vendor === e.vendor ? { ...p, url: e.url } : p));
        return;
      }
      if (e.type === "connector_done") {
        if (e.message) {
          if (e.ok) toastOk(e.message);
          else toastError(e.message);
        }
        settle.current?.();
        return;
      }
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
    // 只挂一次：回调里用不到任何随时间变化的变量。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const start = useCallback(
    async (row: ConnectorRow, job: Job) => {
      setProgress({
        job,
        vendor: row.id,
        lines: [],
        url: null,
        running: true,
        error: null,
      });
      if (job === "login") loginVendor.current = row.id;

      try {
        if (job === "install-cli") await installConnectorCli(row.id);
        else if (job === "install-skill") await installConnectorSkill(row.id);
        else if (job === "logout") await logoutConnector(row.id);
        else await loginConnector(row.id, recommend);
        setProgress((p) => (p ? { ...p, running: false } : p));
      } catch (e) {
        const msg = typeof e === "string" ? e : String(e);
        setProgress((p) => (p ? { ...p, running: false, error: msg } : p));
      } finally {
        if (job === "login") loginVendor.current = null;
        settle.current?.();
      }
    },
    // `recommend` 是用户当场勾的，必须进依赖 —— 否则第一次登录时勾了「顺带授权」，
    // 取消后重开弹窗里还是按老的闭包跑（授权范围与界面上显示的不一致）。
    // `state` 每次渲染都是新对象，放进依赖等于每帧重建；重拉走 ref。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [recommend],
  );

  async function cancel() {
    const vendor = loginVendor.current;
    if (vendor) {
      try {
        await cancelConnectorLogin(vendor);
      } catch (e) {
        toastError(e);
      }
    }
    setProgress((p) => (p ? { ...p, running: false, error: p.error ?? "已取消" } : p));
  }

  return (
    <>
      <PageIntro>
        连接器把外部 IM 接进本应用：底层是厂商官方的 CLI 加配套技能，本应用只负责安装、登录与状态展示。
        装好并登录之后，直接在对话里说「帮我看看明天的日程」这类话就行。
      </PageIntro>

      <div className="mb-4 flex items-center gap-2 rounded-lg bg-card px-3 py-2 text-xs text-dim">
        <Switch
          checked={recommend}
          onChange={setRecommend}
          label="登录时顺带授权推荐权限"
        />
        <span className="min-w-0 flex-1 leading-relaxed">
          登录时一次性授予官方推荐的全部数据权限。不勾的话，第一次用某个能力时会各自弹一次授权页 ——
          功能一样，只是更碎。
        </span>
      </div>

      <AsyncBody state={state} empty="没有可用的连接器。">
        {(rows) => (
          <div className="space-y-3">
            {rows.map((row) => (
              <ConnectorCard
                key={row.id}
                row={row}
                onStart={(job) => void start(row, job)}
              />
            ))}
          </div>
        )}
      </AsyncBody>

      {progress && (
        <ProgressDialog
          p={progress}
          onCancel={() => void cancel()}
          onClose={() => setProgress(null)}
        />
      )}
    </>
  );
}

/**
 * 订阅 `carter://event`，只把连接器那三条过滤出来。
 *
 * 单独的 `listen` 而不是接进 `useAppEvents`：那条总线的回调是给会话/侧栏用的，
 * 一个设置页的临时弹窗不该在那里留一份全局状态。多挂一个监听器的代价是零
 * （Tauri 的事件是按名分发到各监听器的）。
 */
function listenConnectorEvents(
  fn: (e: Extract<AppEvent, { type: `connector_${string}` }>) => void,
) {
  return listen<AppEvent>(EVENT_NAME, ({ payload }) => {
    if (
      payload.type === "connector_output" ||
      payload.type === "connector_auth_url" ||
      payload.type === "connector_done"
    ) {
      fn(payload);
    }
  });
}
