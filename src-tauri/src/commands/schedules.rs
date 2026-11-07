use crate::prelude::*;

// ---------------------------------------------------------------------------
// 定时任务
// ---------------------------------------------------------------------------

/// 全部定时任务（停用的排在后面，其余按下次触发时间）。
#[tauri::command]
pub(crate) fn list_schedules(
    shell: State<'_, Arc<AppShell>>,
) -> Vec<carter_app_core::schedules::Schedule> {
    shell.schedules.list()
}

/// 设置页里新建 / 编辑一条。
///
/// 时间是「相对现在」还是「绝对」由 `kind` 自己决定，这里只做一件事：**节奏变了**
/// 就重算下一次（否则改完「每天 9:00」到「每天 18:00」，下次触发还停在原来的 9:00）。
#[tauri::command]
pub(crate) fn save_schedule(
    shell: State<'_, Arc<AppShell>>,
    schedule: carter_app_core::schedules::Schedule,
) -> Result<(), String> {
    let old = shell.schedules.get(&schedule.id);
    let mut next = schedule;
    next.name = next.name.trim().to_string();
    next.prompt = next.prompt.trim().to_string();
    if next.name.is_empty() {
        return Err("名字不能为空".into());
    }
    if next.prompt.is_empty() {
        return Err("到点要发的那句话不能为空".into());
    }
    match &old {
        Some(o) if o.kind == next.kind => {}
        _ => next.reschedule(),
    }
    // 编辑一条已经跑完的一次性任务：给它一个新时间，它会重新启用。
    if matches!(
        next.kind,
        carter_app_core::schedules::ScheduleKind::Once { at_ms }
            if at_ms <= carter_app_core::protocol::now_ms()
    ) {
        return Err("一次性任务的时间已经过去了，请改成将来的时间".into());
    }
    shell.schedules.upsert(next)
}

#[tauri::command]
pub(crate) fn delete_schedule(shell: State<'_, Arc<AppShell>>, id: String) -> Result<(), String> {
    shell.schedules.remove(&id).map(|_| ())
}

#[tauri::command]
pub(crate) fn toggle_schedule(
    shell: State<'_, Arc<AppShell>>,
    id: String,
    enabled: bool,
) -> Result<Option<carter_app_core::schedules::Schedule>, String> {
    shell.schedules.set_enabled(&id, enabled)
}

/// 「立即运行」：开一条新会话跑一次，不动它原来的节奏。
#[tauri::command]
pub(crate) async fn run_schedule(
    shell: State<'_, Arc<AppShell>>,
    id: String,
) -> Result<String, String> {
    carter_app_core::schedules::fire_now(shell.inner(), &id).await
}
