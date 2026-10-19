import { useCallback, useEffect, useRef, useState } from 'react';
import { Clapperboard, Music, X } from 'lucide-react';
import {
    assetsFromJobProjection,
    buildVideoGenerationRequest,
    errorMessageFromJobProjection,
    generationSubmitSource,
    submitVideoGeneration,
    validateVideoGenerationRequest,
    type ReferenceItem,
    type SettingsShape,
    type VideoGenerationMode,
} from '../../features/media-generation';
import { resolveAssetUrl } from '../../utils/pathManager';
import { cancelModuleJob, useModuleJobs } from './useModuleJobs';
import {
    ConfigHint,
    EmptyState,
    ErrorBanner,
    JobCardShell,
    PopoverSelect,
    PromptTextArea,
    RunButton,
    ToolField,
    ToolSection,
    DownloadButton,
    ChipGroup,
    HiddenFileInput,
    type ToolboxModuleProps,
} from './shared';
import {
    VIDEO_ASPECT_RATIO_OPTIONS,
    VIDEO_DURATION_OPTIONS,
    VIDEO_MODE_OPTIONS,
    VIDEO_RESOLUTION_OPTIONS,
    fileMatchesAccept,
    filesToReferenceItems,
    isVideoReference,
    makeId,
} from './options';

const VIDEO_ACCEPT = 'video/mp4,video/quicktime,video/webm,.mp4,.mov,.webm';
const MAX_VIDEO_REFERENCES = 3;

function ReferenceSlot({
    item,
    onClear,
    label,
    hint,
    onFiles,
    accept,
    disabled,
}: {
    item: ReferenceItem | null;
    onClear: () => void;
    label: string;
    hint: string;
    onFiles: (files: File[]) => void | Promise<void>;
    accept: string;
    disabled?: boolean;
}) {
    const isVideo = isVideoReference(item);
    const isAudio = String(item?.dataUrl || '').startsWith('data:audio/');
    return (
        <div className="flex flex-col gap-2">
            <div className="text-[12px] font-medium text-text-secondary">{label}</div>
            {item ? (
                <div className="group relative flex h-[76px] items-center gap-3 overflow-hidden rounded-[12px] border border-border bg-surface-primary px-3">
                    {isVideo ? (
                        <Clapperboard className="h-6 w-6 shrink-0 text-text-secondary" />
                    ) : isAudio ? (
                        <Music className="h-6 w-6 shrink-0 text-text-secondary" />
                    ) : (
                        <img src={item.dataUrl} alt={item.name} className="h-[60px] w-[60px] shrink-0 rounded-[8px] object-cover" />
                    )}
                    <span className="min-w-0 flex-1 truncate text-[12px] text-text-secondary">{item.name}</span>
                    <button
                        type="button"
                        onClick={onClear}
                        className="shrink-0 rounded-full p-1 text-text-tertiary transition-colors hover:bg-brand-red/10 hover:text-brand-red"
                        aria-label={`移除${label}`}
                    >
                        <X className="h-3.5 w-3.5" />
                    </button>
                </div>
            ) : (
                <div className="flex items-center gap-2">
                    <HiddenFileInput accept={accept} onFiles={onFiles} disabled={disabled} />
                    <span className="text-[11px] text-text-tertiary">{hint}</span>
                </div>
            )}
        </div>
    );
}

