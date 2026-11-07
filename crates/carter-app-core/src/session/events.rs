use super::helpers::*;
use super::runner::Runner;
use super::*;

impl Runner {
    /// 进行中那一轮要不要记下来：只有会被侧边聊天看的主会话才记。
    pub(super) fn live(&self) -> Option<Arc<crate::shell::LiveLog>> {
        self.ctx
            .handle
            .parent_id
            .is_none()
            .then(|| self.ctx.handle.live.clone())
    }

    /// 改目标：落内存、落盘、发事件。三件事必须一起做 —— 少落盘的话重启就丢，
    /// 少发事件的话界面上的目标条会和实际状态对不上。
    pub(super) fn set_goal(&mut self, goal: Option<Goal>) {
        self.goal = goal;
        self.publish_goal();
    }

    pub(super) fn publish_goal(&self) {
        publish_goal(&self.goal, &self.ctx.handle.id, &self.ctx.shell);
    }

    pub(super) fn emit(&self, event: SessionEvent) {
        match event {
            SessionEvent::TurnStarted { .. } => self.ctx.handle.busy.store(true, Ordering::SeqCst),
            SessionEvent::Idle => {
                self.ctx.handle.busy.store(false, Ordering::SeqCst);
                // 这一轮读过信、发信方要回执的，现在送（历史快照此时已经是这一轮的结尾）。
                crate::peers::on_idle(&self.ctx.shell, &self.ctx.handle);
                // 这一轮动过团队数据（`.carter/`）就收成一条提交。放在 idle 而不是
                // 每次工具调用：一轮里建三个任务改两个状态是**一件事**，应该是一条
                // 提交。侧边聊天不碰团队数据（它没挂 `team` 工具），提交是空转。
                if self.ctx.include_task {
                    crate::team::git::commit_turn_if_team(&self.ctx.cwd, self.thread.user_turns);
                }
            }
            _ => {}
        }
        emit(&self.ctx.shell, &self.ctx.handle.id, event);
    }

    pub(super) fn notice(&self, message: &str) {
        self.emit(SessionEvent::Notice {
            message: message.to_string(),
        });
    }

    pub(super) fn error(&self, message: &str) {
        self.emit(SessionEvent::Error {
            message: message.to_string(),
        });
    }
}
