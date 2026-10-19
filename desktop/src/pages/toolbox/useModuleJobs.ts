import { useCallback, useMemo, useState } from 'react';
import { useMediaJobSubscription } from '../../features/media-jobs/useMediaJobSubscription';
import { shallowArrayEqual, useMediaJobsStore } from '../../features/media-jobs/useMediaJobsStore';
import { isMediaJobTerminal, type MediaJobProjection } from '../../features/media-jobs/types';

export type ToolboxJobKind = 'image' | 'video' | 'audio';

/** 取消一个生成中的任务；失败仅记录日志（卡片会在 4 秒后恢复取消按钮）。 */
export async function cancelModuleJob(jobId: string): Promise<void> {
    try {
        await window.ipcRenderer.generation.cancelJob(jobId);
    } catch (error) {
        console.error('Failed to cancel media job:', error);
    }
}

/**
 * 工具箱单模块的 job 数据源：
 * - 提交后把 jobId 加入跟踪列表（onJobUpdated 事件自动推进 store）
 * - 首次挂载按 kind 拉取历史任务（free_creation 队列）
 * - 汇总输出该模块可见的全部 job（新任务在前）
 */
export function useModuleJobs(kind: ToolboxJobKind, isActive: boolean) {
    const [trackedJobIds, setTrackedJobIds] = useState<string[]>([]);

    const registerSubmittedJob = useCallback((jobId: string) => {
        setTrackedJobIds((prev) => (prev.includes(jobId) ? prev : [...prev, jobId]));
    }, []);

    const bootstrapFilter = useMemo(
        () => ({ limit: 50, kind, queueMode: 'free_creation' as const }),
        [kind],
    );

    useMediaJobSubscription(trackedJobIds, {
        enabled: isActive,
        bootstrapFilter,
        bootstrapIncludesTrackedJobs: true,
    });

    const selectJobs = useCallback(
        (state: { jobsById: Record<string, MediaJobProjection> }) => (
            Object.values(state.jobsById)
                .filter((job) => job.kind === kind && job.queueMode === 'free_creation')
        ),
        [kind],
    );

    const jobs = useMediaJobsStore(selectJobs, shallowArrayEqual);

    const sortedJobs = useMemo(
        () => [...jobs].sort((left, right) => {
            const leftTime = Date.parse(left.createdAt) || 0;
            const rightTime = Date.parse(right.createdAt) || 0;
            if (leftTime !== rightTime) return rightTime - leftTime;
            return right.jobId.localeCompare(left.jobId);
        }),
        [jobs],
    );

    const activeJobs = useMemo(
        () => sortedJobs.filter((job) => !isMediaJobTerminal(String(job.status))),
        [sortedJobs],
    );

    return { sortedJobs, activeJobs, registerSubmittedJob };
}
