import type { ReferenceItem } from '../../features/media-generation';

export const IMAGE_ASPECT_RATIO_OPTIONS = [
    { value: 'auto', label: 'Auto' },
    { value: '1:1', label: '1:1' },
    { value: '3:4', label: '3:4' },
    { value: '4:3', label: '4:3' },
    { value: '9:16', label: '9:16' },
    { value: '16:9', label: '16:9' },
    { value: 'custom', label: '自定义' },
] as const;

/** 自定义尺寸输入：宽高各 2-5 位数字，支持 x / X / × / * 分隔，统一归一为 `宽x高`。 */
export function normalizeCustomImageSize(raw: string): string | null {
    const match = raw.trim().match(/^(\d{2,5})\s*[xX×*]\s*(\d{2,5})$/);
    if (!match) return null;
    const width = Number(match[1]);
    const height = Number(match[2]);
    if (!Number.isFinite(width) || !Number.isFinite(height) || width < 16 || height < 16) return null;
    return `${width}x${height}`;
}

export const IMAGE_RESOLUTION_OPTIONS = [
    { value: '1K', label: '1K' },
    { value: '2K', label: '2K' },
    { value: '4K', label: '4K' },
] as const;

export const DEFAULT_IMAGE_QUALITY = 'medium';

export const IMAGE_COUNT_OPTIONS = [
    { value: '1', label: '1 张' },
    { value: '2', label: '2 张' },
    { value: '3', label: '3 张' },
    { value: '4', label: '4 张' },
] as const;

export const VIDEO_MODE_OPTIONS = [
    { value: 'text-to-video', label: '文生视频' },
    { value: 'reference-guided', label: '参考图视频' },
    { value: 'first-last-frame', label: '首尾帧视频' },
    { value: 'continuation', label: '视频续写' },
] as const;

export const VIDEO_ASPECT_RATIO_OPTIONS = [
    { value: '16:9', label: '16:9' },
    { value: '9:16', label: '9:16' },
] as const;

export const VIDEO_RESOLUTION_OPTIONS = [
    { value: '720p', label: '720p' },
    { value: '1080p', label: '1080p' },
] as const;

export const VIDEO_DURATION_OPTIONS = [
    { value: '5', label: '5 秒' },
    { value: '6', label: '6 秒' },
    { value: '7', label: '7 秒' },
    { value: '8', label: '8 秒' },
    { value: '9', label: '9 秒' },
    { value: '10', label: '10 秒' },
    { value: '11', label: '11 秒' },
    { value: '12', label: '12 秒' },
] as const;

export const VIDEO_AUDIO_OPTIONS = [
    { value: 'off', label: '音频关' },
    { value: 'on', label: '音频开' },
] as const;

export const AUDIO_SPEED_OPTIONS = [
    { value: '0.5', label: '0.5' },
    { value: '0.75', label: '0.75' },
    { value: '1', label: '1' },
    { value: '1.25', label: '1.25' },
    { value: '1.5', label: '1.5' },
    { value: '1.75', label: '1.75' },
    { value: '2', label: '2' },
] as const;

export const AUDIO_EMOTION_OPTIONS = [
    { value: '', label: '自然' },
    { value: 'calm', label: '平静' },
    { value: 'happy', label: '开心' },
    { value: 'sad', label: '悲伤' },
    { value: 'angry', label: '愤怒' },
    { value: 'surprised', label: '惊讶' },
    { value: 'whisper', label: '低语' },
    { value: 'fluent', label: '流畅' },
] as const;

export const AUDIO_PAUSE_OPTIONS = [
    { value: '0.3', label: '0.3s' },
    { value: '0.6', label: '0.6s' },
    { value: '1.0', label: '1.0s' },
    { value: '1.5', label: '1.5s' },
    { value: '2.0', label: '2.0s' },
] as const;

export const AUDIO_PAUSE_TOKEN_PATTERN = /〔停顿\s*([0-9.]+)\s*秒〕/g;

export function audioPauseToken(seconds: string): string {
    return `〔停顿${seconds}秒〕`;
}

export function makeId(prefix: string): string {
    return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

export const readFileAsDataUrl = (file: File | Blob): Promise<string> => new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(reader.error || new Error('读取文件失败'));
    reader.readAsDataURL(file);
});

export async function filesToReferenceItems(files: File[], maxCount: number): Promise<ReferenceItem[]> {
    return Promise.all(files.slice(0, maxCount).map(async (file) => ({
        name: file.name,
        dataUrl: await readFileAsDataUrl(file),
    })));
}

export function isVideoReference(item: ReferenceItem | null | undefined): boolean {
    return String(item?.dataUrl || '').startsWith('data:video/');
}

const ACCEPT_EXTENSION_GROUPS: Record<string, string[]> = {
    'image/*': ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'svg', 'avif', 'heic', 'heif', 'jfif'],
    'video/*': ['mp4', 'mov', 'webm', 'm4v', 'avi', 'mkv'],
    'audio/*': ['mp3', 'wav', 'm4a', 'aac', 'flac', 'ogg', 'opus', 'webm'],
};

export function fileMatchesAccept(file: File, accept: string): boolean {
    const tokens = String(accept || '')
        .split(',')
        .map((token) => token.trim().toLowerCase())
        .filter(Boolean);
    if (!tokens.length) return true;
    const mimeType = String(file.type || '').toLowerCase();
    const match = file.name.toLowerCase().match(/\.([a-z0-9]+)$/);
    const ext = match?.[1] || '';
    return tokens.some((token) => {
        if (token.startsWith('.')) {
            return ext === token.slice(1);
        }
        if (token.endsWith('/*')) {
            const prefix = token.slice(0, -1);
            return (mimeType && mimeType.startsWith(prefix)) || ACCEPT_EXTENSION_GROUPS[token]?.includes(ext);
        }
        return mimeType === token || ACCEPT_EXTENSION_GROUPS[token]?.includes(ext);
    });
}
