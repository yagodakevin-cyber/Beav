import { useEffect, useMemo, useRef, useState, type ChangeEvent, type ReactNode } from 'react';
import clsx from 'clsx';
import { ChevronDown, Download, Loader2, Trash2, X } from 'lucide-react';
import type { GeneratedAsset, PickerOption } from '../../features/media-generation';
import { generatedAssetDefaultName } from '../../features/media-generation';
import { isMediaJobTerminal, type MediaJobProjection } from '../../features/media-jobs/types';
import { mediaJobsStore } from '../../features/media-jobs/useMediaJobsStore';
import { appAlert, appConfirm } from '../../utils/appDialogs';

export type ToolboxModulePreset = {
    aspectRatio?: string;
    resolution?: string;
    durationSeconds?: number;
};

export type ToolboxModuleProps = {
    isActive: boolean;
    onExecutionStateChange?: (active: boolean) => void;
    preset?: ToolboxModulePreset | null;
    presetNonce?: number;
};

export function jobPromptText(job: MediaJobProjection): string {
    const request = job.request && typeof job.request === 'object' ? job.request as Record<string, unknown> : null;
    return String(request?.prompt || '').trim();
}

/* ---------------------------------- hooks ---------------------------------- */

export function useDismissiblePopover(open: boolean, onClose: () => void) {
    const rootRef = useRef<HTMLDivElement | null>(null);

    useEffect(() => {
        if (!open) return;

        const handlePointerDown = (event: MouseEvent) => {
            if (!(event.target instanceof Node)) return;
            if (rootRef.current?.contains(event.target)) return;
            onClose();
        };

        const handleKeyDown = (event: KeyboardEvent) => {
            if (event.key === 'Escape') onClose();
        };

        document.addEventListener('mousedown', handlePointerDown);
        document.addEventListener('keydown', handleKeyDown);
        return () => {
            document.removeEventListener('mousedown', handlePointerDown);
            document.removeEventListener('keydown', handleKeyDown);
        };
    }, [onClose, open]);

    return rootRef;
}

/* --------------------------------- selects --------------------------------- */

export function PopoverSelect({
    value,
    onChange,
    onDisabledOptionClick,
    options,
    className,
    title,
    panelClassName,
    layout = 'wrap',
    disabled = false,
    emptyText = '未选择',
    optionAlign = 'left',
}: {
    value: string;
    onChange: (value: string) => void;
    onDisabledOptionClick?: (option: PickerOption) => void;
    options: readonly PickerOption[];
    className?: string;
    title?: string;
    panelClassName?: string;
    layout?: 'wrap' | 'column';
    disabled?: boolean;
    emptyText?: string;
    optionAlign?: 'left' | 'center';
}) {
    const [open, setOpen] = useState(false);
    const rootRef = useDismissiblePopover(open, () => setOpen(false));
    const active = options.find((option) => option.value === value) || options[0];

    return (
        <div ref={rootRef} className="relative">
            <button
                type="button"
                onClick={() => {
                    if (disabled || options.length === 0) return;
                    setOpen((prev) => !prev);
                }}
                disabled={disabled || options.length === 0}
                className={clsx(
                    'inline-flex h-9 min-w-[104px] items-center gap-2 rounded-full border border-border bg-surface-primary px-3 shadow-[var(--ui-shadow-1)] transition-colors hover:border-border/70',
                    open && 'border-brand-red/50 ring-1 ring-brand-red/20',
                    (disabled || options.length === 0) && 'cursor-not-allowed opacity-55 hover:border-border',
                    className,
                )}
            >
                <span className="truncate text-[12px] font-medium text-text-primary">{active?.label || value || emptyText}</span>
                <span className="ml-auto flex h-5 w-5 items-center justify-center rounded-full bg-accent-muted text-text-tertiary">
                    <ChevronDown className="h-3 w-3" />
                </span>
            </button>

            {open && (
                <div
                    className={clsx(
                        'absolute bottom-[calc(100%+10px)] left-0 z-20 min-w-[96px] max-w-[340px] rounded-[20px] border border-border bg-surface-secondary p-3 shadow-[var(--ui-shadow-2)]',
                        panelClassName,
                    )}
                >
                    {title && <div className="mb-3 text-[13px] font-semibold text-text-secondary">{title}</div>}
                    <div className={clsx(
                        'max-h-[420px] overflow-y-auto pr-1',
                        layout === 'column' ? 'flex flex-col gap-2' : 'flex flex-wrap gap-2',
                    )}>
                        {options.map((option) => {
                            const selected = option.value === active?.value;
                            const disabledOption = option.disabled === true;
                            return (
                                <button
                                    key={option.value}
                                    type="button"
                                    onClick={() => {
                                        if (disabledOption) {
                                            onDisabledOptionClick?.(option);
                                            return;
                                        }
                                        onChange(option.value);
                                        setOpen(false);
                                    }}
                                    className={clsx(
                                        'rounded-[14px] border px-3 py-2.5 text-[12px] font-semibold transition-colors',
                                        layout === 'column' ? 'w-full' : 'min-w-[92px] flex-1',
                                        optionAlign === 'center' ? 'text-center' : 'text-left',
                                        disabledOption
                                            ? 'cursor-not-allowed border-brand-red/25 bg-brand-red/10 text-brand-red hover:bg-brand-red/15'
                                        : selected
                                            ? 'border-brand-red/50 bg-brand-red text-white'
                                            : 'border-transparent bg-surface-tertiary text-text-secondary hover:bg-accent-muted',
                                    )}
                                >
                                    <div className="flex min-w-0 items-center gap-2">
                                        {option.tone === 'danger' && (
                                            <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-brand-red" />
                                        )}
                                        <span className="truncate">{option.label}</span>
                                    </div>
                                    {option.description && (
                                        <div className={clsx(
                                            'mt-1 truncate text-[11px] font-normal',
                                            disabledOption ? 'text-brand-red/75' : selected ? 'text-white/75' : 'text-text-tertiary',
                                        )}>
                                            {option.description}
                                        </div>
                                    )}
                                </button>
                            );
                        })}
                    </div>
                </div>
            )}
        </div>
    );
}

