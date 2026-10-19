import { useCallback, useEffect, useState } from 'react';
import clsx from 'clsx';
import { AudioLines, Clapperboard, FolderOpen, ImagePlus, Palette, Wrench } from 'lucide-react';
import type { GenerationIntent } from '../../features/app-shell/types';
import { CoverModule } from './CoverModule';
import { ImageModule } from './ImageModule';
import { VideoModule } from './VideoModule';
import { AudioModule } from './AudioModule';
import type { ToolboxModulePreset } from './shared';

interface ToolboxProps {
    isActive?: boolean;
    pendingIntent?: GenerationIntent | null;
    onIntentConsumed?: () => void;
    onExecutionStateChange?: (active: boolean) => void;
    onOpenAssets?: () => void;
}

type ToolboxTab = 'image' | 'cover' | 'video' | 'audio';

const TAB_ITEMS: Array<{ key: ToolboxTab; label: string; icon: typeof ImagePlus; hint: string }> = [
    { key: 'image', label: '生图', icon: ImagePlus, hint: 'AI 图像生成' },
    { key: 'cover', label: '做封面', icon: Palette, hint: '封面设计制作' },
    { key: 'video', label: '生视频', icon: Clapperboard, hint: 'AI 视频生成' },
    { key: 'audio', label: '生音频', icon: AudioLines, hint: '语音合成与音频' },
];

function intentTab(mode: GenerationIntent['mode']): ToolboxTab {
    if (mode === 'cover') return 'cover';
    if (mode === 'audio') return 'audio';
    // video 与 digital-human 都进入生视频模块
    return 'video';
}

export function Toolbox({
    isActive = false,
    pendingIntent = null,
    onIntentConsumed,
    onExecutionStateChange,
    onOpenAssets,
}: ToolboxProps) {
    const [activeTab, setActiveTab] = useState<ToolboxTab>('image');
    const [preset, setPreset] = useState<ToolboxModulePreset | null>(null);
    const [presetNonce, setPresetNonce] = useState(0);
    const [moduleExecution, setModuleExecution] = useState<Record<ToolboxTab, boolean>>({
        image: false,
        cover: false,
        video: false,
        audio: false,
    });

    useEffect(() => {
        if (!pendingIntent) return;
        setActiveTab(intentTab(pendingIntent.mode));
        const nextPreset = pendingIntent.preset;
        if (nextPreset && (nextPreset.aspectRatio || nextPreset.resolution || nextPreset.durationSeconds)) {
            setPreset({
                aspectRatio: nextPreset.aspectRatio,
                resolution: nextPreset.resolution,
                durationSeconds: nextPreset.durationSeconds,
            });
            setPresetNonce((prev) => prev + 1);
        }
        onIntentConsumed?.();
    }, [pendingIntent, onIntentConsumed]);

    const handleModuleExecutionChange = useCallback((tab: ToolboxTab) => {
        return (active: boolean) => {
            setModuleExecution((prev) => (prev[tab] === active ? prev : { ...prev, [tab]: active }));
        };
    }, []);

    useEffect(() => {
        const anyActive = Object.values(moduleExecution).some(Boolean);
        onExecutionStateChange?.(anyActive);
    }, [moduleExecution, onExecutionStateChange]);

    const activeMeta = TAB_ITEMS.find((item) => item.key === activeTab);

    return (
        <div className="flex h-full min-h-0 flex-col">
            <header className="flex items-center gap-3 border-b border-border px-4 py-2.5">
                <div className="flex items-center gap-2 pr-1 text-text-primary">
                    <Wrench className="h-4 w-4 text-accent-primary" />
                    <span className="text-[14px] font-semibold">工具箱</span>
                </div>
                <nav className="flex items-center gap-1.5" aria-label="工具箱模块切换">
                    {TAB_ITEMS.map((item) => {
                        const active = item.key === activeTab;
                        const Icon = item.icon;
                        return (
                            <button
                                key={item.key}
                                type="button"
                                onClick={() => setActiveTab(item.key)}
                                title={item.hint}
                                aria-current={active ? 'page' : undefined}
                                className={clsx(
                                    'inline-flex h-9 items-center gap-1.5 rounded-full border px-3.5 text-[13px] font-medium transition-colors',
                                    active
                                        ? 'border-brand-red/40 bg-brand-red/10 text-brand-red'
                                        : 'border-transparent text-text-secondary hover:bg-surface-tertiary hover:text-text-primary',
                                )}
                            >
                                <Icon className="h-4 w-4" />
                                {item.label}
                            </button>
                        );
                    })}
                </nav>
                <div className="ml-auto flex items-center gap-2">
                    {activeMeta && <span className="text-[12px] text-text-tertiary">{activeMeta.hint}</span>}
                    {onOpenAssets && (
                        <button
                            type="button"
                            onClick={onOpenAssets}
                            className="inline-flex h-9 items-center gap-1.5 rounded-full border border-border bg-surface-primary px-3 text-[12px] font-medium text-text-secondary transition-colors hover:border-border/70 hover:text-text-primary"
                        >
                            <FolderOpen className="h-3.5 w-3.5" />
                            资产库
                        </button>
                    )}
                </div>
            </header>

            <div className="min-h-0 flex-1">
                <div className={activeTab === 'image' ? 'h-full min-h-0' : 'hidden'}>
                    <ImageModule
                        isActive={isActive && activeTab === 'image'}
                        onExecutionStateChange={handleModuleExecutionChange('image')}
                        preset={preset}
                        presetNonce={presetNonce}
                    />
                </div>
                <div className={activeTab === 'cover' ? 'h-full min-h-0' : 'hidden'}>
                    <CoverModule
                        isActive={isActive && activeTab === 'cover'}
                        onExecutionStateChange={handleModuleExecutionChange('cover')}
                    />
                </div>
                <div className={activeTab === 'video' ? 'h-full min-h-0' : 'hidden'}>
                    <VideoModule
                        isActive={isActive && activeTab === 'video'}
                        onExecutionStateChange={handleModuleExecutionChange('video')}
                        preset={preset}
                        presetNonce={presetNonce}
                    />
                </div>
                <div className={activeTab === 'audio' ? 'h-full min-h-0' : 'hidden'}>
                    <AudioModule
                        isActive={isActive && activeTab === 'audio'}
                        onExecutionStateChange={handleModuleExecutionChange('audio')}
                    />
                </div>
            </div>
        </div>
    );
}
