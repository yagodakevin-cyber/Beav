import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import clsx from 'clsx';
import { X } from 'lucide-react';
import {
    assetsFromJobProjection,
    buildImageGenerationRequest,
    buildImageModelOptions,
    errorMessageFromJobProjection,
    generationSubmitSource,
    resolveSelectedModelOverride,
    submitImageGeneration,
    validateImageGenerationRequest,
    type GeneratedAsset,
    type ImageGenerationMode,
    type PickerOption,
    type ReferenceItem,
    type SettingsShape,
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
    TextInput,
    ToolField,
    ToolSection,
    DownloadButton,
    ChipGroup,
    HiddenFileInput,
    type ToolboxModuleProps,
} from './shared';
import {
    IMAGE_ASPECT_RATIO_OPTIONS,
    IMAGE_COUNT_OPTIONS,
    IMAGE_RESOLUTION_OPTIONS,
    DEFAULT_IMAGE_QUALITY,
    fileMatchesAccept,
    filesToReferenceItems,
    makeId,
    normalizeCustomImageSize,
} from './options';

const IMAGE_MODE_OPTIONS = [
    { value: 'text-to-image', label: '文生图' },
    { value: 'reference-guided', label: '参考图生成' },
    { value: 'image-to-image', label: '图生图' },
] as const satisfies ReadonlyArray<{ value: ImageGenerationMode; label: string }>;

const MAX_IMAGE_REFERENCES = 4;

