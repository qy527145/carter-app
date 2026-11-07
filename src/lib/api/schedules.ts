import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 定时任务
// ---------------------------------------------------------------------------

/** 触发方式。镜像 `schedules.rs::ScheduleKind`（`tag = "kind"`）。 */
export type ScheduleKind =
  /** 一次性，到点跑一次就自己停用。 */
  | { kind: "once"; at_ms: number }
  /** 每隔 N 分钟。 */
  | { kind: "interval"; every_minutes: number }
  /** 每天的 `hour:minute`（本机时间）。 */
  | { kind: "daily"; hour: number; minute: number };

/** 镜像 `schedules.rs::Schedule`。 */
export type Schedule = {
  id: string;
  name: string;
  /** 到点原样发出去的正文。 */
  prompt: string;
  kind: ScheduleKind;
  cwd: string;
  model: string | null;
  enabled: boolean;
  created_at: number;
  next_at: number;
  last_fired_at: number | null;
  last_session_id: string | null;
  runs: number;
  last_error: string | null;
};

export function listSchedules(): Promise<Schedule[]> {
  return invoke<Schedule[]>("list_schedules");
}

/** 新建 / 编辑一条。节奏变了后端会重算下一次触发。 */
export function saveSchedule(schedule: Schedule): Promise<void> {
  return invoke<void>("save_schedule", { schedule });
}

export function deleteSchedule(id: string): Promise<void> {
  return invoke<void>("delete_schedule", { id });
}

/** 暂停 / 恢复。返回更新后的那条（重新打开时下一次可能被重算）。 */
export function toggleSchedule(id: string, enabled: boolean): Promise<Schedule | null> {
  return invoke<Schedule | null>("toggle_schedule", { id, enabled });
}

/** 立即跑一次，返回新开的会话 id。不动它原来的节奏。 */
export function runSchedule(id: string): Promise<string> {
  return invoke<string>("run_schedule", { id });
}
