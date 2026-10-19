import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import clsx from 'clsx';
import { AudioLines, RefreshCw, Volume2 } from 'lucide-react';
import {
    buildAudioGenerationRequest,
    buildAudioLanguageOptions,
    buildAudioModelOptions,
    buildAudioVoiceOptions,
    DEFAULT_AUDIO_TTS_MODEL,
    errorMessageFromJobProjection,
    generationSubmitSource,
    assetsFromJobProjection,
    normalizeAudioTtsModel,
    normalizeVoiceList,
    resolveSelectedModelOverride,
    submitAudioGeneration,
    SUPPORTED_AUDIO_TTS_MODELS,
    validateAudioGenerationRequest,
    voiceMatchesAudioModel,
    voiceLanguageMatches,
    type PickerOption,
    type SettingsShape,
    type VoiceListItem,
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
    RemoveJobButton,
    ChipGroup,
    type ToolboxModuleProps,
} from './shared';
import {
    AUDIO_EMOTION_OPTIONS,
    AUDIO_PAUSE_OPTIONS,
    AUDIO_SPEED_OPTIONS,
    audioPauseToken,
    makeId,
} from './options';

export function AudioModule({ isActive, onExecutionStateChange }: ToolboxModuleProps) {
    const [settings, setSettings] = useState<SettingsShape>({});
    const [text, setText] = useState('');
    const [audioModel, setAudioModel] = useState('');
    const [voiceId, setVoiceId] = useState('');
    const [voices, setVoices] = useState<VoiceListItem[]>([]);
    const [isLoadingVoices, setIsLoadingVoices] = useState(false);
    const [languageBoost, setLanguageBoost] = useState('Chinese');
    const [speed, setSpeed] = useState('1');
    const [emotion, setEmotion] = useState('');
    const [error, setError] = useState('');
    const [isSubmitting, setIsSubmitting] = useState(false);
    const [voicesError, setVoicesError] = useState('');
    const [voiceListNonce, setVoiceListNonce] = useState(0);
    const textAreaRef = useRef<HTMLTextAreaElement | null>(null);
    const errorRef = useRef<HTMLDivElement | null>(null);

    const { sortedJobs, activeJobs, registerSubmittedJob } = useModuleJobs('audio', isActive);

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
                console.error('Failed to load settings for audio module:', loadError);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, []);

    const audioModelOptions = useMemo<PickerOption[]>(() => buildAudioModelOptions(settings), [settings]);
    const effectiveAudioModel = normalizeAudioTtsModel(
        audioModel || String(settings.voice_tts_model || settings.tts_model || DEFAULT_AUDIO_TTS_MODEL),
    );

    useEffect(() => {
        setAudioModel((prev) => {
            const normalized = prev.trim();
            if (normalized && audioModelOptions.some((option) => option.value === normalized)) return prev;
            return audioModelOptions[0]?.value || '';
        });
    }, [audioModelOptions]);

    const hasVoiceConfig = Boolean((settings.voice_endpoint || settings.tts_endpoint || settings.api_endpoint || '').trim())
        && Boolean((settings.voice_api_key || settings.tts_api_key || settings.api_key || '').trim());

    const voicesForModel = useMemo(
        () => voices.filter((voice) => voiceMatchesAudioModel(voice, effectiveAudioModel)),
        [voices, effectiveAudioModel],
    );
    const selectedVoice = useMemo(
        () => voicesForModel.find((voice) => voice.id === voiceId.trim()) || null,
        [voiceId, voicesForModel],
    );

    useEffect(() => {
        onExecutionStateChange?.(activeJobs.length > 0);
    }, [activeJobs.length, onExecutionStateChange]);

    useEffect(() => {
        if (!isActive || !hasVoiceConfig) {
            setVoices([]);
            setVoicesError('');
            return;
        }

        let cancelled = false;
        setIsLoadingVoices(true);
        setVoicesError('');
        void (async () => {
            try {
                const result = await window.ipcRenderer.voice.list({ model: effectiveAudioModel }) as unknown;
                if (cancelled) return;
                const nextVoices = normalizeVoiceList(result);
                setVoices(nextVoices);
                setVoiceId((prev) => {
                    const normalized = prev.trim();
                    if (normalized && nextVoices.some((voice) => (
                        voice.id === normalized && voiceMatchesAudioModel(voice, effectiveAudioModel)
                    ))) {
                        return prev;
                    }
                    return nextVoices.find((voice) => voiceMatchesAudioModel(voice, effectiveAudioModel))?.id || '';
                });
            } catch (listError) {
                if (!cancelled) {
                    console.error('Failed to load audio voices:', listError);
                    setVoices([]);
                    setVoicesError('音色列表加载失败，请检查网络后重试');
                }
            } finally {
                if (!cancelled) setIsLoadingVoices(false);
            }
        })();

        return () => {
            cancelled = true;
        };
    }, [effectiveAudioModel, hasVoiceConfig, isActive, voiceListNonce]);

    useEffect(() => {
        setVoiceId((prev) => {
            const current = prev.trim();
            if (current && voicesForModel.some((voice) => (
                voice.id === current && voiceLanguageMatches(voice, languageBoost)
            ))) {
                return prev;
            }
            return voicesForModel.find((voice) => voiceLanguageMatches(voice, languageBoost))?.id || '';
        });
    }, [languageBoost, voicesForModel]);

    const languageOptions = useMemo<PickerOption[]>(() => buildAudioLanguageOptions(voicesForModel), [voicesForModel]);
    const voiceOptions = useMemo<PickerOption[]>(() => buildAudioVoiceOptions(voicesForModel, languageBoost), [languageBoost, voicesForModel]);
    const mergedVoiceOptions = useMemo<PickerOption[]>(() => {
        const normalized = voiceId.trim();
        if (!normalized || voiceOptions.some((option) => option.value === normalized)) return voiceOptions;
        if (!voicesForModel.some((voice) => voice.id === normalized)) return voiceOptions;
        return [
            { value: normalized, label: normalized, description: '当前音色' },
            ...voiceOptions,
        ];
    }, [voiceId, voiceOptions, voicesForModel]);

    const insertPause = useCallback((seconds: string) => {
        const token = audioPauseToken(seconds);
        const element = textAreaRef.current;
        if (!element) {
            setText((prev) => `${prev}${token}`);
            return;
        }
        const start = element.selectionStart ?? text.length;
        const end = element.selectionEnd ?? text.length;
        setText((prev) => `${prev.slice(0, start)}${token}${prev.slice(end)}`);
        window.requestAnimationFrame(() => {
            element.focus();
            const cursor = start + token.length;
            element.setSelectionRange(cursor, cursor);
        });
    }, [text.length]);

    const handleGenerate = useCallback(async () => {
        if (isSubmitting) return;
        const request = buildAudioGenerationRequest({
            prompt: text,
            title: '',
            projectId: '',
            model: effectiveAudioModel,
            voiceId,
            voiceTargetTtsModel: selectedVoice?.targetTtsModel || effectiveAudioModel,
            languageBoost,
            speed,
            emotion,
            responseFormat: 'mp3',
        });
        const validationError = validateAudioGenerationRequest(request, {
            hasVoiceConfig,
            audioVoiceIdsForModel: voicesForModel.map((voice) => voice.id),
        });
        if (validationError) {
            setError(validationError);
            return;
        }

        setError('');
        setIsSubmitting(true);
        try {
            const routeOverride = resolveSelectedModelOverride(settings, 'voiceTts', 'tts', request.model);
            const result = await submitAudioGeneration(window.ipcRenderer.generation, request, {
                clientRequestId: makeId('toolbox-audio'),
                source: generationSubmitSource('standalone'),
                queueMode: 'free_creation',
                routeOverride,
            });
            registerSubmittedJob(result.jobId);
            // 保留文本，便于微调后再次合成
        } catch (submitError) {
            setError(submitError instanceof Error ? submitError.message : '生音频提交失败');
        } finally {
            setIsSubmitting(false);
        }
    }, [
        effectiveAudioModel,
        emotion,
        hasVoiceConfig,
        isSubmitting,
        languageBoost,
        registerSubmittedJob,
        selectedVoice?.targetTtsModel,
        settings,
        speed,
        text,
        voiceId,
        voicesForModel,
    ]);

    const configHint = !hasVoiceConfig
        ? '尚未配置语音合成服务：请前往「设置 → AI 源」完成接口地址与密钥配置后使用'
        : '';

    return (
        <div className="flex h-full min-h-0 gap-4 p-4">
            <aside className="flex w-[340px] shrink-0 flex-col gap-3 overflow-y-auto pr-1">
                <ToolSection title="合成平台">
                    <ChipGroup
                        value={effectiveAudioModel}
                        options={SUPPORTED_AUDIO_TTS_MODELS.map((option) => ({ value: option.value, label: option.label }))}
                        onChange={setAudioModel}
                    />
                </ToolSection>

                <ToolSection title="朗读文本">
                    <PromptTextArea
                        value={text}
                        onChange={setText}
                        rows={7}
                        textRef={textAreaRef}
                        placeholder={'输入要合成的文本...\n支持插入停顿标记，例如：〔停顿1秒〕'}
                    />
                    {text.trim().length > 0 && (
                        <div className="mt-1.5 text-right text-[11px] text-text-tertiary">
                            {text.trim().length} 字
                        </div>
                    )}
                    <div className="mt-3">
                        <ToolField label="插入停顿">
                            <div className="flex flex-wrap items-center gap-1.5">
                                {AUDIO_PAUSE_OPTIONS.map((option) => (
                                    <button
                                        key={option.value}
                                        type="button"
                                        onClick={() => insertPause(option.value)}
                                        className="inline-flex h-7 items-center rounded-full border border-border bg-surface-primary px-2.5 text-[11px] font-medium text-text-secondary transition-colors hover:border-brand-red/40 hover:text-brand-red"
                                    >
                                        {option.label}
                                    </button>
                                ))}
                            </div>
                        </ToolField>
                    </div>
                </ToolSection>

                <ToolSection title="音色与语调">
                    <div className="flex flex-col gap-3">
                        <ToolField label="音色">
                            <PopoverSelect
                                value={voiceId}
                                onChange={setVoiceId}
                                options={mergedVoiceOptions}
                                layout="column"
                                panelClassName="w-[280px]"
                                className="w-full"
                                disabled={isLoadingVoices || mergedVoiceOptions.length === 0}
                                emptyText={isLoadingVoices ? '音色加载中...' : '暂无可用音色'}
                            />
                            {voicesError && (
                                <div className="flex items-center gap-2 text-[11px] text-brand-red">
                                    <span className="min-w-0 flex-1">{voicesError}</span>
                                    <button
                                        type="button"
                                        onClick={() => setVoiceListNonce((nonce) => nonce + 1)}
                                        disabled={isLoadingVoices}
                                        className="inline-flex h-6 shrink-0 items-center gap-1 rounded-full border border-border bg-surface-primary px-2 text-[11px] font-medium text-text-secondary transition-colors hover:border-brand-red/40 hover:text-brand-red disabled:opacity-55"
                                    >
                                        <RefreshCw className={clsx('h-3 w-3', isLoadingVoices && 'animate-spin')} />
                                        重试
                                    </button>
                                </div>
                            )}
                            {!voicesError && !isLoadingVoices && hasVoiceConfig && voicesForModel.length === 0 && (
                                <div className="text-[11px] leading-4 text-text-tertiary">
                                    当前平台暂无可用音色：请确认已在「设置 → AI 源」配置对应平台的接口与密钥
                                </div>
                            )}
                        </ToolField>
                        <div className="grid grid-cols-2 gap-3">
                            <ToolField label="语言">
                                <PopoverSelect
                                    value={languageBoost}
                                    onChange={setLanguageBoost}
                                    options={languageOptions}
                                    className="w-full"
                                    panelClassName="w-[200px]"
                                />
                            </ToolField>
                            <ToolField label="语速">
                                <PopoverSelect
                                    value={speed}
                                    onChange={setSpeed}
                                    options={AUDIO_SPEED_OPTIONS}
                                    className="w-full"
                                    panelClassName="w-[200px]"
                                />
                            </ToolField>
                        </div>
                        <ToolField label="情感">
                            <PopoverSelect
                                value={emotion}
                                onChange={setEmotion}
                                options={AUDIO_EMOTION_OPTIONS}
                                className="w-full"
                                panelClassName="w-[200px]"
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
                    disabled={!hasVoiceConfig || isSubmitting}
                    pending={isSubmitting}
                    label="生成语音"
                />
            </aside>

            <main className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto">
                {sortedJobs.length === 0 ? (
                    <div className="flex flex-1 items-center justify-center rounded-[20px] border border-dashed border-border bg-surface-primary/50">
                        <EmptyState
                            title="还没有生成记录"
                            description="在左侧输入文本并点击「生成语音」，生成的音频会显示在这里。"
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
                                        正在合成语音，请稍候...
                                    </div>
                                )}
                                {!isRunning && jobError && String(job.status) !== 'completed' && (
                                    <div className="text-[12px] text-brand-red">{jobError}</div>
                                )}
                                {assets.map((asset) => {
                                    const url = resolveAssetUrl(asset.previewUrl || asset.relativePath || '');
                                    if (!url) return null;
                                    return (
                                        <div key={asset.id} className="flex items-center gap-3 rounded-[14px] border border-border bg-surface-primary px-3 py-2.5">
                                            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-brand-red/10 text-brand-red">
                                                <AudioLines className="h-4.5 w-4.5" />
                                            </div>
                                            <audio
                                                src={url}
                                                controls
                                                controlsList="nodownload"
                                                preload="metadata"
                                                className="h-9 min-w-0 flex-1"
                                            />
                                            <Volume2 className="h-4 w-4 shrink-0 text-text-tertiary" />
                                            <DownloadButton asset={asset} compact />
                                            <RemoveJobButton jobId={job.jobId} />
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