export function ImageModule({ isActive, onExecutionStateChange, preset, presetNonce = 0 }: ToolboxModuleProps) {
    const [settings, setSettings] = useState<SettingsShape>({});
    const [prompt, setPrompt] = useState('');
    const [imageMode, setImageMode] = useState<ImageGenerationMode>('text-to-image');
    const [references, setReferences] = useState<ReferenceItem[]>([]);
    const [isReadingRefs, setIsReadingRefs] = useState(false);
    const [model, setModel] = useState('');
    const [aspectRatio, setAspectRatio] = useState('4:3');
    const [size, setSize] = useState('');
    const [resolution, setResolution] = useState('1K');
    const [count, setCount] = useState(1);
    const [error, setError] = useState('');
    const [isSubmitting, setIsSubmitting] = useState(false);
    const [previewAsset, setPreviewAsset] = useState<GeneratedAsset | null>(null);
    const errorRef = useRef<HTMLDivElement | null>(null);

    const { sortedJobs, activeJobs, registerSubmittedJob } = useModuleJobs('image', isActive);

    useEffect(() => {
        if (!error) return;
        errorRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    }, [error]);

    useEffect(() => {
        if (!previewAsset) return;
        const handleKeyDown = (event: KeyboardEvent) => {
            if (event.key === 'Escape') setPreviewAsset(null);
        };
        document.addEventListener('keydown', handleKeyDown);
        return () => document.removeEventListener('keydown', handleKeyDown);
    }, [previewAsset]);

    useEffect(() => {
        let cancelled = false;
        void (async () => {
            try {
                const next = await window.ipcRenderer.getSettings() as SettingsShape;
                if (!cancelled) setSettings((next || {}) as SettingsShape);
            } catch (loadError) {
                console.error('Failed to load settings for image module:', loadError);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, []);

    const modelOptions = useMemo<PickerOption[]>(() => buildImageModelOptions(settings), [settings]);

    useEffect(() => {
        setModel((prev) => {
            const normalized = prev.trim();
            if (normalized && modelOptions.some((option) => option.value === normalized)) return prev;
            const fromSettings = String(settings.image_model || '').trim();
            if (fromSettings && modelOptions.some((option) => option.value === fromSettings)) return fromSettings;
            return modelOptions[0]?.value || fromSettings;
        });
    }, [modelOptions, settings.image_model]);

    useEffect(() => {
        onExecutionStateChange?.(activeJobs.length > 0);
    }, [activeJobs.length, onExecutionStateChange]);

    /** 比例与自定义尺寸互斥：选中具体比例时清空自定义尺寸，避免两个参数同时生效。 */
    const applyAspectRatio = useCallback((value: string) => {
        setAspectRatio(value);
        if (value !== 'custom') setSize('');
    }, []);

    useEffect(() => {
        if (!preset) return;
        if (preset.aspectRatio) applyAspectRatio(preset.aspectRatio);
        if (preset.resolution) setResolution(preset.resolution);
    }, [preset, presetNonce, applyAspectRatio]);

    const hasImageConfig = Boolean((settings.image_endpoint || settings.api_endpoint || '').trim())
        && Boolean((settings.image_api_key || settings.api_key || '').trim());

    const appendReferenceFiles = useCallback(async (files: File[]) => {
        const accepted = files.filter((file) => fileMatchesAccept(file, 'image/*'));
        if (!accepted.length) {
            setError('仅支持上传图片文件（png / jpg / webp 等）');
            return;
        }
        setIsReadingRefs(true);
        try {
            const nextItems = await filesToReferenceItems(accepted, MAX_IMAGE_REFERENCES);
            setReferences((prev) => [...prev, ...nextItems].slice(0, MAX_IMAGE_REFERENCES));
            // 与实际请求行为保持一致：上传参考图后自动进入参考图生成模式
            setImageMode((prev) => (prev === 'text-to-image' ? 'reference-guided' : prev));
        } catch (readError) {
            console.error('Failed to read image references:', readError);
            setError('参考图读取失败，请重试');
        } finally {
            setIsReadingRefs(false);
        }
    }, []);

    const handleGenerate = useCallback(async () => {
        if (isSubmitting) return;
        const isCustomSize = aspectRatio === 'custom';
        const normalizedSize = isCustomSize ? normalizeCustomImageSize(size) : null;
        if (isCustomSize && !normalizedSize) {
            setError('自定义尺寸格式不正确，请按「宽x高」填写（如 2048x2048），最短边不低于 16px');
            return;
        }
        const request = buildImageGenerationRequest({
            prompt,
            title: '',
            projectId: '',
            count,
            model,
            aspectRatio: isCustomSize ? 'auto' : aspectRatio,
            size: normalizedSize || '',
            quality: DEFAULT_IMAGE_QUALITY,
            resolution,
            imageMode,
            referenceItems: references,
        });
        const validationError = validateImageGenerationRequest(request, { hasImageConfig });
        if (validationError) {
            setError(validationError);
            return;
        }

        setError('');
        setIsSubmitting(true);
        try {
            const routeOverride = resolveSelectedModelOverride(settings, 'image', 'image', request.model);
            const result = await submitImageGeneration(window.ipcRenderer.generation, request, {
                clientRequestId: makeId('toolbox-image'),
                source: generationSubmitSource('standalone'),
                queueMode: 'free_creation',
                routeOverride,
                provider: settings.image_provider,
                providerTemplate: settings.image_provider_template,
            });
            registerSubmittedJob(result.jobId);
            // 保留输入内容，便于基于当前提示词/参数微调后再次生成
        } catch (submitError) {
            setError(submitError instanceof Error ? submitError.message : '生图提交失败');
        } finally {
            setIsSubmitting(false);
        }
    }, [
        aspectRatio,
        count,
        hasImageConfig,
        imageMode,
        isSubmitting,
        model,
        prompt,
        references,
        registerSubmittedJob,
        resolution,
        settings,
        size,
    ]);

    const configHint = !hasImageConfig
        ? '尚未配置生图服务：请前往「设置 → AI 源」添加具备生图能力的模型后使用'
        : !model.trim() && modelOptions.length === 0
            ? '尚未添加生图模型：请前往「设置 → AI 源」添加具备生图能力的模型后使用'
            : !model.trim()
                ? '尚未选择生图模型，请先在上方「模型」中选择'
                : '';

    const previewUrl = useMemo(() => (
        previewAsset ? resolveAssetUrl(previewAsset.previewUrl || previewAsset.relativePath || '') : ''
    ), [previewAsset]);

    return (
        <div className="flex h-full min-h-0 gap-4 p-4">
            <aside className="flex w-[340px] shrink-0 flex-col gap-3 overflow-y-auto pr-1">
                <ToolSection title="提示词">
                    <PromptTextArea
                        value={prompt}
                        onChange={setPrompt}
                        rows={6}
                        placeholder="描述您想生成的场景、风格、细节..."
                    />
                </ToolSection>

                <ToolSection title="生成模式">
                    <ChipGroup
                        value={imageMode}
                        options={IMAGE_MODE_OPTIONS}
                        onChange={(value) => {
                            setImageMode(value);
                            // 文生图不使用参考图；清空以避免下次提交被自动升级为参考图模式
                            if (value === 'text-to-image') setReferences([]);
                        }}
                    />
                    <div className="mt-3">
                        <ToolField label={`参考图（${references.length}/${MAX_IMAGE_REFERENCES}）`}>
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
                                {references.length < MAX_IMAGE_REFERENCES && (
                                    <HiddenFileInput
                                        accept="image/*"
                                        multiple
                                        disabled={isReadingRefs}
                                        onFiles={appendReferenceFiles}
                                    />
                                )}
                            </div>
                        </ToolField>
                    </div>
                </ToolSection>

                <ToolSection title="参数配置">
                    <div className="flex flex-col gap-3">
                        <ToolField label="模型">
                            <PopoverSelect
                                value={model}
                                onChange={setModel}
                                options={modelOptions}
                                layout="column"
                                panelClassName="w-[260px]"
                                className="w-full"
                                disabled={modelOptions.length === 0}
                                emptyText="未添加生图模型"
                            />
                        </ToolField>
                        <ToolField label="画面比例">
                            <ChipGroup value={aspectRatio} options={IMAGE_ASPECT_RATIO_OPTIONS} onChange={applyAspectRatio} />
                        </ToolField>
                        {aspectRatio === 'custom' && (
                            <ToolField label="自定义尺寸">
                                <TextInput value={size} onChange={setSize} placeholder="宽x高，如 2048x2048" />
                                <span className="text-[11px] leading-4 text-text-tertiary">
                                    按「宽x高」填写精确像素尺寸，最短边不低于 16px；选择其他比例时自动清除
                                </span>
                            </ToolField>
                        )}
                        <div className="grid grid-cols-2 gap-3">
                            <ToolField label="清晰度">
                                <PopoverSelect
                                    value={resolution}
                                    onChange={setResolution}
                                    options={IMAGE_RESOLUTION_OPTIONS}
                                    className="w-full"
                                    panelClassName="w-[200px]"
                                />
                            </ToolField>
                            <ToolField label="生成数量">
                                <PopoverSelect
                                    value={String(count)}
                                    onChange={(value) => setCount(Number(value) || 1)}
                                    options={IMAGE_COUNT_OPTIONS}
                                    className="w-full"
                                    panelClassName="w-[200px]"
                                />
                            </ToolField>
                        </div>
                    </div>
                </ToolSection>

                <div ref={errorRef}>
                    <ErrorBanner message={error} onClose={() => setError('')} />
                </div>
                <ConfigHint message={configHint} />
                <RunButton
                    onClick={() => void handleGenerate()}
                    disabled={!hasImageConfig || !model.trim() || isSubmitting}
                    pending={isSubmitting}
                    label="生成图片"
                />
            </aside>

            <main className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto">
                {sortedJobs.length === 0 ? (
                    <div className="flex flex-1 items-center justify-center rounded-[20px] border border-dashed border-border bg-surface-primary/50">
                        <EmptyState
                            title="还没有生成记录"
                            description="在左侧配置参数并点击「生成图片」，生成的图片会显示在这里。"
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
                                        正在生成图片，请稍候...
                                    </div>
                                )}
                                {!isRunning && jobError && String(job.status) !== 'completed' && (
                                    <div className="text-[12px] text-brand-red">{jobError}</div>
                                )}
                                {assets.length > 0 && (
                                    <div className={clsx(
                                        'grid gap-2',
                                        assets.length === 1 ? 'grid-cols-1' : 'grid-cols-2',
                                    )}>
                                        {assets.map((asset) => {
                                            const url = resolveAssetUrl(asset.previewUrl || asset.relativePath || '');
                                            if (!url) return null;
                                            return (
                                                <div key={asset.id} className="group relative overflow-hidden rounded-[14px] border border-border bg-surface-tertiary">
                                                    <button
                                                        type="button"
                                                        onClick={() => setPreviewAsset(asset)}
                                                        className="block w-full cursor-zoom-in"
                                                        aria-label="预览大图"
                                                    >
                                                        <img
                                                            src={url}
                                                            alt={asset.title || '生成图片'}
                                                            className="h-auto max-h-[280px] w-full object-cover"
                                                            loading="lazy"
                                                        />
                                                    </button>
                                                    <div className="absolute bottom-2 right-2 opacity-0 transition-opacity group-hover:opacity-100">
                                                        <DownloadButton asset={asset} compact />
                                                    </div>
                                                </div>
                                            );
                                        })}
                                    </div>
                                )}
                            </JobCardShell>
                        );
                    })
                )}
            </main>

            {previewAsset && previewUrl && (
                <div
                    className="fixed inset-0 z-50 flex items-center justify-center bg-black/85 p-10"
                    onClick={() => setPreviewAsset(null)}
                >
                    <div className="relative max-h-full max-w-full" onClick={(event) => event.stopPropagation()}>
                        <img src={previewUrl} alt={previewAsset.title || '生成图片'} className="max-h-[80vh] max-w-[80vw] rounded-[16px] object-contain" />
                        <div className="mt-3 flex items-center justify-center gap-2">
                            <DownloadButton asset={previewAsset} />
                        </div>
                        <button
                            type="button"
                            onClick={() => setPreviewAsset(null)}
                            className="absolute -right-10 -top-10 flex h-9 w-9 items-center justify-center rounded-full bg-white/10 text-white transition-colors hover:bg-white/20"
                            aria-label="关闭预览"
                        >
                            <X className="h-4.5 w-4.5" />
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}