/* ---------------------------------- chips ---------------------------------- */

export function ChipGroup<T extends string>({
    value,
    options,
    onChange,
    className,
}: {
    value: T;
    options: ReadonlyArray<{ value: T; label: string }>;
    onChange: (value: T) => void;
    className?: string;
}) {
    return (
        <div className={clsx('flex flex-wrap items-center gap-2', className)}>
            {options.map((option) => {
                const active = option.value === value;
                return (
                    <button
                        key={option.value}
                        type="button"
                        onClick={() => onChange(option.value)}
                        className={clsx(
                            'inline-flex h-9 items-center rounded-full border px-3 text-[12px] font-medium transition-colors',
                            active
                                ? 'border-brand-red/40 bg-brand-red/10 text-brand-red'
                                : 'border-border bg-surface-primary text-text-secondary hover:border-border/70',
                        )}
                    >
                        {option.label}
                    </button>
                );
            })}
        </div>
    );
}

export function ToggleChips({
    value,
    options,
    onChange,
}: {
    value: boolean;
    options: ReadonlyArray<{ value: string; label: string }>;
    onChange: (next: boolean) => void;
}) {
    return (
        <ChipGroup
            value={value ? 'on' : 'off'}
            options={options.map((option) => ({ value: option.value, label: option.label }))}
            onChange={(next) => onChange(next === 'on')}
        />
    );
}

/* -------------------------------- form areas ------------------------------- */

export function ToolSection({
    title,
    action,
    children,
    className,
}: {
    title?: string;
    action?: ReactNode;
    children: ReactNode;
    className?: string;
}) {
    return (
        <section className={clsx('rounded-[20px] border border-border bg-surface-secondary p-4', className)}>
            {(title || action) && (
                <div className="mb-3 flex items-center justify-between gap-2">
                    {title && <div className="text-[13px] font-semibold text-text-secondary">{title}</div>}
                    {action}
                </div>
            )}
            {children}
        </section>
    );
}

export function ToolField({
    label,
    children,
    className,
}: {
    label: string;
    children: ReactNode;
    className?: string;
}) {
    return (
        <div className={clsx('flex flex-col gap-2', className)}>
            <div className="text-[12px] font-medium text-text-secondary">{label}</div>
            {children}
        </div>
    );
}

export function TextInput({
    value,
    onChange,
    placeholder,
    disabled,
    type = 'text',
    className,
}: {
    value: string;
    onChange: (value: string) => void;
    placeholder?: string;
    disabled?: boolean;
    type?: string;
    className?: string;
}) {
    return (
        <input
            type={type}
            value={value}
            disabled={disabled}
            onChange={(event) => onChange(event.target.value)}
            placeholder={placeholder}
            className={clsx(
                'h-10 w-full rounded-[12px] border border-border bg-surface-primary px-3 text-[13px] text-text-primary outline-none transition-colors placeholder:text-text-tertiary focus:border-brand-red/40 disabled:opacity-55',
                className,
            )}
        />
    );
}

