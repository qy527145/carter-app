use super::helpers::*;
use super::runner::{Runner, TurnResult};
use super::sinks::*;
use super::*;

impl Runner {
    /// 一轮收尾时的上下文管理：发一次最新的占用；超线了就自动压缩。
    ///
    /// 先处理 carter 在这一轮**中途**自己做的那次压缩：它换掉的是更早那段历史，
    /// 轮次号、用户原文、变更清单、时刻记录都得跟着重排，界面也要整个重建一次
    /// （见 [`Runner::apply_compaction`]）。
    pub(super) async fn after_turn(&mut self, result: &TurnResult) {
        if let Some(tier) = result.mid_compact.clone() {
            // carter 中途压完的历史已经在手上了，压之前多大已经量不到 —— 用此刻的
            // 估算当「之后」那一半，界面上那两个数只影响提示语的措辞。
            let now = self.history_tokens();
            self.apply_compaction(result.turns_before, now, tier, true);
        }
        let ctx = self.context_view();
        self.emit(SessionEvent::Context(ctx));
        if ctx.auto && ctx.threshold > 0 && ctx.used > ctx.threshold && !result.user_cancelled {
            self.compact(true).await;
        }
    }

    /// 压缩上下文。`auto` = 占用超线自动触发；否则是用户的 `/compact`。
    ///
    /// 两条路用同一个 carter 函数，差别只在目标值：自动压缩的目标是触发线（折叠旧工具
    /// 输出就够的话不必摘要），手动压缩的目标是 0（用户明说了要压，一定走到摘要）。
    ///
    /// **就地跑，不丢后台**：历史归会话任务独占（见 [`Runner::thread`]），后台那份
    /// 改完也没人接得住。摘要要几秒，这几秒里仍然读命令通道，停止 / 插队 / 作答都
    /// 照常送达；按了停止则**这一轮的下一次请求**不再发（压缩本身跑完，历史才不会
    /// 停在半截）。
    pub(super) async fn compact(&mut self, auto: bool) {
        // 带着断口去摘要的话，摘要请求本身就会被上游拒掉。
        self.settle_history();
        let before = self.history_tokens();
        // 与 `TurnRemap` 同一口径：历史里数出来的轮数。
        let turns_before = crate::view::user_turn_count(&self.thread.messages);
        self.emit(SessionEvent::CompactStarted { auto });

        let fast = self.ctx.fast_model();
        let cm = fast.unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        });
        let target = if auto {
            self.context_view().threshold
        } else {
            0
        }
        .saturating_sub(self.overhead);

        let mut sink = CompactSink::default();
        let cancel = self.ctx.handle.cancel.clone();
        let mut stopped = false;
        let mut closed = false;
        let res = {
            let compact = carter::agent::context::compact(
                &mut self.thread,
                &*cm.provider,
                &cm.model,
                target,
                &mut sink,
            );
            tokio::pin!(compact);
            loop {
                tokio::select! {
                    r = &mut compact => break r,
                    cmd = self.cmd_rx.recv() => match cmd {
                        Some(cmd) => {
                            stopped |= handle_live_command(
                                cmd,
                                &self.ctx,
                                &mut self.queued,
                                &mut self.goal,
                                &mut self.changes,
                                self.changes_turn,
                            );
                        }
                        None => {
                            closed = true;
                            break Err(carter::CarterError::Config("会话已结束".into()));
                        }
                    },
                    _ = self.ctx.handle.close.cancelled(), if !closed => {
                        closed = true;
                        break Err(carter::CarterError::Config("会话已结束".into()));
                    }
                }
            }
        };
        // 取消标记在这一段里被消费掉了，下一轮不要一进去就判成取消。
        if stopped || cancel.is_cancelled() {
            cancel.reset();
        }
        if closed {
            return;
        }
        let after = self.history_tokens();
        self.forget_reported();

        match (res, sink.tier) {
            (Err(e), _) => self.error(&format!("压缩上下文失败：{e}")),
            (Ok(()), Some(tier)) => {
                self.apply_compaction(turns_before, before, tier, auto);
            }
            (Ok(()), None) => {
                // 没摘要成：要么折叠旧工具输出就够了（自动压缩），要么历史太短切不出
                // 中段，要么摘要请求失败（carter 已降级成只折叠）。三种都要说清。
                self.ctx.handle.publish_history(&self.thread.messages);
                let msg = if let Some(why) = sink.failures.last() {
                    format!(
                        "摘要没生成出来（{why}），只折叠了较早的工具输出：~{before} → ~{after} tokens"
                    )
                } else if after < before {
                    format!("已折叠较早的工具输出：~{before} → ~{after} tokens")
                } else {
                    "对话还很短，没有可以压缩的内容".to_string()
                };
                self.notice(&msg);
            }
        }
        self.emit_context();
    }

    /// 摘要压缩之后的善后：轮次号重排（历史、用户原文、变更清单、时刻四处一致），
    /// 发布新历史，让界面整个重建。
    ///
    /// `turns_before` 是压缩**之前**的轮数 —— `TurnRemap` 靠它把旧号映到新号，
    /// 被摘要吃掉的那几轮映不到（`None`）。
    pub(super) fn apply_compaction(
        &mut self,
        turns_before: u32,
        before: u64,
        tier: String,
        auto: bool,
    ) {
        let remap = crate::history::TurnRemap::new(turns_before, &self.thread.messages);
        self.thread.user_turns = remap.after();
        let id = self.ctx.handle.id.clone();
        if !remap.is_identity() {
            if let Err(e) = crate::prompts::remap(&id, |t| remap.map(t)) {
                tracing::warn!("压缩后重排用户原文失败：{e}");
            }
            if let Err(e) = crate::changes::remap(&id, &remap) {
                tracing::warn!("压缩后重排变更记录失败：{e}");
            }
            if let Err(e) = crate::timeline::remap(&id, |t| remap.map(t)) {
                tracing::warn!("压缩后重排时刻记录失败：{e}");
            }
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        self.forget_reported();
        let after = self.history_tokens();
        self.emit(SessionEvent::Compacted {
            history: crate::view::to_views(&self.thread.messages),
            prompts: crate::prompts::load(&id),
            changes: crate::changes::load(&id),
            changes_since: crate::changes::since(&id),
            times: crate::timeline::load(&id),
            before,
            after,
            tier,
            auto,
        });
    }

    /// 历史消息部分的估算 token 数。
    pub(super) fn history_tokens(&self) -> u64 {
        carter::agent::context::estimate_tokens_for(
            &self.thread.messages,
            &self.ctx.model.tokenizer,
        )
    }

    /// 当前的上下文占用（见 [`ContextView`]）。
    ///
    /// 以上游回报的为准；还没有回报（刚打开、刚压缩完）时才用估算。
    pub(super) fn context_view(&self) -> ContextView {
        let reported = self.reported.load(Ordering::Relaxed);
        context_view(
            if reported > 0 {
                reported
            } else {
                self.history_tokens() + self.overhead
            },
            self.ctx.model.context_window,
            &self.ctx.config.agent,
        )
    }

    /// 历史被我们自己改写过（压缩、回退）：上游上次回报的占用不再对得上，
    /// 回落到估算，直到下一次主对话请求带回新数。
    pub(super) fn forget_reported(&self) {
        self.reported.store(0, Ordering::Relaxed);
    }

    pub(super) fn emit_context(&self) {
        self.emit(SessionEvent::Context(self.context_view()));
    }

    /// 轮首：开始监听 cwd 的文件变更。
    ///
    /// 监听器只是**观察**，不参与工具执行 —— 它建不起来也不影响这一轮。
    pub(super) fn begin_changes(&mut self) {
        // 上一轮理论上已经收走了；真剩下来也别把两轮的变更混在一起。
        self.changes = None;
        self.changes_turn = self.thread.user_turns;
        self.changes = ChangeWatch::start(&self.ctx.cwd, &self.ctx.settings.changes_ignore);
    }

    /// 轮末：收回这一轮真实变更的文件，落盘并推给界面。
    ///
    /// 空清单不发事件 —— 前端仍会从成功的 `write_file` / `edit_file` 参数里补，
    /// 那些落在 cwd 之外的写入（比如写 `/tmp`）监听器本来就看不见。
    pub(super) async fn finish_changes(&mut self, used_tools: bool) {
        let Some(watch) = self.changes.take() else {
            return;
        };
        // 这一轮一个工具都没调，就不可能有人动文件 —— 连那 150ms 的收尾等待一起省掉。
        if !used_tools {
            return;
        }
        let done = watch.finish().await;
        if done.files.is_empty() {
            return;
        }
        let turn = self.changes_turn;
        if let Err(e) = crate::changes::record(
            &self.ctx.handle.id,
            turn,
            &done.files,
            done.baseline.as_deref(),
        ) {
            // 落不了盘只影响历史回看，实时清单照发。
            tracing::warn!("记录本轮变更文件失败：{e}");
        }
        self.emit(SessionEvent::FilesChanged {
            turn,
            files: done.files,
        });
    }
}