export function VideoModule({ isActive, onExecutionStateChange, preset, presetNonce = 0 }: ToolboxModuleProps) {
    const [settings, setSettings] = useState<SettingsShape>({});
    const [prompt, setPrompt] = useState('');
    const [videoMode, setVideoMode] = useState<VideoGenerationMode>('text-to-video');
    const [references, setReferences] = useState<ReferenceItem[]>([]);
    const [firstFrame, setFirstFrame] = useState<ReferenceItem | null>(null);
    const [lastFrame, setLastFrame] = useState<ReferenceItem | null>(null);
    const [firstClip, setFirstClip] = useState<ReferenceItem | null>(null);
    const [drivingAudio, setDrivingAudio] = useState<ReferenceItem | null>(null);
    const [isReadingRefs, setIsReadingRefs] = useState(false);
    const [aspectRatio, setAspectRatio] = useState<'16:9' | '9:16'>('16:9');
    const [resolution, setResolution] = useState<'720p' | '1080p'>('720p');
    const [durationSeconds, setDurationSeconds] = useState(8);
    const [generateAudio, setGenerateAudio] = useState(false);
    const [error, setError] = useState('');
    const [isSubmitting, setIsSubmitting] = useState(false);
    const errorRef = useRef<HTMLDivElement | null>(null);

    const { sortedJobs, activeJobs, registerSubmittedJob } = useModuleJobs('video', isActive);

    useEffect(() => {
        if (!error) return;
        errorRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    }, [error]);

    useEffect(() => {
        let cancelled = false;
        void (async () => {
            try {
                const next = await window.ipcRenderer.getSettings() as SettingsShape;
                if (!cancelled) setSettings((next || {}) as SettingsShape);
            } catch (loadError) {
                console.error('Failed to load settings for video module:', loadError);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, []);

    useEffect(() => {
        onExecutionStateChange?.(activeJobs.length > 0);
    }, [activeJobs.length, onExecutionStateChange]);

    useEffect(() => {
        if (!preset) return;
        if (preset.aspectRatio === '9:16' || preset.aspectRatio === '16:9') setAspectRatio(preset.aspectRatio);
        if (preset.resolution === '720p' || preset.resolution === '1080p') setResolution(preset.resolution);
        if (typeof preset.durationSeconds === 'number' && preset.durationSeconds > 0) {
            setDurationSeconds(Math.min(12, Math.max(5, Math.round(preset.durationSeconds))));
        }
    }, [preset, presetNonce]);

    const hasVideoConfig = Boolean((settings.video_endpoint || settings.api_endpoint || '').trim())
        && Boolean((settings.video_api_key || settings.api_key || '').trim());

    const appendReferenceFiles = useCallback(async (files: File[]) => {
        const accepted = files.filter((file) => fileMatchesAccept(file, 'image/*'));
        if (!accepted.length) return;
        setIsReadingRefs(true);
        try {
            const nextItems = await filesToReferenceItems(accepted, MAX_VIDEO_REFERENCES);
            setReferences((prev) => [...prev, ...nextItems].slice(0, MAX_VIDEO_REFERENCES));
        } catch (readError) {
            console.error('Failed to read video references:', readError);
            setError('参考图读取失败，请重试');
        } finally {
            setIsReadingRefs(false);
        }
    }, []);

    const pickSingleFile = useCallback(async (files: File[], accept: string): Promise<ReferenceItem | null> => {
        const accepted = files.find((file) => fileMatchesAccept(file, accept));
        if (!accepted) return null;
        try {
            const [item] = await filesToReferenceItems([accepted], 1);
            return item || null;
        } catch (readError) {
            console.error('Failed to read file:', readError);
            setError('文件读取失败，请重试');
            return null;
        }
    }, []);

    const handleGenerate = useCallback(async () => {
        if (isSubmitting) return;
        const request = buildVideoGenerationRequest({
            prompt,
            title: '',
            projectId: '',
            model: '',
            aspectRatio,
            resolution,
            durationSeconds,
            generateAudio,
            videoMode,
            referenceItems: references,
            firstFrame,
            lastFrame,
            firstClip,
            drivingAudio,
        });
        const validationError = validateVideoGenerationRequest(request, { hasVideoConfig });
        if (validationError) {
            setError(validationError);
            return;
        }

        setError('');
        setIsSubmitting(true);
        try {
            const result = await submitVideoGeneration(window.ipcRenderer.generation, request, {
                clientRequestId: makeId('toolbox-video'),
                source: generationSubmitSource('standalone'),
                queueMode: 'free_creation',
            });
            registerSubmittedJob(result.jobId);
            // 保留提示词与素材，便于微调后再次生成
        } catch (submitError) {
            setError(submitError instanceof Error ? submitError.message : '生视频提交失败');
        } finally {
            setIsSubmitting(false);
        }
    }, [
        aspectRatio,
        drivingAudio,
        firstClip,
        firstFrame,
        generateAudio,
        hasVideoConfig,
        isSubmitting,
        lastFrame,
        prompt,
        references,
        registerSubmittedJob,
        resolution,
        durationSeconds,
        videoMode,
    ]);

    const configHint = !hasVideoConfig
        ? '尚未配置生视频服务：请前往「设置 → AI 源」完成接口地址与密钥配置后使用'
        : '';

    return (
        <div className="flex h-full min-h-0 gap-4 p-4">
            <aside className="flex w-[340px] shrink-0 flex-col gap-3 overflow-y-auto pr-1">
                <ToolSection title="提示词">
                    <PromptTextArea
                        value={prompt}
                        onChange={setPrompt}
                        rows={5}
                        placeholder="描述您想生成的视频场景、镜头、动作..."
                    />
                </ToolSection>

                <ToolSection title="生成模式">
                    <ChipGroup value={videoMode} options={VIDEO_MODE_OPTIONS} onChange={(value) => setVideoMode(value)} />
                    <div className="mt-3 flex flex-col gap-3">
                        {videoMode === 'reference-guided' && (
                            <ToolField label={`参考图（${references.length}/${MAX_VIDEO_REFERENCES}）`}>
                                <div className="flex flex-wrap items-center gap-2">
                                    {references.map((item, index) => (
                                        <div key={`${item.name}-${index}`} className="group relative h-16 w-16 overflow-hidden rounded-[12px] border border-border">
                                            <img src={item.dataUrl} alt={item.name} className="h-full w-full object-cover" />
                                            <button
                                                type="button"
                                                onClick={() => setReferences((prev) => prev.filter((_, i) => i !== index))}
                                                className="absolute right-1 top-1 hidden h-5 w-5 items-center justify-center rounded-full bg-black/70 text-white group-hover:flex"
                                                aria-label="移除参考图"
                                            >
                                                <X className="h-3 w-3" />
                                            </button>
                                        </div>
                                    ))}
                                    {references.length < MAX_VIDEO_REFERENCES && (
                                        <HiddenFileInput
                                            accept="image/*"
                                            multiple
                                            disabled={isReadingRefs}
                                            onFiles={appendReferenceFiles}
                                        />
                                    )}
                                </div>
                            </ToolField>
                        )}
                        {videoMode === 'first-last-frame' && (
                            <>
                                <ReferenceSlot
                                    label="首帧"
                                    hint="上传视频起始画面"
                                    accept="image/*"
                                    item={firstFrame}
                                    onClear={() => setFirstFrame(null)}
                                    onFiles={async (files) => setFirstFrame(await pickSingleFile(files, 'image/*'))}
                                />
                                <ReferenceSlot
                                    label="尾帧"
                                    hint="上传视频结束画面"
                                    accept="image/*"
                                    item={lastFrame}
                                    onClear={() => setLastFrame(null)}
                                    onFiles={async (files) => setLastFrame(await pickSingleFile(files, 'image/*'))}
                                />
                            </>
                        )}
                        {videoMode === 'continuation' && (
                            <>
                                <ReferenceSlot
                                    label="起始视频"
                                    hint="上传 mp4 / mov / webm"
                                    accept={VIDEO_ACCEPT}
                                    item={firstClip}
                                    onClear={() => setFirstClip(null)}
                                    onFiles={async (files) => setFirstClip(await pickSingleFile(files, VIDEO_ACCEPT))}
                                />
                                <ReferenceSlot
                                    label="驱动音频（可选）"
                                    hint="上传 mp3 / wav 等音频"
                                    accept="audio/*"
                                    item={drivingAudio}
                                    onClear={() => setDrivingAudio(null)}
                                    onFiles={async (files) => setDrivingAudio(await pickSingleFile(files, 'audio/*'))}
                                />
                            </>
                        )}
                    </div>
                </ToolSection>

                <ToolSection title="参数配置">
                    <div className="flex flex-col gap-3">
                        <ToolField label="画面比例">
                            <ChipGroup value={aspectRatio} options={VIDEO_ASPECT_RATIO_OPTIONS} onChange={(value) => setAspectRatio(value)} />
                        </ToolField>
                        <div className="grid grid-cols-2 gap-3">
                            <ToolField label="分辨率">
                                <PopoverSelect
                                    value={resolution}
                                    onChange={(value) => setResolution(value === '1080p' ? '1080p' : '720p')}
                                    options={VIDEO_RESOLUTION_OPTIONS}
                                    className="w-full"
                                    panelClassName="w-[200px]"
                                />
                            </ToolField>
                            <ToolField label="时长">
                                <PopoverSelect
                                    value={String(durationSeconds)}
                                    onChange={(value) => setDurationSeconds(Number(value) || 8)}
                                    options={VIDEO_DURATION_OPTIONS}
                                    className="w-full"
                                    panelClassName="w-[200px]"
                                />
                            </ToolField>
                        </div>
                        <ToolField label="AI 配音">
                            <ChipGroup
                                value={generateAudio ? 'on' : 'off'}
                                options={[
                                    { value: 'off', label: '音频关' },
                                    { value: 'on', label: '音频开' },
                                ]}
                                onChange={(next) => setGenerateAudio(next === 'on')}
                            />
                        </ToolField>
                    </div>
                </ToolSection>

                <div ref={errorRef}>
                    <ErrorBanner message={error} onClose={() => setError('')} />
                </div>
                <ConfigHint message={configHint} />
                <RunButton
                    onClick={() => void handleGenerate()}
                    disabled={!hasVideoConfig || isSubmitting}
                    pending={isSubmitting}
                    label="生成视频"
                />
            </aside>

            <main className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto">
                {sortedJobs.length === 0 ? (
                    <div className="flex flex-1 items-center justify-center rounded-[20px] border border-dashed border-border bg-surface-primary/50">
                        <EmptyState
                            title="还没有生成记录"
                            description="在左侧配置参数并点击「生成视频」，生成的视频会显示在这里。"
                        />
                    </div>
                ) : (
                    sortedJobs.map((job) => {
                        const assets = assetsFromJobProjection(job);
                        const jobError = errorMessageFromJobProjection(job);
                        const isRunning = !['completed', 'failed', 'cancelled', 'dead_lettered'].includes(String(job.status));
                        return (
                            <JobCardShell
                                key={job.jobId}
                                job={job}
                                onCancel={() => void cancelModuleJob(job.jobId)}
                            >
                                {isRunning && (
                                    <div className="flex items-center gap-2 rounded-[12px] bg-surface-tertiary px-3 py-3 text-[12px] text-text-secondary">
                                        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-brand-red" />
                                        视频生成耗时较长，完成后会自动显示在这里...
                                    </div>
                                )}
                                {!isRunning && jobError && String(job.status) !== 'completed' && (
                                    <div className="text-[12px] text-brand-red">{jobError}</div>
                                )}
                                {assets.map((asset) => {
                                    const url = resolveAssetUrl(asset.previewUrl || asset.relativePath || '');
                                    if (!url) return null;
                                    return (
                                        <div key={asset.id} className="overflow-hidden rounded-[14px] border border-border bg-surface-primary">
                                            <video
                                                src={url}
                                                controls
                                                preload="metadata"
                                                className="max-h-[420px] w-full bg-black object-contain"
                                            />
                                            <div className="flex items-center justify-between gap-2 px-3 py-2">
                                                <span className="min-w-0 flex-1 truncate text-[11px] text-text-tertiary">
                                                    {asset.title || '生成视频'}
                                                </span>
                                                <DownloadButton asset={asset} compact />
                                            </div>
                                        </div>
                                    );
                                })}
                            </JobCardShell>
                        );
                    })
                )}
            </main>
        </div>
    );
}