export function PromptTextArea({
    value,
    onChange,
    placeholder,
    rows = 6,
    className,
    textRef,
    onKeyDown,
}: {
    value: string;
    onChange: (value: string) => void;
    placeholder?: string;
    rows?: number;
    className?: string;
    textRef?: React.Ref<HTMLTextAreaElement>;
    onKeyDown?: React.KeyboardEventHandler<HTMLTextAreaElement>;
}) {
    return (
        <textarea
            ref={textRef}
            value={value}
            rows={rows}
            onChange={(event) => onChange(event.target.value)}
            onKeyDown={onKeyDown}
            placeholder={placeholder}
            className={clsx(
                'w-full resize-y rounded-[12px] border border-border bg-surface-primary p-3 text-[13px] leading-6 text-text-primary outline-none transition-colors placeholder:text-text-tertiary focus:border-brand-red/40',
                className,
            )}
        />
    );
}

/* --------------------------------- buttons --------------------------------- */

export function RunButton({
    onClick,
    disabled,
    label,
    pending = false,
    className,
}: {
    onClick: () => void;
    disabled?: boolean;
    label: string;
    pending?: boolean;
    className?: string;
}) {
    return (
        <button
            type="button"
            onClick={onClick}
            disabled={disabled || pending}
            className={clsx(
                'inline-flex h-11 w-full items-center justify-center gap-2 rounded-full bg-brand-red text-[14px] font-semibold text-white shadow-[var(--ui-shadow-1)] transition-colors hover:bg-brand-red/90 disabled:cursor-not-allowed disabled:opacity-45',
                className,
            )}
        >
            {pending ? (
                <Loader2 className="h-4.5 w-4.5 animate-spin" />
            ) : (
                label
            )}
        </button>
    );
}

export function SecondaryButton({
    onClick,
    disabled,
    children,
    className,
    title,
}: {
    onClick: () => void;
    disabled?: boolean;
    children: ReactNode;
    className?: string;
    title?: string;
}) {
    return (
        <button
            type="button"
            onClick={onClick}
            disabled={disabled}
            title={title}
            className={clsx(
                'inline-flex h-9 items-center justify-center gap-1.5 rounded-full border border-border bg-surface-primary px-3 text-[12px] font-medium text-text-secondary transition-colors hover:border-border/70 hover:text-text-primary disabled:cursor-not-allowed disabled:opacity-50',
                className,
            )}
        >
            {children}
        </button>
    );
}

/* ------------------------------ states & misc ------------------------------ */

export function ErrorBanner({ message, onClose }: { message: string; onClose?: () => void }) {
    if (!message) return null;
    return (
        <div className="flex items-start gap-2 rounded-[14px] border border-brand-red/30 bg-brand-red/10 px-3 py-2.5 text-[12px] leading-5 text-brand-red">
            <span className="min-w-0 flex-1 break-words">{message}</span>
            {onClose && (
                <button
                    type="button"
                    onClick={onClose}
                    className="shrink-0 rounded-full p-0.5 transition-colors hover:bg-brand-red/15"
                    aria-label="关闭错误提示"
                >
                    <X className="h-3.5 w-3.5" />
                </button>
            )}
        </div>
    );
}

const STATUS_TONES: Record<string, string> = {
    completed: 'border-emerald-500/30 bg-emerald-500/10 text-emerald-600',
    failed: 'border-brand-red/30 bg-brand-red/10 text-brand-red',
    cancelled: 'border-border bg-surface-tertiary text-text-tertiary',
    dead_lettered: 'border-brand-red/30 bg-brand-red/10 text-brand-red',
    cancel_requested: 'border-border bg-surface-tertiary text-text-tertiary',
};

const STATUS_LABELS: Record<string, string> = {
    accepted: '已受理',
    queued: '排队中',
    submitting: '提交中',
    submitted: '已提交',
    polling: '生成中',
    downloading: '下载中',
    persisting: '保存中',
    binding: '入库存档',
    completed: '已完成',
    failed: '失败',
    cancel_requested: '取消中',
    cancelled: '已取消',
    dead_lettered: '失败（重试耗尽）',
};

export function StatusBadge({ status }: { status: string }) {
    const normalized = String(status || '');
    const tone = STATUS_TONES[normalized] || 'border-sky-500/30 bg-sky-500/10 text-sky-600';
    const isActiveState = !STATUS_TONES[normalized];
    return (
        <span className={clsx(
            'inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11px] font-medium',
            tone,
        )}>
            {isActiveState && <Loader2 className="h-3 w-3 animate-spin" />}
            {STATUS_LABELS[normalized] || normalized}
        </span>
    );
}

export function formatTimestamp(value: string | number | null | undefined): string {
    if (value == null) return '';
    const time = typeof value === 'number' ? value : Date.parse(value);
    if (!Number.isFinite(time) || time <= 0) return '';
    const date = new Date(time);
    const now = new Date();
    const sameDay = date.toDateString() === now.toDateString();
    const hh = String(date.getHours()).padStart(2, '0');
    const mm = String(date.getMinutes()).padStart(2, '0');
    if (sameDay) return `${hh}:${mm}`;
    return `${date.getMonth() + 1}-${date.getDate()} ${hh}:${mm}`;
}

export function DownloadButton({ asset, compact = false }: { asset: GeneratedAsset; compact?: boolean }) {
    const [saving, setSaving] = useState(false);
    const source = asset.previewUrl || asset.relativePath || '';
    if (!source) return null;

    const handleSave = async () => {
        if (saving) return;
        setSaving(true);
        try {
            const result = await window.ipcRenderer.files.saveAs({
                source,
                defaultName: generatedAssetDefaultName(asset, source),
            }) as { success?: boolean; error?: string; canceled?: boolean };
            if (!result?.success && !result?.canceled) {
                throw new Error(result?.error || '保存失败');
            }
        } catch (error) {
            console.error('Failed to save generated asset:', error);
            void appAlert(error instanceof Error ? error.message : '保存失败');
        } finally {
            setSaving(false);
        }
    };

    if (compact) {
        return (
            <button
                type="button"
                onClick={() => void handleSave()}
                disabled={saving}
                title="下载"
                aria-label="下载"
                className="inline-flex h-8 w-8 items-center justify-center rounded-full border border-border bg-surface-primary text-text-secondary transition-colors hover:border-border/70 hover:text-text-primary disabled:opacity-50"
            >
                {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" />}
            </button>
        );
    }

    return (
        <SecondaryButton onClick={() => void handleSave()} disabled={saving}>
            {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" />}
            下载
        </SecondaryButton>
    );
}

export function RemoveJobButton({ jobId, onRemoved }: { jobId: string; onRemoved?: (jobId: string) => void }) {
    const [removing, setRemoving] = useState(false);

    const handleRemove = async () => {
        if (removing) return;
        const confirmed = await appConfirm('确定要移除这条生成记录吗？移除后将不再显示在列表中。', {
            title: '移除记录',
            confirmLabel: '移除',
            cancelLabel: '保留',
            tone: 'danger',
        });
        if (!confirmed) return;
        setRemoving(true);
        try {
            const result = await window.ipcRenderer.generation.deleteJob(jobId) as { success?: boolean; error?: string };
            if (result?.success === false) {
                throw new Error(result?.error || '移除失败');
            }
            // 后端归档事件会驱动 store 移除，这里兜底同步，保证列表立即收敛
            mediaJobsStore.removeJob(jobId);
            onRemoved?.(jobId);
        } catch (error) {
            console.error('Failed to remove media job:', error);
            void appAlert(error instanceof Error ? error.message : '移除失败');
        } finally {
            setRemoving(false);
        }
    };

    return (
        <button
            type="button"
            onClick={() => void handleRemove()}
            disabled={removing}
            title="移除"
            aria-label="移除"
            className="inline-flex h-8 w-8 items-center justify-center rounded-full border border-border bg-surface-primary text-text-secondary transition-colors hover:border-brand-red/40 hover:text-brand-red active:bg-surface-tertiary disabled:opacity-50"
        >
            {removing ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Trash2 className="h-3.5 w-3.5" />}
        </button>
    );
}

export function EmptyState({ title, description }: { title: string; description?: string }) {
    return (
        <div className="flex h-full min-h-[220px] flex-col items-center justify-center gap-2 px-6 text-center">
            <div className="text-[14px] font-semibold text-text-secondary">{title}</div>
            {description && <div className="max-w-[320px] text-[12px] leading-5 text-text-tertiary">{description}</div>}
        </div>
    );
}

export function ConfigHint({ message }: { message: string }) {
    if (!message) return null;
    return (
        <div className="flex items-start gap-2 rounded-[14px] border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-[12px] leading-5 text-amber-600">
            <span className="min-w-0 flex-1 break-words">{message}</span>
        </div>
    );
}

export function JobCardShell({
    job,
    onCancel,
    children,
}: {
    job: MediaJobProjection;
    onCancel?: () => void;
    children: ReactNode;
}) {
    const prompt = jobPromptText(job);
    const isRunning = !isMediaJobTerminal(String(job.status));
    const error = job.status === 'failed' || job.status === 'dead_lettered'
        ? String(job.attempt?.lastError || job.cancelReason || '生成失败，请重试')
        : '';
    const [isCancelling, setIsCancelling] = useState(false);
    const [, setElapsedTick] = useState(0);

    useEffect(() => {
        if (!isRunning) return;
        const timer = window.setInterval(() => setElapsedTick((tick) => tick + 1), 1000);
        return () => window.clearInterval(timer);
    }, [isRunning]);

    useEffect(() => {
        if (!isRunning) setIsCancelling(false);
    }, [isRunning]);

    const elapsedLabel = useMemo(() => {
        if (!isRunning) return '';
        const start = Date.parse(job.createdAt);
        if (!Number.isFinite(start)) return '';
        const totalSeconds = Math.max(0, Math.floor((Date.now() - start) / 1000));
        const minutes = Math.floor(totalSeconds / 60);
        const seconds = totalSeconds % 60;
        return `${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }, [isRunning, job.createdAt]);

    const handleCancel = () => {
        if (isCancelling) return;
        setIsCancelling(true);
        onCancel?.();
        // 若取消请求未生效（IPC 失败等），4 秒后恢复按钮可再次尝试
        window.setTimeout(() => setIsCancelling(false), 4000);
    };

    return (
        <div className="rounded-[18px] border border-border bg-surface-secondary p-4">
            <div className="mb-3 flex items-center gap-2">
                <StatusBadge status={String(job.status)} />
                {isRunning && elapsedLabel && (
                    <span className="shrink-0 text-[11px] tabular-nums text-text-tertiary" title="已用时">
                        已用时 {elapsedLabel}
                    </span>
                )}
                {prompt && <span className="min-w-0 flex-1 truncate text-[12px] text-text-tertiary">{prompt}</span>}
                {isRunning && onCancel && (
                    <button
                        type="button"
                        onClick={handleCancel}
                        disabled={isCancelling}
                        title="取消该任务"
                        aria-label="取消该任务"
                        className="inline-flex h-6 shrink-0 items-center gap-1 rounded-full border border-border bg-surface-primary px-2 text-[11px] font-medium text-text-secondary transition-colors hover:border-brand-red/40 hover:text-brand-red disabled:cursor-not-allowed disabled:opacity-55"
                    >
                        {isCancelling ? <Loader2 className="h-3 w-3 animate-spin" /> : <X className="h-3 w-3" />}
                        取消
                    </button>
                )}
                <span className="ml-auto shrink-0 text-[11px] text-text-tertiary">{formatTimestamp(job.createdAt)}</span>
            </div>
            {error && (
                <div className="mb-2 rounded-[12px] border border-brand-red/25 bg-brand-red/5 px-3 py-2 text-[12px] leading-5 text-brand-red">
                    {error}
                </div>
            )}
            {children}
        </div>
    );
}

export function Spinner({ label }: { label?: string }) {
    return (
        <div className="flex h-full min-h-[120px] flex-col items-center justify-center gap-2 text-text-tertiary">
            <Loader2 className="h-5 w-5 animate-spin" />
            {label && <span className="text-[12px]">{label}</span>}
        </div>
    );
}

export function HiddenFileInput({
    accept,
    multiple,
    onFiles,
    disabled,
}: {
    accept: string;
    multiple?: boolean;
    onFiles: (files: File[]) => void | Promise<void>;
    disabled?: boolean;
}) {
    const handleChange = async (event: ChangeEvent<HTMLInputElement>) => {
        try {
            const files = Array.from(event.target.files || []).filter((file) => file.name !== '..');
            if (files.length) await onFiles(files);
        } finally {
            event.target.value = '';
        }
    };
    return (
        <label
            className={clsx(
                'inline-flex h-9 cursor-pointer items-center justify-center gap-1.5 rounded-full border border-dashed border-border bg-surface-primary px-3 text-[12px] font-medium text-text-secondary transition-colors hover:border-brand-red/40 hover:text-text-primary',
                disabled && 'pointer-events-none opacity-50',
            )}
        >
            <input
                type="file"
                accept={accept}
                multiple={multiple}
                className="hidden"
                onChange={handleChange}
                disabled={disabled}
            />
            上传
        </label>
    );
}
